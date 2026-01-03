//! CI Pipeline Orchestrator
//!
//! Main entry point for CI pipeline execution, integrating with the provider
//! system for context detection, file change tracking, and reporting.
//!
//! This module orchestrates complex async workflows with caching, concurrency control,
//! and multi-project coordination. The complexity is inherent to the domain.

// CI orchestration has inherent complexity - coordinates async tasks, caching, reporting
#![allow(clippy::cognitive_complexity, clippy::too_many_lines)]

use crate::affected::{compute_affected_tasks, matched_inputs_for_task};
use crate::discovery::evaluate_module_from_cwd;
use crate::ir::CachePolicy;
use crate::provider::CIProvider;
use crate::report::json::write_report;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use chrono::Utc;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    cuenv_events::emit_ci_context!(&context.provider, &context.event, &context.ref_name);

    // Get changed files
    let changed_files = provider.changed_files().await?;
    cuenv_events::emit_ci_changed_files!(changed_files.len());

    // Evaluate module and discover projects
    let module = evaluate_module_from_cwd()?;
    let project_count = module.project_count();
    if project_count == 0 {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    cuenv_events::emit_ci_projects_discovered!(project_count);

    // Collect projects with their configs
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = Project::try_from(instance)?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }

    // Build project map for cross-project dependency resolution
    let mut project_map = std::collections::HashMap::new();
    for (path, config) in &projects {
        let name = config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), (path.clone(), config.clone()));
        }
    }

    // Track if any project failed
    let mut any_failed = false;

    // Process each project
    for (project_path, config) in &projects {
        // Determine pipeline to run
        let pipeline_name = specific_pipeline
            .clone()
            .unwrap_or_else(|| "default".to_string());

        // Find pipeline in config
        let Some(ci) = &config.ci else {
            return Err(cuenv_core::Error::configuration(format!(
                "Project {} has no CI configuration",
                project_path.display()
            )));
        };

        let available_pipelines: Vec<&str> = ci.pipelines.keys().map(String::as_str).collect();
        let Some(pipeline) = ci.pipelines.get(&pipeline_name) else {
            return Err(cuenv_core::Error::configuration(format!(
                "Pipeline '{}' not found in project {}. Available pipelines: {}",
                pipeline_name,
                project_path.display(),
                available_pipelines.join(", ")
            )));
        };

        // Extract task names from pipeline tasks (which can be simple strings or matrix tasks)
        let pipeline_task_names: Vec<String> = pipeline
            .tasks
            .iter()
            .map(|t| t.task_name().to_string())
            .collect();

        // For release events, run all tasks unconditionally (no affected-file filtering)
        let tasks_to_run = if context.event == "release" {
            pipeline_task_names
        } else {
            compute_affected_tasks(
                &changed_files,
                &pipeline_task_names,
                project_path,
                config,
                &project_map,
            )
        };

        if tasks_to_run.is_empty() {
            cuenv_events::emit_ci_project_skipped!(project_path.display(), "No affected tasks");
            continue;
        }

        tracing::info!(
            project = %project_path.display(),
            tasks = ?tasks_to_run,
            "Running tasks for project"
        );

        if !dry_run {
            let result = execute_project_pipeline(
                project_path,
                config,
                &pipeline_name,
                &tasks_to_run,
                context,
                &changed_files,
                provider.as_ref(),
            )
            .await;

            if let Err(e) = result {
                tracing::error!(error = %e, "Pipeline execution error");
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
    project_path: &Path,
    config: &Project,
    pipeline_name: &str,
    tasks_to_run: &[String],
    context: &crate::context::CIContext,
    changed_files: &[PathBuf],
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
    let mut executor_config = CIExecutorConfig::new(project_path.to_path_buf())
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

    // Register common CI secret patterns for redaction.
    // These are typically passed via GitHub Actions secrets or similar.
    register_ci_secrets();

    // Execute tasks
    for task_name in tasks_to_run {
        let inputs_matched =
            matched_inputs_for_task(task_name, config, changed_files, project_path);
        let outputs = config
            .tasks
            .get(task_name)
            .and_then(|def| def.as_single())
            .map(|task| task.outputs.clone())
            .unwrap_or_default();

        let project_display = project_path.display().to_string();
        cuenv_events::emit_ci_task_executing!(&project_display, task_name);
        let task_start = std::time::Instant::now();

        // Execute the task using the runner
        let result =
            execute_single_task_by_name(config, task_name, project_path, cache_policy_override)
                .await;

        let duration = u64::try_from(task_start.elapsed().as_millis()).unwrap_or(0);

        let (status, exit_code, cache_key) = match result {
            Ok(output) => {
                if output.success {
                    cuenv_events::emit_ci_task_result!(&project_display, task_name, true);
                    (
                        TaskStatus::Success,
                        Some(output.exit_code),
                        if output.from_cache {
                            Some(format!("cached:{}", output.task_id))
                        } else {
                            Some(output.task_id)
                        },
                    )
                } else {
                    cuenv_events::emit_ci_task_result!(&project_display, task_name, false);
                    pipeline_status = PipelineStatus::Failed;
                    (TaskStatus::Failed, Some(output.exit_code), None)
                }
            }
            Err(e) => {
                tracing::error!(error = %e, task = task_name, "Task execution error");
                cuenv_events::emit_ci_task_result!(&project_display, task_name, false);
                pipeline_status = PipelineStatus::Failed;
                (TaskStatus::Failed, None, None)
            }
        };

        tasks_reports.push(TaskReport {
            name: task_name.clone(),
            status,
            duration_ms: duration,
            exit_code,
            cache_key,
            inputs_matched,
            outputs,
        });
    }

    let completed_at = Utc::now();
    #[allow(clippy::cast_sign_loss)]
    let duration_ms = (completed_at - start_time).num_milliseconds() as u64;

    // Generate report
    let report = PipelineReport {
        version: cuenv_core::VERSION.to_string(),
        project: project_path.display().to_string(),
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
    write_pipeline_report(&report, context, project_path);
    notify_provider(provider, &report, pipeline_name).await;

    Ok(pipeline_status)
}

/// Write pipeline report to disk
fn write_pipeline_report(
    report: &PipelineReport,
    context: &crate::context::CIContext,
    project_path: &Path,
) {
    // Ensure report directory exists
    let report_dir = Path::new(".cuenv/reports");
    if let Err(e) = std::fs::create_dir_all(report_dir) {
        tracing::warn!(error = %e, "Failed to create report directory");
        return;
    }

    let sha_dir = report_dir.join(&context.sha);
    let _ = std::fs::create_dir_all(&sha_dir);

    let project_filename = project_path.display().to_string().replace(['/', '\\'], "-") + ".json";
    let report_path = sha_dir.join(project_filename);

    if let Err(e) = write_report(report, &report_path) {
        tracing::warn!(error = %e, "Failed to write report");
    } else {
        cuenv_events::emit_ci_report!(report_path.display());
    }

    // Write GitHub Job Summary
    if let Err(e) = crate::report::markdown::write_job_summary(report) {
        tracing::warn!(error = %e, "Failed to write job summary");
    }
}

/// Notify CI provider about pipeline results
async fn notify_provider(provider: &dyn CIProvider, report: &PipelineReport, pipeline_name: &str) {
    // Post results to CI provider
    let check_name = format!("cuenv: {pipeline_name}");
    match provider.create_check(&check_name).await {
        Ok(handle) => {
            if let Err(e) = provider.complete_check(&handle, report).await {
                tracing::warn!(error = %e, "Failed to complete check run");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create check run");
        }
    }

    // Post PR comment with report summary
    if let Err(e) = provider.upload_report(report).await {
        tracing::warn!(error = %e, "Failed to post PR comment");
    }
}

/// Check if this is a fork PR (should use readonly cache)
fn is_fork_pr(context: &crate::context::CIContext) -> bool {
    // Fork PRs typically have a different head repo than base repo
    // This is a simplified check - providers may need more sophisticated detection
    context.event == "pull_request" && context.ref_name.starts_with("refs/pull/")
}

/// Register common CI secret environment variables for redaction.
///
/// This ensures that secrets passed via CI provider (GitHub Actions, etc.)
/// are automatically redacted from task output.
fn register_ci_secrets() {
    // Common secret environment variable patterns
    const SECRET_PATTERNS: &[&str] = &[
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "ACTIONS_RUNTIME_TOKEN",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_CLIENT_SECRET",
        "GCP_SERVICE_ACCOUNT_KEY",
        "CACHIX_AUTH_TOKEN",
        "CODECOV_TOKEN",
        "CUE_REGISTRY_TOKEN",
        "VSCE_PAT",
        "NPM_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "PYPI_TOKEN",
        "DOCKER_PASSWORD",
        "CLOUDFLARE_API_TOKEN",
        "OP_SERVICE_ACCOUNT_TOKEN",
        "CUENV_SECRET_SALT",
        "CUENV_SECRET_SALT_PREV",
    ];

    for pattern in SECRET_PATTERNS {
        if let Ok(value) = std::env::var(pattern) {
            cuenv_events::register_secret(value);
        }
    }
}

/// Execute a single task by name using the existing project config
///
/// This bridges the gap between the task name-based execution in `run_ci`
/// and the IR-based execution in `CIExecutor`.
async fn execute_single_task_by_name(
    config: &Project,
    task_name: &str,
    project_root: &Path,
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
        secrets: BTreeMap::new(), // Secrets handled separately
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
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        // Phase task fields (not applicable for regular tasks)
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    };

    // Build environment
    let mut env: BTreeMap<String, String> = task
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
