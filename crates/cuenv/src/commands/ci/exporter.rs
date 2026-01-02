//! CI Pipeline Export Module
//!
//! Handles exporting CI pipelines to various formats (Buildkite, GitLab, GitHub Actions, CircleCI).
//! Outputs to stdout by default for dynamic pipeline upload, or to a file with --output.

use super::args::{CiArgs, ExportFormat};
use crate::providers::detect_ci_provider;
use cuenv_ci::discovery::discover_projects;
use cuenv_ci::emitter::Emitter;
use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata};
use cuenv_core::Result;
use std::io::Write;

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
#[allow(clippy::print_stdout)]
pub async fn execute_export(args: &CiArgs, format: ExportFormat) -> Result<()> {
    let provider = detect_ci_provider(args.from.clone());
    let context = provider.context();
    let changed_files = provider.changed_files().await?;

    tracing::info!(
        provider = %context.provider,
        event = %context.event,
        ref_name = %context.ref_name,
        changed_files = changed_files.len(),
        "Export context"
    );

    // Discover projects
    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }

    tracing::info!(count = projects.len(), "Found projects");

    let pipeline_name = args.pipeline_name();

    // Collect all tasks for export
    let collected = collect_tasks_for_export(&projects, pipeline_name)?;

    if collected.tasks.is_empty() {
        tracing::info!("No tasks to export");
        let empty_yaml = match format {
            ExportFormat::Buildkite => "steps: []\n",
            ExportFormat::Gitlab => "{}\n",
            ExportFormat::GithubActions => "jobs: {}\n",
            ExportFormat::Circleci => "version: 2.1\njobs: {}\n",
        };
        output_yaml(args, empty_yaml)?;
        return Ok(());
    }

    // Build combined IR
    let combined_ir = IntermediateRepresentation {
        version: "1.5".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name.to_string(),
            environment: collected.environment,
            requires_onepassword: false,
            project_name: None,
            trigger: None,
            pipeline_tasks: vec![],
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

/// Collect all IR tasks from all projects for a given pipeline.
///
/// For export mode, we collect ALL tasks defined in the pipeline rather than
/// filtering by affected files. The CI orchestrator handles trigger logic.
fn collect_tasks_for_export(
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
    pipeline_name: &str,
) -> Result<CollectedTasks> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_runtimes = Vec::new();

    for project in projects {
        let config = &project.config;

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
                project = %project.path.display(),
                "No tasks in pipeline"
            );
            continue;
        }

        tracing::info!(
            project = %project.path.display(),
            tasks = ?pipeline_task_names,
            "Exporting pipeline tasks"
        );

        // Compile with pipeline context
        let options = CompilerOptions {
            pipeline: Some(ci_pipeline.clone()),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        // Collect all tasks from the compiled IR (phase tasks + regular tasks)
        compiled_runtimes = ir.runtimes;
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
    }
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
#[allow(clippy::print_stdout)]
fn output_yaml(args: &CiArgs, yaml: &str) -> Result<()> {
    match &args.output {
        Some(path) => {
            let mut file = std::fs::File::create(path).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(path.clone().into_boxed_path()),
                operation: "create".to_string(),
            })?;
            file.write_all(yaml.as_bytes())
                .map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(path.clone().into_boxed_path()),
                    operation: "write".to_string(),
                })?;
            tracing::info!(path = %path.display(), "Wrote pipeline YAML");
            Ok(())
        }
        None => {
            println!("{yaml}");
            Ok(())
        }
    }
}
