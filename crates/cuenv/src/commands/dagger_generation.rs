//! Shared project compilation and workspace grouping for Dagger sync and export.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::ir::IntermediateRepresentation;
use cuenv_core::ci::{PipelineMode, PipelineTask};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskNode;
use cuenv_core::{Error, Result};
use cuenv_dagger::{DaggerCiConfig, DaggerWorkspaceEmitter, EmittedFile, WorkspacePipeline};
use cuenv_task_exec::TaskIndex;

pub(super) struct DaggerProject<'a> {
    pub project: &'a Project,
    pub project_dir: &'a Path,
    pub module_dir: &'a Path,
}

pub(super) struct DaggerGeneration<'a> {
    pub package: &'a str,
    pub repository: &'a Path,
    pub pipeline: Option<&'a str>,
    pub selection: PipelineSelection<'a>,
}

#[derive(Clone, Copy)]
pub(super) enum PipelineSelection<'a> {
    DaggerProvider,
    Explicit,
    ExplicitProject(&'a Path),
}

struct PlannedPipeline<'a> {
    project: &'a Project,
    project_dir: PathBuf,
    module_dir: PathBuf,
    ir: IntermediateRepresentation,
}

pub(super) struct DaggerModule<'a> {
    config: DaggerCiConfig,
    pipelines: Vec<PlannedPipeline<'a>>,
}

impl DaggerModule<'_> {
    fn inputs<'a>(&'a self, package: &'a str) -> Vec<WorkspacePipeline<'a>> {
        self.pipelines
            .iter()
            .map(|pipeline| WorkspacePipeline {
                ir: &pipeline.ir,
                project: Some(pipeline.project),
                project_dir: &pipeline.project_dir,
                module_dir: &pipeline.module_dir,
                package,
            })
            .collect()
    }

    pub(super) fn emit_source(&self, package: &str) -> Result<String> {
        DaggerWorkspaceEmitter::new(self.config.clone())
            .emit_module(&self.inputs(package))
            .map_err(|error| {
                Error::configuration(format!("Dagger module generation failed: {error}"))
            })
    }

    pub(super) fn emit_workspace(&self, package: &str) -> Result<Vec<EmittedFile>> {
        DaggerWorkspaceEmitter::new(self.config.clone())
            .emit_workspace(&self.inputs(package))
            .map_err(|error| {
                Error::configuration(format!("Dagger workspace generation failed: {error}"))
            })
    }
}

pub(super) fn compile_modules<'a>(
    mut projects: Vec<DaggerProject<'a>>,
    request: &DaggerGeneration<'_>,
) -> Result<Vec<DaggerModule<'a>>> {
    projects.sort_by_key(|project| project.project_dir);
    let mut modules = BTreeMap::new();
    for project in projects {
        append_pipelines(&mut modules, &project, request)?;
    }
    Ok(modules.into_values().collect())
}

fn append_pipelines<'a>(
    modules: &mut BTreeMap<(PathBuf, String), DaggerModule<'a>>,
    project: &DaggerProject<'a>,
    request: &DaggerGeneration<'_>,
) -> Result<()> {
    let Some(ci) = &project.project.ci else {
        return Ok(());
    };
    let mut names: Vec<&str> = ci.pipelines.keys().map(String::as_str).collect();
    names.sort_unstable();
    let names = names.into_iter().filter(|&name| {
        request.pipeline.is_none_or(|wanted| name == wanted)
            && (match request.selection {
                PipelineSelection::Explicit => true,
                PipelineSelection::ExplicitProject(path) => path == project.project_dir,
                PipelineSelection::DaggerProvider => false,
            } || ci
                .providers_for_pipeline(name)
                .iter()
                .any(|provider| provider == "dagger"))
    });
    let provider = ci
        .provider
        .as_ref()
        .and_then(|provider| provider.get("dagger"));
    for name in names {
        let Some(pipeline) = ci.pipelines.get(name) else {
            continue;
        };
        let override_config = pipeline
            .provider
            .as_ref()
            .and_then(|provider| provider.get("dagger"));
        let config = provider_config(provider, override_config).map_err(|error| {
            Error::configuration(format!(
                "Invalid Dagger provider configuration in '{}:{name}': {error}",
                project.project.name
            ))
        })?;
        if pipeline.mode == PipelineMode::Thin
            && let Some(binary) = config.cuenv_binary.as_deref()
        {
            validate_binary(request.repository, Path::new(binary))?;
        }
        let workspace = request.repository.join(config.workspace_dir());
        let project_dir = relative_to_repository(project.project_dir, request.repository)?;
        let module_dir = relative_to_repository(project.module_dir, request.repository)?;
        let key = (workspace, config.module_name().to_string());
        let module = modules.entry(key).or_insert_with(|| DaggerModule {
            config: config.clone(),
            pipelines: Vec::new(),
        });
        if module.config != config {
            return Err(Error::configuration(format!(
                "Pipelines sharing Dagger module '{}' must use the same provider configuration. Set a distinct moduleName for '{}:{name}'.",
                config.module_name(),
                project.project.name
            )));
        }
        let mut ir = Compiler::with_options(
            project.project.clone(),
            CompilerOptions {
                module_root: Some(project.module_dir.to_path_buf()),
                project_root: Some(project.project_dir.to_path_buf()),
                project_path: project
                    .project_dir
                    .strip_prefix(project.module_dir)
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned()),
                pipeline_name: Some(name.to_string()),
                pipeline: Some(pipeline.clone()),
                project_package: Some(request.package.to_owned()),
                ci_mode: true,
                ..CompilerOptions::default()
            },
        )
        .compile()
        .map_err(|error| {
            Error::configuration(format!(
                "Failed to compile Dagger pipeline '{name}' for project '{}': {error}",
                project.project.name
            ))
        })?;
        ir.pipeline.name = name.to_string();
        ir.pipeline.mode = pipeline.mode;
        ir.pipeline.project_name = Some(project.project.name.clone());
        if pipeline.mode == PipelineMode::Expanded {
            filter_expanded_tasks(project.project, &mut ir)?;
        }
        module.pipelines.push(PlannedPipeline {
            project: project.project,
            project_dir: project_dir.clone(),
            module_dir: module_dir.clone(),
            ir,
        });
    }
    Ok(())
}

fn relative_to_repository(path: &Path, repository: &Path) -> Result<PathBuf> {
    path.strip_prefix(repository).map(Path::to_path_buf).map_err(|_| {
        Error::configuration(format!(
            "Dagger repository root '{}' must contain the CUE module and project. '{}' is outside it.",
            repository.display(), path.display()
        ))
    })
}

pub(super) fn repository_root(module_root: &Path) -> PathBuf {
    gix::discover(module_root)
        .ok()
        .and_then(|repo| repo.workdir().map(Path::to_path_buf))
        .unwrap_or_else(|| module_root.to_path_buf())
}

fn provider_config(
    base: Option<&serde_json::Value>,
    pipeline: Option<&serde_json::Value>,
) -> Result<DaggerCiConfig> {
    let mut config = serde_json::Map::new();
    for value in base.into_iter().chain(pipeline) {
        let properties = value.as_object().ok_or_else(|| {
            Error::configuration("Dagger provider configuration must be an object")
        })?;
        config.extend(properties.clone());
    }
    serde_json::from_value(serde_json::Value::Object(config))
        .map_err(|error| Error::configuration(error.to_string()))
}

fn filter_expanded_tasks(project: &Project, ir: &mut IntermediateRepresentation) -> Result<()> {
    if ir
        .pipeline
        .pipeline_task_defs
        .iter()
        .any(|task| !matches!(task, PipelineTask::Simple(_)))
    {
        return Err(Error::configuration(format!(
            "Dagger expanded pipeline '{}' in '{}' requires named task references; use thin mode for inline or matrix tasks",
            ir.pipeline.name, project.name
        )));
    }
    let index = TaskIndex::build(&project.tasks)?;
    for task in ir.tasks.iter_mut().filter(|task| task.phase.is_none()) {
        if let Ok(entry) = index.resolve(&task.id)
            && let TaskNode::Task(source) = &entry.node
        {
            for producer in source.iter_task_output_names() {
                if !producer.starts_with('#')
                    && !task
                        .depends_on
                        .iter()
                        .any(|dependency| dependency == producer)
                {
                    task.depends_on.push(producer.to_string());
                }
            }
        }
    }
    let tasks = std::mem::take(&mut ir.tasks);
    let (phase, runnable): (Vec<_>, Vec<_>) =
        tasks.into_iter().partition(|task| task.phase.is_some());
    ir.tasks = cuenv_ci::pipeline::filter_tasks(&ir.pipeline.pipeline_tasks, runnable);
    ir.tasks.extend(phase);
    if ir.regular_tasks().next().is_none() {
        return Err(Error::configuration(format!(
            "Dagger expanded pipeline '{}' in '{}' selects no runnable tasks",
            ir.pipeline.name, project.name
        )));
    }
    Ok(())
}

fn validate_binary(repository: &Path, binary: &Path) -> Result<()> {
    if !binary
        .components()
        .any(|component| matches!(component, std::path::Component::Normal(_)))
        || binary.components().any(|component| {
            !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err(Error::configuration(
            "Dagger cuenvBinary must name a file relative to the Git root",
        ));
    }
    let path = repository.join(binary);
    let target = path.canonicalize().map_err(|error| {
        Error::io_with_path("find bundled Dagger cuenv executable", path.clone(), error)
    })?;
    if !target.starts_with(repository) || !target.is_file() {
        return Err(Error::configuration(format!(
            "Dagger cuenvBinary '{}' must resolve to a file within the Git root",
            binary.display()
        )));
    }
    Ok(())
}
