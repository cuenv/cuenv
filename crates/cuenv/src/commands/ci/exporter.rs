//! CI Pipeline Export Module
//!
//! Handles exporting CI pipelines to various formats (Buildkite, GitLab, GitHub Actions, CircleCI).
//! Outputs to stdout by default for dynamic pipeline upload, or to a file with --output.

use super::args::{CiArgs, ExportFormat};
use crate::providers;

use cuenv_ci::emitter::Emitter;
use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata};
use cuenv_core::Result;
use cuenv_core::ci::PipelineMode;
use cuenv_core::manifest::Project;

use std::io::Write;
use std::path::PathBuf;

/// Execute export mode - generate pipeline YAML.
///
/// # Arguments
/// * `args` - CLI arguments
/// * `format` - Export format (buildkite, gitlab, etc.)
///
/// # Errors
///
/// Returns error if:
/// - No projects are found
/// - Pipeline compilation fails
/// - Emitter is not available (feature not enabled)
/// - File write fails (when --output specified)
pub async fn execute_export(args: &CiArgs, format: ExportFormat) -> Result<()> {
    let provider = providers::detect_ci_provider(args.from.clone());
    let context = provider.context();
    let changes = provider.changed_files().await?;

    tracing::info!(
        provider = %context.provider,
        event = %context.event,
        ref_name = %context.ref_name,
        changed_files = changes.files().len(),
        "Export context"
    );

    // Evaluate module and discover projects
    let module = cuenv_ci::discovery::evaluate_module_for_package(&args.package)?;
    let project_count = module.project_count();
    if project_count == 0 {
        return Err(cuenv_core::Error::configuration(format!(
            "No cuenv projects found. Ensure env.cue files declare 'package {}'",
            args.package
        )));
    }

    // Collect projects with their configs
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = instance.to_project()?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }

    let path_filter = if format == ExportFormat::Dagger {
        super::runner::resolve_path_filter(&args.path)?.map(|path| module.root.join(path))
    } else {
        (args.path != ".").then(|| module.root.join(&args.path))
    };
    let projects: Vec<_> = projects
        .into_iter()
        .filter(|(path, config)| {
            path_filter
                .as_ref()
                .is_none_or(|filter| path.starts_with(filter))
                && args
                    .project
                    .as_ref()
                    .is_none_or(|project| config.name == *project)
        })
        .collect();
    if args.project.is_some() && projects.len() != 1 {
        return Err(cuenv_core::Error::configuration(format!(
            "--project must select one project within --path; found {}",
            projects.len()
        )));
    }
    if format == ExportFormat::Dagger {
        return export_dagger(args, &module.root, &projects);
    }

    tracing::info!(count = projects.len(), "Found projects");

    let pipeline_name = args.pipeline_name();

    // Collect all tasks for export. A project's file references bind among
    // what that project imports, read from its instance in the module.
    let collected = collect_tasks_for_export(
        &projects,
        ExportPipeline {
            name: pipeline_name,
            module_root: &module.root,
        },
    )?;

    if collected.tasks.is_empty() {
        tracing::info!("No tasks to export");
        let empty_yaml = match format {
            ExportFormat::Buildkite => "steps: []\n",
            ExportFormat::Gitlab => "{}\n",
            ExportFormat::GithubActions => "jobs: {}\n",
            ExportFormat::Circleci => "version: 2.1\njobs: {}\n",
            ExportFormat::Dagger => unreachable!("Dagger export uses its own module"),
        };
        output_yaml(args, empty_yaml)?;
        return Ok(());
    }

    // Build combined IR
    let combined_ir = IntermediateRepresentation {
        version: "1.5".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name.to_string(),
            mode: PipelineMode::default(),
            environment: collected.environment,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: collected.runtimes,
        tasks: collected.tasks,
    };

    // Get emitter and generate YAML
    let yaml = emit_pipeline(&combined_ir, format)?;
    output_yaml(args, &yaml)?;

    Ok(())
}

/// Collected tasks from project discovery.
struct CollectedTasks {
    tasks: Vec<cuenv_ci::ir::Task>,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    environment: Option<String>,
}

/// The pipeline an export is for.
#[derive(Clone, Copy)]
struct ExportPipeline<'pipeline> {
    name: &'pipeline str,
    /// The root of the CUE module the projects belong to.
    module_root: &'pipeline std::path::Path,
}

/// Collect all IR tasks from all projects for a given pipeline.
///
/// For export mode, we collect ALL tasks defined in the pipeline rather than
/// filtering by affected files. The CI orchestrator handles trigger logic.
fn collect_tasks_for_export(
    projects: &[(PathBuf, Project)],
    pipeline: ExportPipeline<'_>,
) -> Result<CollectedTasks> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions, FileAnchors};

    let pipeline_name = pipeline.name;
    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_runtimes = Vec::new();

    for (project_path, config) in projects {
        let Some(ci) = &config.ci else {
            continue;
        };

        let Some(ci_pipeline) = ci.pipelines.get(pipeline_name) else {
            continue;
        };

        pipeline_environment.clone_from(&ci_pipeline.environment);

        // Extract task names from pipeline for logging
        let pipeline_task_names: Vec<String> = ci_pipeline
            .tasks
            .iter()
            .map(|t| t.task_name().to_string())
            .collect();

        if pipeline_task_names.is_empty() {
            tracing::debug!(
                project = %project_path.display(),
                "No tasks in pipeline"
            );
            continue;
        }

        tracing::info!(
            project = %project_path.display(),
            tasks = ?pipeline_task_names,
            "Exporting pipeline tasks"
        );

        // Compile with pipeline context
        let options = CompilerOptions {
            pipeline_name: Some(pipeline_name.to_string()),
            pipeline: Some(ci_pipeline.clone()),
            file_anchors: Some(FileAnchors::Instance {
                module_root: pipeline.module_root.to_path_buf(),
                directory: project_path.clone(),
            }),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        // Collect all tasks from the compiled IR (phase tasks + regular tasks)
        // and every project's runtimes, keeping the first runtime per id.
        for runtime in ir.runtimes {
            if !compiled_runtimes
                .iter()
                .any(|existing: &cuenv_ci::ir::Runtime| existing.id == runtime.id)
            {
                compiled_runtimes.push(runtime);
            }
        }
        all_ir_tasks.extend(ir.tasks);
    }

    Ok(CollectedTasks {
        tasks: all_ir_tasks,
        runtimes: compiled_runtimes,
        environment: pipeline_environment,
    })
}

/// Emit pipeline using the appropriate emitter.
fn emit_pipeline(ir: &IntermediateRepresentation, format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Buildkite => emit_buildkite(ir),
        ExportFormat::Gitlab => emit_gitlab(ir),
        ExportFormat::GithubActions => emit_github_actions(ir),
        ExportFormat::Circleci => emit_circleci(ir),
        ExportFormat::Dagger => unreachable!("Dagger export uses its own module"),
    }
}

#[cfg(feature = "dagger")]
fn export_dagger(
    args: &CiArgs,
    root: &std::path::Path,
    projects: &[(PathBuf, Project)],
) -> Result<()> {
    use crate::commands::dagger_generation::{
        DaggerGeneration, DaggerProject, PipelineSelection, compile_modules, repository_root,
    };

    let repository = repository_root(root);
    let selected = projects
        .iter()
        .map(|(path, project)| DaggerProject {
            project,
            project_dir: path,
            module_dir: root,
        })
        .collect();
    let modules = compile_modules(
        selected,
        &DaggerGeneration {
            package: &args.package,
            repository: &repository,
            pipeline: Some(args.pipeline_name()),
            selection: PipelineSelection::Explicit,
        },
    )?;
    let module = match modules.as_slice() {
        [module] => module,
        [] => {
            return Err(cuenv_core::Error::configuration(format!(
                "No project has Dagger pipeline '{}'",
                args.pipeline_name()
            )));
        }
        _ => {
            return Err(cuenv_core::Error::configuration(
                "Dagger export produces one module. Select a project with --project or use 'cuenv sync ci --provider dagger' to generate multiple modules.",
            ));
        }
    };
    output_yaml(args, &module.emit_source(&args.package)?)
}

#[cfg(not(feature = "dagger"))]
fn export_dagger(
    _args: &CiArgs,
    _root: &std::path::Path,
    _projects: &[(PathBuf, Project)],
) -> Result<()> {
    Err(cuenv_core::Error::configuration(
        "Dagger support is not enabled. Rebuild with --features dagger",
    ))
}

#[cfg(feature = "buildkite")]
fn emit_buildkite(ir: &IntermediateRepresentation) -> Result<String> {
    use cuenv_buildkite::BuildkiteEmitter;
    let emitter = BuildkiteEmitter::new().with_emojis();
    emitter
        .emit(ir)
        .map_err(|e| cuenv_core::Error::configuration(format!("Buildkite emitter failed: {e}")))
}

#[cfg(not(feature = "buildkite"))]
fn emit_buildkite(_ir: &IntermediateRepresentation) -> Result<String> {
    Err(cuenv_core::Error::configuration(
        "Buildkite support is not enabled. Rebuild with --features buildkite",
    ))
}

#[cfg(feature = "github")]
fn emit_github_actions(ir: &IntermediateRepresentation) -> Result<String> {
    use cuenv_github::workflow::GitHubActionsEmitter;
    let emitter = GitHubActionsEmitter::default();
    emitter.emit(ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("GitHub Actions emitter failed: {e}"))
    })
}

#[cfg(not(feature = "github"))]
fn emit_github_actions(_ir: &IntermediateRepresentation) -> Result<String> {
    Err(cuenv_core::Error::configuration(
        "GitHub Actions support is not enabled. Rebuild with --features github",
    ))
}

fn emit_gitlab(_ir: &IntermediateRepresentation) -> Result<String> {
    // TODO: Implement GitLab emitter
    Err(cuenv_core::Error::configuration(
        "GitLab CI export is not yet implemented",
    ))
}

fn emit_circleci(_ir: &IntermediateRepresentation) -> Result<String> {
    // TODO: Implement CircleCI emitter
    Err(cuenv_core::Error::configuration(
        "CircleCI export is not yet implemented",
    ))
}

/// Output YAML to stdout or file based on args.
fn output_yaml(args: &CiArgs, yaml: &str) -> Result<()> {
    if let Some(path) = &args.output {
        let mut file = std::fs::File::create(path)
            .map_err(|e| cuenv_core::Error::io_with_path("create", path.clone(), e))?;
        file.write_all(yaml.as_bytes())
            .map_err(|e| cuenv_core::Error::io_with_path("write", path.clone(), e))?;
        tracing::info!(path = %path.display(), "Wrote pipeline YAML");
        Ok(())
    } else {
        cuenv_events::println_redacted(yaml);
        Ok(())
    }
}
