use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use cuenv_ci::emitter::{EmitterError, EmitterResult};
use cuenv_ci::ir::Task as IrTask;
use cuenv_core::environment::{EnvSelection, EnvValue};
use cuenv_core::manifest::{DaggerCacheSharingMode, DaggerRuntime, Project, Runtime};
use cuenv_core::tasks::{Input, Task, TaskDirectoryBase, TaskNode};
use cuenv_task_exec::{TaskCommandExt, TaskIndex};

use super::{
    WorkspacePipeline, array, identifier, literal, relative_path, unsupported, upper_initial,
};

#[derive(Clone, Copy)]
pub(super) struct ExpandedConfig<'a> {
    pub scope: &'a str,
    pub project_id: usize,
    pub default_image: &'a str,
}

struct PlannedTask<'a> {
    ir: &'a IrTask,
    name: String,
    image: Image,
    runtime: Option<DaggerRuntime>,
    workdir: String,
    workspace_path: String,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    artifacts: BTreeSet<String>,
}

enum Image {
    Registry(String),
    Dockerfile(String),
}

pub(super) fn append_checks(
    out: &mut String,
    pipeline: &WorkspacePipeline<'_>,
    config: ExpandedConfig<'_>,
) -> EmitterResult<()> {
    let ir = pipeline.ir;
    if ir.tasks.iter().any(|task| task.phase.is_some()) {
        return Err(unsupported(format!(
            "CI contributor phases in pipeline '{}'; use thin mode",
            ir.pipeline.name
        )));
    }
    let index = pipeline
        .project
        .map(|project| TaskIndex::build(&project.tasks))
        .transpose()
        .map_err(|error| EmitterError::InvalidIR(error.to_string()))?;
    let project_env = collect_project_env(pipeline)?;
    let mut tasks = BTreeMap::new();
    for (position, ir_task) in ir.regular_tasks().enumerate() {
        let source = index
            .as_ref()
            .and_then(|index| index.resolve(&ir_task.id).ok())
            .and_then(|entry| match &entry.node {
                TaskNode::Task(task) => Some(task.as_ref()),
                _ => None,
            });
        let task = plan_task(TaskPlanInput {
            ir: ir_task,
            source,
            pipeline,
            default_image: config.default_image,
            project_env: &project_env,
            name: format!(
                "{}Task{position}{}",
                config.scope,
                upper_initial(&identifier(&ir_task.id))
            ),
        })?;
        tasks.insert(ir_task.id.as_str(), task);
    }
    validate_dependencies(&tasks)?;
    for task in tasks.values() {
        append_task(
            out,
            task,
            &TaskRenderContext {
                tasks: &tasks,
                project_id: config.project_id,
                project: pipeline.project,
                project_dir: format!("/{}", relative_path(pipeline.project_dir)?.display()),
            },
        )?;
    }
    Ok(())
}

struct TaskPlanInput<'a, 'ir> {
    ir: &'ir IrTask,
    source: Option<&'a Task>,
    pipeline: &'a WorkspacePipeline<'ir>,
    default_image: &'a str,
    project_env: &'a BTreeMap<String, String>,
    name: String,
}

fn plan_task<'a>(input: TaskPlanInput<'_, 'a>) -> EmitterResult<PlannedTask<'a>> {
    let TaskPlanInput {
        ir,
        source,
        pipeline,
        default_image,
        project_env,
        name,
    } = input;
    let mut command = ir.command.clone();
    let mut env = project_env.clone();
    env.extend(ir.env.clone());
    let mut artifacts = BTreeSet::new();
    if let Some(task) = source {
        validate_task_options(task, ir)?;
        let spec = task
            .command_spec(str::to_string)
            .map_err(|error| EmitterError::InvalidIR(error.to_string()))?;
        command = std::iter::once(spec.program).chain(spec.args).collect();
        for (key, value) in &task.env {
            let value = serde_json::from_value::<EnvValue>(value.clone()).map_err(|error| {
                EmitterError::InvalidIR(format!(
                    "invalid env var '{key}' on task '{}': {error}",
                    ir.id
                ))
            })?;
            env.insert(key.clone(), render_env(&value, key)?);
        }
        for input in &task.inputs {
            match input {
                Input::Path(_) => {}
                Input::Task(input) if input.map.is_none() => {
                    artifacts.insert(input.task.clone());
                }
                Input::Mapped(input) if !input.relocates() && input.producer_task.is_some() => {
                    artifacts.extend(input.producer_task.clone());
                }
                _ => {
                    return Err(unsupported(format!(
                        "anchored or relocated inputs on task '{}'; use thin mode",
                        ir.id
                    )));
                }
            }
        }
    } else if ir.shell || !ir.artifact_downloads.is_empty() {
        return Err(unsupported(format!(
            "unresolved command or artifact input on task '{}'; use thin mode",
            ir.id
        )));
    }
    if command.is_empty() {
        return Err(EmitterError::InvalidIR(format!(
            "task '{}' has no command",
            ir.id
        )));
    }
    for pattern in ir
        .inputs
        .iter()
        .chain(ir.outputs.iter().map(|output| &output.path))
    {
        relative_path(Path::new(pattern))?;
    }

    let (image, runtime) = plan_image(
        source.and_then(|task| task.runtime.as_ref()).or_else(|| {
            pipeline
                .project
                .and_then(|project| project.runtime.as_ref())
        }),
        ir,
        default_image,
    )?;
    let relative_workdir = resolve_workdir(pipeline, source)?;
    let workspace_path = runtime
        .as_ref()
        .map_or("/work", |runtime| runtime.workdir.as_str());
    if !workspace_path.starts_with('/') {
        return Err(EmitterError::InvalidIR(format!(
            "task '{}' declares a non-absolute Dagger workdir",
            ir.id
        )));
    }
    let workspace_path = workspace_path.trim_end_matches('/');
    let workspace_path = if workspace_path.is_empty() {
        "/"
    } else {
        workspace_path
    }
    .to_string();
    let workdir = Path::new(&workspace_path)
        .join(relative_workdir)
        .to_string_lossy()
        .into_owned();
    Ok(PlannedTask {
        ir,
        name,
        image,
        runtime,
        workdir,
        workspace_path,
        command,
        env,
        artifacts,
    })
}

fn plan_image(
    runtime: Option<&Runtime>,
    task: &IrTask,
    default_image: &str,
) -> EmitterResult<(Image, Option<DaggerRuntime>)> {
    match runtime {
        Some(Runtime::Dagger(runtime)) => {
            if runtime.engine.is_some() {
                return Err(unsupported(format!(
                    "task '{}' selects an engine; run it directly or use thin mode",
                    task.id
                )));
            }
            let image = match (&runtime.image, &runtime.dockerfile) {
                (Some(image), None) => Image::Registry(image.clone()),
                (None, Some(dockerfile)) => {
                    relative_path(Path::new(dockerfile))?;
                    Image::Dockerfile(dockerfile.clone())
                }
                _ => {
                    return Err(EmitterError::InvalidIR(format!(
                        "task '{}' requires exactly one image or Dockerfile",
                        task.id
                    )));
                }
            };
            Ok((image, Some(runtime.as_ref().clone())))
        }
        Some(Runtime::Container(runtime)) => Ok((Image::Registry(runtime.image.clone()), None)),
        Some(_) => Err(unsupported(format!(
            "non-container runtime on task '{}'; use thin mode",
            task.id
        ))),
        None => Ok((Image::Registry(default_image.to_string()), None)),
    }
}

fn validate_task_options(task: &Task, ir: &IrTask) -> EmitterResult<()> {
    if cuenv_core::tasks::has_output_refs(&task.args, &task.env) {
        return Err(unsupported(format!(
            "runtime task or image output reference on task '{}'; use thin mode",
            ir.id
        )));
    }
    if task.params.is_some()
        || task.timeout.is_some()
        || task.retry.is_some()
        || task.continue_on_error
        || !task.captures.is_empty()
    {
        return Err(unsupported(format!(
            "task parameters, timeout, retries, continue-on-error or captures on task '{}'; use thin mode",
            ir.id
        )));
    }
    Ok(())
}

fn collect_project_env(
    pipeline: &WorkspacePipeline<'_>,
) -> EmitterResult<BTreeMap<String, String>> {
    let Some(project) = pipeline.project else {
        return Ok(BTreeMap::new());
    };
    if !project.contexts.is_empty() || project.hooks.is_some() || !project.vcs.is_empty() {
        return Err(unsupported(format!(
            "contexts, hooks or VCS dependencies in project '{}'; use thin mode",
            project.name
        )));
    }
    let Some(env) = &project.env else {
        return Ok(BTreeMap::new());
    };
    env.select(EnvSelection {
        environment: pipeline.ir.pipeline.environment.as_deref(),
        contexts: &[],
    })
    .iter()
    .map(|(key, value)| Ok((key.clone(), render_env(value, key)?)))
    .collect()
}

pub(super) fn render_env(value: &EnvValue, key: &str) -> EmitterResult<String> {
    match value {
        EnvValue::String(value) if !value.starts_with("cuenv:") => Ok(value.clone()),
        EnvValue::Int(value) => Ok(value.to_string()),
        EnvValue::Bool(value) => Ok(value.to_string()),
        _ => Err(unsupported(format!(
            "runtime-resolved environment variable '{key}'; use thin mode"
        ))),
    }
}

fn resolve_workdir(
    pipeline: &WorkspacePipeline<'_>,
    task: Option<&Task>,
) -> EmitterResult<PathBuf> {
    let project_dir = relative_path(pipeline.project_dir)?;
    let module_dir = relative_path(pipeline.module_dir)?;
    let Some(task) = task else {
        return Ok(PathBuf::new());
    };
    let source_dir = |source: &cuenv_core::tasks::SourceLocation| {
        module_dir.join(source.directory().unwrap_or("."))
    };
    let mut path = task
        .source
        .as_ref()
        .map_or_else(|| project_dir.clone(), source_dir);
    if let Some(dir) = &task.directory {
        match dir.from {
            TaskDirectoryBase::Module => path.clone_from(&module_dir),
            TaskDirectoryBase::Caller => {
                let source = task.caller_source.as_ref().ok_or_else(|| {
                    EmitterError::InvalidIR("task dir references missing caller source".into())
                })?;
                path = source_dir(source);
            }
            TaskDirectoryBase::Definition => {}
        }
        path.push(relative_path(Path::new(&dir.path))?);
    }
    relative_path(&path)?
        .strip_prefix(project_dir)
        .map(Path::to_path_buf)
        .map_err(|_| unsupported("task working directories outside their project; use thin mode"))
}

fn validate_dependencies(tasks: &BTreeMap<&str, PlannedTask<'_>>) -> EmitterResult<()> {
    for task in tasks.values() {
        let ir = task.ir;
        if ir.matrix.is_some()
            || ir.manual_approval
            || ir.deployment
            || !ir.external_dependencies.is_empty()
            || ir.condition.is_some()
            || ir.concurrency_group.is_some()
            || !ir.secrets.is_empty()
        {
            return Err(unsupported(format!(
                "matrix, deployment, approval, external dependency, condition, concurrency or secret on task '{}'; use thin mode if supported by cuenv ci",
                ir.id
            )));
        }
        for name in ir.depends_on.iter().chain(task.artifacts.iter()) {
            if !tasks.contains_key(name.as_str()) {
                return Err(EmitterError::InvalidIR(format!(
                    "task '{}' depends on unavailable task '{name}'",
                    ir.id
                )));
            }
        }
    }

    // Drain ready tasks to reject cycles before emitting recursive functions.
    let mut pending: BTreeMap<&str, BTreeSet<&str>> = tasks
        .iter()
        .map(|(&name, task)| {
            (
                name,
                task.ir
                    .depends_on
                    .iter()
                    .chain(task.artifacts.iter())
                    .map(String::as_str)
                    .collect(),
            )
        })
        .collect();
    while !pending.is_empty() {
        let ready: BTreeSet<_> = pending
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(&name, _)| name)
            .collect();
        if ready.is_empty() {
            return Err(EmitterError::InvalidIR(format!(
                "Dagger check dependencies contain a cycle involving {}",
                pending.keys().copied().collect::<Vec<_>>().join(", ")
            )));
        }
        pending.retain(|name, deps| {
            deps.retain(|dep| !ready.contains(dep));
            !ready.contains(name)
        });
    }
    Ok(())
}

struct TaskRenderContext<'a> {
    tasks: &'a BTreeMap<&'a str, PlannedTask<'a>>,
    project_id: usize,
    project: Option<&'a Project>,
    project_dir: String,
}

fn append_task(
    out: &mut String,
    task: &PlannedTask<'_>,
    context: &TaskRenderContext<'_>,
) -> EmitterResult<()> {
    let name = &task.name;
    let helper = format!("run{}", upper_initial(name));
    let _ = writeln!(out, "\n  {helper}(ws: Workspace!): Container! {{");
    let dependencies: BTreeSet<_> = task
        .ir
        .depends_on
        .iter()
        .chain(task.artifacts.iter())
        .collect();
    for (index, dep) in dependencies.iter().enumerate() {
        let producer = &context.tasks[dep.as_str()];
        let _ = writeln!(
            out,
            "    let need{index} = run{}(ws)",
            upper_initial(&producer.name)
        );
        let _ = writeln!(out, "    need{index}.sync");
    }

    append_task_container(out, task, &context.project_dir);
    for (index, dep) in dependencies.iter().enumerate() {
        if !task.artifacts.contains(*dep) {
            continue;
        }
        let producer = &context.tasks[dep.as_str()];
        if producer.ir.outputs.is_empty() {
            return Err(EmitterError::InvalidIR(format!(
                "artifact producer '{dep}' has no outputs"
            )));
        }
        let outputs = array(
            producer
                .ir
                .outputs
                .iter()
                .map(|output| output.path.as_str()),
        );
        let relative = Path::new(&producer.workdir)
            .strip_prefix(&producer.workspace_path)
            .map_err(|_| {
                EmitterError::InvalidIR(format!("artifact producer '{dep}' has an invalid workdir"))
            })?;
        let destination = Path::new(&task.workspace_path).join(relative);
        let _ = writeln!(
            out,
            "      .withDirectory({}, need{index}.directory({}), include: {outputs})",
            literal(&destination.to_string_lossy()),
            literal(&producer.workdir)
        );
    }
    for service in super::services::task_services(task.ir) {
        let project = context.project.ok_or_else(|| {
            EmitterError::InvalidIR(format!("service '{service}' has no owning project"))
        })?;
        let name = super::services::service_name(project, context.project_id, service)?;
        let _ = writeln!(
            out,
            "      .withServiceBinding({}, {name})",
            literal(service)
        );
    }
    let nesting = task
        .runtime
        .as_ref()
        .filter(|runtime| runtime.disable_dagger_in_dagger)
        .map_or("", |_| ", disableDaggerInDagger: true");
    let _ = writeln!(
        out,
        "      .withExec({}{nesting})",
        array(task.command.iter().map(String::as_str))
    );
    let _ = writeln!(out, "  }}\n\n  pub {name}(ws: Workspace!): Void @check {{");
    let _ = writeln!(out, "    {helper}(ws).sync\n    null\n  }}");
    Ok(())
}

fn append_task_container(out: &mut String, task: &PlannedTask<'_>, project_dir: &str) {
    let platform = task
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.platform.as_deref());
    let source = literal(&task.workspace_path);
    let directory = literal(project_dir);
    match &task.image {
        Image::Registry(image) => {
            let container = platform.map_or("container".to_string(), |platform| {
                format!("container(platform: {})", literal(platform))
            });
            let _ = writeln!(out, "    {container}.from({})", literal(image));
        }
        Image::Dockerfile(dockerfile) => {
            let platform = platform.map_or_else(String::new, |platform| {
                format!(", platform: {}", literal(platform))
            });
            let _ = writeln!(
                out,
                "    ws.directory({directory}).dockerBuild(dockerfile: {}{platform})",
                literal(dockerfile)
            );
        }
    }
    let inputs = if task.ir.inputs.is_empty() {
        String::new()
    } else {
        format!(
            ", include: {}",
            array(task.ir.inputs.iter().map(String::as_str))
        )
    };
    let _ = writeln!(
        out,
        "      .withDirectory({source}, ws.directory({directory}{inputs}))"
    );
    let _ = writeln!(out, "      .withWorkdir({})", literal(&task.workdir));
    for (key, value) in &task.env {
        let _ = writeln!(
            out,
            "      .withEnvVariable({}, {})",
            literal(key),
            literal(value)
        );
    }
    if let Some(runtime) = &task.runtime {
        for volume in &runtime.cache_volumes {
            let sharing = match volume.sharing {
                DaggerCacheSharingMode::Shared => "SHARED",
                DaggerCacheSharingMode::Private => "PRIVATE",
                DaggerCacheSharingMode::Locked => "LOCKED",
            };
            let _ = writeln!(
                out,
                "      .withMountedCache({}, cacheVolume({}), sharing: CacheSharingMode.{sharing})",
                literal(&volume.path),
                literal(&volume.key)
            );
        }
    }
}
