//! CI Pipeline Orchestrator
//!
//! Main entry point for CI pipeline execution, integrating with the provider
//! system for context detection, file change tracking, and reporting.

// CI orchestrator outputs to stdout/stderr as part of its normal operation
#![allow(clippy::print_stdout, clippy::print_stderr)]

use crate::affected::{compute_affected_tasks, matched_inputs_for_task};
use crate::discovery::discover_projects;
use crate::ir::CachePolicy;
use crate::provider::CIProvider;
use crate::report::json::write_report;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use chrono::Utc;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use std::collections::HashMap;
use std::sync::Arc;

use super::ExecutorError;
use super::config::CIExecutorConfig;
use super::runner::{IRTaskRunner, TaskOutput};

/// Run the CI pipeline logic
///
/// This is the main entry point for CI execution, integrating with the provider
/// system for context detection, file change tracking, and reporting.
///
/// # Arguments
///
/// * `provider` - The CI provider to use for changed files detection and reporting
/// * `dry_run` - If true, don't actually run tasks
/// * `specific_pipeline` - If set, only run tasks from this pipeline
///
/// # Errors
/// Returns error if IO errors occur or tasks fail
#[allow(clippy::too_many_lines)]
pub async fn run_ci(
    provider: Arc<dyn CIProvider>,
    dry_run: bool,
    specific_pipeline: Option<String>,
) -> Result<()> {
    let context = provider.context();
    println!(
        "Context: {} (event: {}, ref: {})",
        context.provider, context.event, context.ref_name
    );

    // Get changed files
    let changed_files = provider.changed_files().await?;
    println!("Changed files: {}", changed_files.len());

    // Discover projects
    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    println!("Found {} projects", projects.len());

    // Build project map for cross-project dependency resolution
    let mut project_map = std::collections::HashMap::new();
    for project in &projects {
        let name = project.config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), project.clone());
        }
    }

    // Track if any project failed
    let mut any_failed = false;

    // Process each project
    for project in &projects {
        let config = &project.config;

        // Determine pipeline to run
        let pipeline_name = specific_pipeline
            .clone()
            .unwrap_or_else(|| "default".to_string());

        // Find pipeline in config
        let Some(ci) = &config.ci else {
            return Err(cuenv_core::Error::configuration(format!(
                "Project {} has no CI configuration",
                project.path.display()
            )));
        };

        let available_pipelines: Vec<&str> = ci.pipelines.iter().map(|p| p.name.as_str()).collect();
        let Some(pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
            return Err(cuenv_core::Error::configuration(format!(
                "Pipeline '{}' not found in project {}. Available pipelines: {}",
                pipeline_name,
                project.path.display(),
                available_pipelines.join(", ")
            )));
        };

        // Get the directory containing the env.cue file
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

        // For release events, run all tasks unconditionally (no affected-file filtering)
        let tasks_to_run = if context.event == "release" {
            pipeline.tasks.clone()
        } else {
            compute_affected_tasks(
                &changed_files,
                &pipeline.tasks,
                project_root,
                config,
                &project_map,
            )
        };

        if tasks_to_run.is_empty() {
            println!("Project {}: No affected tasks", project.path.display());
            continue;
        }

        println!(
            "Project {}: Running tasks {:?}",
            project.path.display(),
            tasks_to_run
        );

        if !dry_run {
            let result = execute_project_pipeline(
                project,
                config,
                &pipeline.name,
                &tasks_to_run,
                project_root,
                context,
                &changed_files,
                provider.as_ref(),
            )
            .await;

            if let Err(e) = result {
                eprintln!("Pipeline execution error: {e}");
                any_failed = true;
            } else if result.is_ok_and(|status| status == PipelineStatus::Failed) {
                any_failed = true;
            }
        }
    }

    if any_failed {
        return Err(cuenv_core::Error::configuration(
            "One or more CI tasks failed",
        ));
    }

    Ok(())
}

/// Execute a project's pipeline and handle reporting
#[allow(clippy::too_many_arguments)] // Pipeline execution requires many context params
#[allow(clippy::too_many_lines)] // Complex orchestration logic
async fn execute_project_pipeline(
    project: &crate::discovery::DiscoveredCIProject,
    config: &Project,
    pipeline_name: &str,
    tasks_to_run: &[String],
    project_root: &std::path::Path,
    context: &crate::context::CIContext,
    changed_files: &[std::path::PathBuf],
    provider: &dyn CIProvider,
) -> Result<PipelineStatus> {
    let start_time = Utc::now();
    let mut tasks_reports = Vec::new();
    let mut pipeline_status = PipelineStatus::Success;

    // Determine cache policy override based on context
    let cache_policy_override = if is_fork_pr(context) {
        Some(CachePolicy::Readonly)
    } else {
        None
    };

    // Create executor configuration with salt rotation support
    let mut executor_config = CIExecutorConfig::new(project_root.to_path_buf())
        .with_capture_output(true)
        .with_dry_run(false)
        .with_secret_salt(std::env::var("CUENV_SECRET_SALT").unwrap_or_default());

    // Add previous salt for rotation support
    if let Ok(prev_salt) = std::env::var("CUENV_SECRET_SALT_PREV")
        && !prev_salt.is_empty()
    {
        executor_config = executor_config.with_secret_salt_prev(prev_salt);
    }

    let _executor_config = if let Some(policy) = cache_policy_override {
        executor_config.with_cache_policy_override(policy)
    } else {
        executor_config
    };

    // Execute tasks
    for task_name in tasks_to_run {
        let inputs_matched =
            matched_inputs_for_task(task_name, config, changed_files, project_root);
        let outputs = config
            .tasks
            .get(task_name)
            .and_then(|def| def.as_single())
            .map(|task| task.outputs.clone())
            .unwrap_or_default();

        println!("  -> Executing {task_name}");
        let task_start = std::time::Instant::now();

        // Execute the task using the runner
        let result =
            execute_single_task_by_name(config, task_name, project_root, cache_policy_override)
                .await;

        let duration = u64::try_from(task_start.elapsed().as_millis()).unwrap_or(0);

        let (status, exit_code, cache_key) = match result {
            Ok(output) => {
                if output.success {
                    println!("  -> {task_name} passed");
                    (
                        TaskStatus::Success,
                        Some(output.exit_code),
                        if output.from_cache {
                            Some(format!("cached:{}", output.task_id))
                        } else {
                            None
                        },
                    )
                } else {
                    println!("  -> {task_name} failed (exit code {})", output.exit_code);
                    pipeline_status = PipelineStatus::Failed;
                    (TaskStatus::Failed, Some(output.exit_code), None)
                }
            }
            Err(e) => {
                println!("  -> {task_name} failed: {e}");
                pipeline_status = PipelineStatus::Failed;
                (TaskStatus::Failed, Some(1), None)
            }
        };

        tasks_reports.push(TaskReport {
            name: task_name.clone(),
            status,
            duration_ms: duration,
            exit_code,
            inputs_matched,
            cache_key,
            outputs,
        });
    }

    let completed_at = Utc::now();
    #[allow(clippy::cast_sign_loss)]
    let duration_ms = (completed_at - start_time).num_milliseconds() as u64;

    // Generate report
    let report = PipelineReport {
        version: cuenv_core::VERSION.to_string(),
        project: project.path.display().to_string(),
        pipeline: pipeline_name.to_string(),
        context: ContextReport {
            provider: context.provider.clone(),
            event: context.event.clone(),
            ref_name: context.ref_name.clone(),
            base_ref: context.base_ref.clone(),
            sha: context.sha.clone(),
            changed_files: changed_files
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        },
        started_at: start_time,
        completed_at: Some(completed_at),
        duration_ms: Some(duration_ms),
        status: pipeline_status,
        tasks: tasks_reports,
    };

    // Write reports and notify provider
    write_pipeline_report(&report, context, project);
    notify_provider(provider, &report, pipeline_name).await;

    Ok(pipeline_status)
}

/// Write pipeline report to disk
fn write_pipeline_report(
    report: &PipelineReport,
    context: &crate::context::CIContext,
    project: &crate::discovery::DiscoveredCIProject,
) {
    // Ensure report directory exists
    let report_dir = std::path::Path::new(".cuenv/reports");
    if let Err(e) = std::fs::create_dir_all(report_dir) {
        println!("Failed to create report directory: {e}");
        return;
    }

    let sha_dir = report_dir.join(&context.sha);
    let _ = std::fs::create_dir_all(&sha_dir);

    let project_filename = project.path.display().to_string().replace(['/', '\\'], "-") + ".json";
    let report_path = sha_dir.join(project_filename);

    if let Err(e) = write_report(report, &report_path) {
        println!("Failed to write report: {e}");
    } else {
        println!("Report written to: {}", report_path.display());
    }

    // Write GitHub Job Summary
    if let Err(e) = crate::report::markdown::write_job_summary(report) {
        eprintln!("Warning: Failed to write job summary: {e}");
    }
}

/// Notify CI provider about pipeline results
async fn notify_provider(provider: &dyn CIProvider, report: &PipelineReport, pipeline_name: &str) {
    // Post results to CI provider
    let check_name = format!("cuenv: {pipeline_name}");
    match provider.create_check(&check_name).await {
        Ok(handle) => {
            if let Err(e) = provider.complete_check(&handle, report).await {
                eprintln!("Warning: Failed to complete check run: {e}");
            }
        }
        Err(e) => {
            eprintln!("Warning: Failed to create check run: {e}");
        }
    }

    // Post PR comment with report summary
    if let Err(e) = provider.upload_report(report).await {
        eprintln!("Warning: Failed to post PR comment: {e}");
    }
}

/// Check if this is a fork PR (should use readonly cache)
fn is_fork_pr(context: &crate::context::CIContext) -> bool {
    // Fork PRs typically have a different head repo than base repo
    // This is a simplified check - providers may need more sophisticated detection
    context.event == "pull_request" && context.ref_name.starts_with("refs/pull/")
}

/// Execute a single task by name using the existing project config
///
/// This bridges the gap between the task name-based execution in `run_ci`
/// and the IR-based execution in `CIExecutor`.
async fn execute_single_task_by_name(
    config: &Project,
    task_name: &str,
    project_root: &std::path::Path,
    cache_policy_override: Option<CachePolicy>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    // Get task definition
    let Some(task_def) = config.tasks.get(task_name) else {
        return Err(ExecutorError::Compilation(format!(
            "Task '{task_name}' not found in project config"
        )));
    };

    let Some(task) = task_def.as_single() else {
        return Err(ExecutorError::Compilation(format!(
            "Task '{task_name}' is a group, not a single task"
        )));
    };

    // Build a minimal IR task for execution
    let ir_task = crate::ir::Task {
        id: task_name.to_string(),
        runtime: None, // TODO: Support runtime from task config
        command: if task.command.is_empty() {
            // If no command, use script
            vec![task.script.clone().unwrap_or_default()]
        } else {
            vec![task.command.clone()]
        },
        shell: task.script.is_some() || !task.command.is_empty(),
        env: task
            .env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
        secrets: HashMap::new(), // Secrets handled separately
        resources: None,
        concurrency_group: None,
        inputs: task
            .inputs
            .iter()
            .filter_map(|i| i.as_path())
            .cloned()
            .collect(),
        outputs: vec![],
        depends_on: task.depends_on.clone(),
        cache_policy: cache_policy_override.unwrap_or(CachePolicy::Normal),
        deployment: false,
        manual_approval: false,
    };

    // Build environment
    let mut env: HashMap<String, String> = task
        .env
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();

    // Add PATH and HOME
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    if let Ok(home) = std::env::var("HOME") {
        env.insert("HOME".to_string(), home);
    }

    // Execute using runner directly
    let runner = IRTaskRunner::new(project_root.to_path_buf(), true);
    let output = runner.execute(&ir_task, env).await?;

    Ok(output)
}
