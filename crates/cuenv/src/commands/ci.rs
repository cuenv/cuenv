//! CI command execution module
//!
//! Handles CI pipeline execution for various providers.
//! For workflow file generation, use `cuenv sync ci` instead.

use crate::providers::detect_ci_provider;
use cuenv_ci::executor::run_ci;
use cuenv_core::Result;

/// Execute CI pipelines.
///
/// - `dry_run`: Show what would be executed without running
/// - `pipeline`: Force a specific pipeline to run
/// - `dynamic`: Output dynamic pipeline YAML to stdout (e.g., "buildkite")
/// - `from`: Base ref for affected task detection
#[allow(clippy::print_stdout)]
pub async fn execute_ci(
    dry_run: bool,
    pipeline: Option<String>,
    dynamic: Option<String>,
    from: Option<String>,
) -> Result<()> {
    // Handle --dynamic option for dynamic pipeline output to stdout
    if let Some(provider) = dynamic {
        return execute_dynamic_output(&provider, pipeline, from).await;
    }

    // Default: run CI pipelines locally
    let provider = detect_ci_provider(from);
    run_ci(provider, dry_run, pipeline).await
}

/// Output dynamic pipeline YAML to stdout for CI systems that support dynamic pipelines.
///
/// Currently only Buildkite is supported for dynamic output.
#[allow(clippy::print_stdout)]
async fn execute_dynamic_output(
    provider: &str,
    pipeline: Option<String>,
    from: Option<String>,
) -> Result<()> {
    match provider {
        "buildkite" => {
            #[cfg(feature = "buildkite")]
            {
                execute_dynamic_buildkite(pipeline, from).await
            }
            #[cfg(not(feature = "buildkite"))]
            {
                let _ = (pipeline, from);
                Err(cuenv_core::Error::configuration(
                    "Buildkite support is not enabled. Rebuild with --features buildkite",
                ))
            }
        }
        _ => Err(cuenv_core::Error::configuration(format!(
            "Unsupported dynamic provider: {provider}. Only 'buildkite' supports dynamic output. \
             For GitHub Actions workflow files, use 'cuenv sync ci' instead."
        ))),
    }
}

// ============================================================================
// Buildkite Dynamic Pipeline Generation
// ============================================================================

/// Result of collecting affected tasks from projects
#[cfg(feature = "buildkite")]
struct CollectedTasks {
    tasks: Vec<cuenv_ci::ir::Task>,
    stages: cuenv_ci::ir::StageConfiguration,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    environment: Option<String>,
}

/// Collect affected IR tasks from all projects for a given pipeline
#[cfg(feature = "buildkite")]
fn collect_affected_tasks_for_pipeline(
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
    project_map: &std::collections::HashMap<String, cuenv_ci::discovery::DiscoveredCIProject>,
    pipeline_name: &str,
    changed_files: &[std::path::PathBuf],
    event: &str,
) -> Result<CollectedTasks> {
    use cuenv_ci::affected::compute_affected_tasks;
    use cuenv_ci::compiler::{Compiler, CompilerOptions};
    use cuenv_ci::ir::StageConfiguration;

    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_stages = StageConfiguration::default();
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

        // Extract task names from pipeline tasks (which can be simple strings or matrix tasks)
        let pipeline_task_names: Vec<String> = ci_pipeline
            .tasks
            .iter()
            .map(|t| t.task_name().to_string())
            .collect();

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
            eprintln!("Project {}: No affected tasks", project.path.display());
            continue;
        }

        eprintln!(
            "Project {}: Affected tasks {:?}",
            project.path.display(),
            tasks_to_run
        );

        // Compile with pipeline context for environment-aware contributors
        let options = CompilerOptions {
            pipeline: Some(ci_pipeline.clone()),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        let affected_tasks: Vec<_> = ir
            .tasks
            .into_iter()
            .filter(|t| tasks_to_run.contains(&t.id))
            .collect();

        // Capture stages and runtimes from compiled IR
        compiled_stages = ir.stages;
        compiled_runtimes = ir.runtimes;

        all_ir_tasks.extend(affected_tasks);
    }

    Ok(CollectedTasks {
        tasks: all_ir_tasks,
        stages: compiled_stages,
        runtimes: compiled_runtimes,
        environment: pipeline_environment,
    })
}

/// Execute Buildkite dynamic pipeline output - outputs pipeline YAML to stdout
#[cfg(feature = "buildkite")]
#[allow(clippy::print_stdout)]
async fn execute_dynamic_buildkite(pipeline: Option<String>, from: Option<String>) -> Result<()> {
    use cuenv_buildkite::BuildkiteEmitter;
    use cuenv_ci::discovery::discover_projects;
    use cuenv_ci::emitter::Emitter;
    use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata};
    use std::collections::HashMap;

    let provider = detect_ci_provider(from);
    let context = provider.context();
    let changed_files = provider.changed_files().await?;

    eprintln!(
        "Context: {} (event: {}, ref: {})",
        context.provider, context.event, context.ref_name
    );
    eprintln!("Changed files: {}", changed_files.len());

    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    eprintln!("Found {} projects", projects.len());

    let mut project_map = HashMap::new();
    for project in &projects {
        let name = project.config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), project.clone());
        }
    }

    let pipeline_name = pipeline.unwrap_or_else(|| "default".to_string());
    let collected = collect_affected_tasks_for_pipeline(
        &projects,
        &project_map,
        &pipeline_name,
        &changed_files,
        &context.event,
    )?;

    if collected.tasks.is_empty() {
        eprintln!("No affected tasks to run");
        println!("steps: []");
        return Ok(());
    }

    // Note: requires_onepassword is now derived from stages (1Password contributor)
    let combined_ir = IntermediateRepresentation {
        version: "1.4".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name,
            environment: collected.environment,
            requires_onepassword: false, // Derived from stages, not stored
            project_name: None,
            trigger: None,
            pipeline_tasks: vec![],
        },
        runtimes: collected.runtimes,
        stages: collected.stages,
        tasks: collected.tasks,
    };

    let emitter = BuildkiteEmitter::new().with_emojis();
    let yaml = emitter.emit(&combined_ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to emit Buildkite pipeline: {e}"))
    })?;

    println!("{yaml}");
    Ok(())
}
