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
use std::collections::HashMap;
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

    // Build project map for dependency resolution
    let project_map: HashMap<_, _> = projects
        .iter()
        .filter_map(|p| {
            let name = p.config.name.trim();
            if name.is_empty() {
                None
            } else {
                Some((name.to_string(), p.clone()))
            }
        })
        .collect();

    let pipeline_name = args.pipeline_name();

    // Collect affected tasks
    let collected = collect_affected_tasks_for_export(
        &projects,
        &project_map,
        pipeline_name,
        &changed_files,
        &context.event,
    )?;

    if collected.tasks.is_empty() {
        tracing::info!("No affected tasks to run");
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

/// Collect affected IR tasks from all projects for a given pipeline.
fn collect_affected_tasks_for_export(
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
    project_map: &HashMap<String, cuenv_ci::discovery::DiscoveredCIProject>,
    pipeline_name: &str,
    changed_files: &[std::path::PathBuf],
    event: &str,
) -> Result<CollectedTasks> {
    use cuenv_ci::affected::compute_affected_tasks;
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_runtimes = Vec::new();

    for project in projects {
        let config = &project.config;

        let Some(ci) = &config.ci else {
            continue;
        };

        let Some(ci_pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
            continue;
        };

        pipeline_environment.clone_from(&ci_pipeline.environment);

        let project_root = project.path.parent().map_or_else(
            || std::path::Path::new("."),
            |p| {
                if p.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else {
                    p
                }
            },
        );

        // Extract task names from pipeline
        let pipeline_task_names: Vec<String> = ci_pipeline
            .tasks
            .iter()
            .map(|t| t.task_name().to_string())
            .collect();

        // Determine tasks to run based on event type
        let tasks_to_run = if event == "release" {
            pipeline_task_names
        } else {
            compute_affected_tasks(
                changed_files,
                &pipeline_task_names,
                project_root,
                config,
                project_map,
            )
        };

        if tasks_to_run.is_empty() {
            tracing::debug!(
                project = %project.path.display(),
                "No affected tasks"
            );
            continue;
        }

        tracing::info!(
            project = %project.path.display(),
            tasks = ?tasks_to_run,
            "Affected tasks"
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

        // Separate phase tasks and regular tasks
        let phase_tasks: Vec<_> = ir
            .tasks
            .iter()
            .filter(|t| t.phase.is_some())
            .cloned()
            .collect();
        let affected_tasks: Vec<_> = ir
            .tasks
            .into_iter()
            .filter(|t| t.phase.is_none() && tasks_to_run.contains(&t.id))
            .collect();

        compiled_runtimes = ir.runtimes;
        all_ir_tasks.extend(phase_tasks);
        all_ir_tasks.extend(affected_tasks);
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
