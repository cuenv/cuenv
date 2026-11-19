use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use cuenv_ci::emitter::{EmitterError, EmitterResult};
use cuenv_ci::ir::Task as IrTask;
use cuenv_core::ci::PipelineMode;
use cuenv_core::manifest::{Entrypoint, Project, Runtime, Service};
use cuenv_core::tasks::{DependencyKind, Task};
use cuenv_task_exec::TaskCommandExt;

use super::expanded::render_env;
use super::{WorkspacePipeline, array, identifier, literal, unsupported, upper_initial};

#[derive(Clone, Copy)]
pub(super) struct ServiceRenderOptions<'a> {
    pub project_id: usize,
    pub expose: bool,
    pub pipelines: &'a [WorkspacePipeline<'a>],
    pub project_dir: &'a Path,
}

pub(super) fn append_project_services(
    out: &mut String,
    project: &Project,
    options: ServiceRenderOptions<'_>,
) -> EmitterResult<()> {
    let mut requested = BTreeSet::new();
    if options.expose {
        requested.extend(
            project
                .services
                .iter()
                .filter(|(_, service)| {
                    matches!(
                        service.runtime,
                        Some(Runtime::Container(_) | Runtime::Dagger(_))
                    )
                })
                .map(|(name, _)| name.as_str()),
        );
    }
    for pipeline in options.pipelines.iter().filter(|pipeline| {
        pipeline.ir.pipeline.mode == PipelineMode::Expanded
            && pipeline.project_dir == options.project_dir
            && pipeline
                .project
                .is_some_and(|owner| owner.name == project.name)
    }) {
        requested.extend(pipeline.ir.regular_tasks().flat_map(task_services));
    }
    let mut visiting = BTreeSet::new();
    let mut rendered = BTreeSet::new();
    for service in requested {
        append_service(
            out,
            service,
            &mut ServiceRender {
                project,
                options: &options,
                visiting: &mut visiting,
                rendered: &mut rendered,
            },
        )?;
    }
    Ok(())
}

pub(super) fn task_services(task: &IrTask) -> impl Iterator<Item = &str> {
    task.provider_hints
        .as_ref()
        .and_then(|hints| hints.get("dagger"))
        .and_then(|dagger| dagger.get("services"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
}

pub(super) fn service_name(
    project: &Project,
    project_id: usize,
    service: &str,
) -> EmitterResult<String> {
    let keys = sorted_names(project);
    let index = keys.binary_search(&service).map_err(|_| {
        EmitterError::InvalidIR(format!(
            "unknown Dagger service '{service}' in project '{}'",
            project.name
        ))
    })?;
    Ok(format!(
        "{}Project{project_id}Service{index}{}",
        identifier(&project.name),
        upper_initial(&identifier(service))
    ))
}

fn sorted_names(project: &Project) -> Vec<&str> {
    let mut names: Vec<_> = project.services.keys().map(String::as_str).collect();
    names.sort_unstable();
    names
}

struct ServiceRender<'a, 'b> {
    project: &'a Project,
    options: &'a ServiceRenderOptions<'a>,
    visiting: &'b mut BTreeSet<String>,
    rendered: &'b mut BTreeSet<String>,
}

fn append_service(
    out: &mut String,
    name: &str,
    state: &mut ServiceRender<'_, '_>,
) -> EmitterResult<()> {
    if state.rendered.contains(name) {
        return Ok(());
    }
    if !state.visiting.insert(name.to_string()) {
        return Err(EmitterError::InvalidIR(format!(
            "Dagger service dependency cycle at '{name}'"
        )));
    }
    let project = state.project;
    let service = project.services.get(name).ok_or_else(|| {
        EmitterError::InvalidIR(format!(
            "unknown Dagger service '{name}' in project '{}'",
            project.name
        ))
    })?;
    for dependency in &service.depends_on {
        if dependency.kind != DependencyKind::Service {
            return Err(unsupported(format!(
                "non-service dependency '{}' on Dagger service '{name}'",
                dependency.name
            )));
        }
        append_service(out, &dependency.name, state)?;
    }

    let function = service_name(project, state.options.project_id, name)?;
    if state.options.expose {
        let _ = writeln!(out, "\n  @up\n  pub {function}: Service! {{");
    } else {
        let _ = writeln!(out, "\n  {function}: Service! {{");
    }
    append_container(out, name, service)?;
    for dependency in &service.depends_on {
        let target = service_name(project, state.options.project_id, &dependency.name)?;
        let _ = writeln!(
            out,
            "      .withServiceBinding({}, {target})",
            literal(&dependency.name)
        );
    }
    let args = entrypoint(name, &service.entrypoint)?;
    if args.is_empty() {
        out.push_str("      .asService(useEntrypoint: true)\n");
    } else {
        let _ = writeln!(
            out,
            "      .asService(args: {})",
            array(args.iter().map(String::as_str))
        );
    }
    out.push_str("  }\n");
    state.visiting.remove(name);
    state.rendered.insert(name.to_string());
    Ok(())
}

fn append_container(out: &mut String, name: &str, service: &Service) -> EmitterResult<()> {
    let (image, runtime) = match service.runtime.as_ref() {
        Some(Runtime::Container(runtime)) => (runtime.image.as_str(), None),
        Some(Runtime::Dagger(runtime))
            if runtime.engine.is_none() && runtime.cache_volumes.is_empty() =>
        {
            let image = runtime.image.as_deref().ok_or_else(|| {
                unsupported(format!(
                    "Dockerfile-backed service '{name}'; specify a container image"
                ))
            })?;
            (image, Some(runtime.as_ref()))
        }
        _ => {
            return Err(unsupported(format!(
                "Dagger service '{name}' requires a container image with no engine override or cache mounts"
            )));
        }
    };
    let platform = runtime.and_then(|runtime| runtime.platform.as_deref());
    let container = platform.map_or("container".to_string(), |platform| {
        format!("container(platform: {})", literal(platform))
    });
    let _ = writeln!(out, "    {container}.from({})", literal(image));
    if let Some(directory) = service.dir.as_deref() {
        let _ = writeln!(out, "      .withWorkdir({})", literal(directory));
    }
    for (key, value) in service.env.iter().collect::<BTreeMap<_, _>>() {
        let _ = writeln!(
            out,
            "      .withEnvVariable({}, {})",
            literal(key),
            literal(&render_env(value, key)?)
        );
    }
    for port in &service.ports {
        let protocol = match port.protocol.as_str() {
            "tcp" => "TCP",
            "udp" => "UDP",
            _ => {
                return Err(EmitterError::InvalidIR(format!(
                    "unknown protocol '{}' on service '{name}'",
                    port.protocol
                )));
            }
        };
        let _ = writeln!(
            out,
            "      .withExposedPort({}, protocol: NetworkProtocol.{protocol})",
            port.port
        );
    }
    Ok(())
}

fn entrypoint(name: &str, entrypoint: &Entrypoint) -> EmitterResult<Vec<String>> {
    match entrypoint {
        Entrypoint::Command(command) => {
            if command.command.is_empty() {
                return Ok(Vec::new());
            }
            std::iter::once(Ok(command.command.clone()))
                .chain(command.args.iter().map(|arg| {
                    arg.as_str().map(str::to_owned).ok_or_else(|| {
                        unsupported(format!("non-literal argument on Dagger service '{name}'"))
                    })
                }))
                .collect()
        }
        Entrypoint::Script(script) => {
            let task = Task {
                script: Some(script.script.clone()),
                script_shell: script.script_shell,
                shell_options: script.shell_options,
                ..Task::default()
            };
            command_spec(&task)
        }
        Entrypoint::Task(task)
            if task.env.is_empty() && task.inputs.is_empty() && task.depends_on.is_empty() =>
        {
            command_spec(task)
        }
        Entrypoint::Task(_) => Err(unsupported(format!(
            "task-specific environment, inputs or dependencies on Dagger service '{name}'"
        ))),
    }
}

fn command_spec(task: &Task) -> EmitterResult<Vec<String>> {
    let spec = task
        .command_spec(str::to_string)
        .map_err(|error| EmitterError::InvalidIR(error.to_string()))?;
    Ok(std::iter::once(spec.program).chain(spec.args).collect())
}
