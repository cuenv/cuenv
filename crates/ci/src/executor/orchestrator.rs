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
use crate::compiler::Compiler;
use crate::discovery::evaluate_module_from_cwd;
use crate::ir::CachePolicy;
use crate::provider::CIProvider;
use crate::report::json::write_report;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use chrono::Utc;
use cuenv_core::ci::AnnotationValue;
use cuenv_core::cue::discovery::find_ancestor_env_files;
use cuenv_core::lockfile::{LOCKFILE_NAME, LockedToolPlatform, Lockfile};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::captures::resolve_captures;
use cuenv_core::tasks::{TaskGraph, TaskIndex};
use cuenv_core::tools::{
    Platform, ResolvedTool, ResolvedToolActivationStep, ToolActivationResolveOptions, ToolExtract,
    ToolOptions, ToolRegistry, ToolSource, apply_resolved_tool_activation, resolve_tool_activation,
    validate_tool_activation,
};
use cuenv_core::{DryRun, Result};
use cuenv_hooks::{
    ExecutionStatus, HookExecutionConfig, HookExecutionState, StateManager, compute_instance_hash,
    execute_hooks,
};
use std::collections::{BTreeMap, HashMap, HashSet};
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
/// * `dry_run` - Whether to skip actual task execution
/// * `specific_pipeline` - If set, only run tasks from this pipeline
/// * `environment` - Optional environment override for secrets resolution
/// * `path_filter` - If set, only process projects under this path (relative to module root)
///
/// # Errors
/// Returns error if IO errors occur or tasks fail
#[allow(clippy::too_many_lines)]
pub async fn run_ci(
    provider: Arc<dyn CIProvider>,
    dry_run: DryRun,
    specific_pipeline: Option<String>,
    environment: Option<String>,
    path_filter: Option<&str>,
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
        tracing::info!("No cuenv projects discovered; skipping CI run.");
        return Ok(());
    }
    cuenv_events::emit_ci_projects_discovered!(project_count);

    // Collect projects with their configs
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = Project::try_from(instance)?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }

    // Filter projects by path if specified (and not the default ".")
    let projects: Vec<(PathBuf, Project)> = match path_filter {
        Some(filter) if filter != "." => {
            let filter_path = module.root.join(filter);
            projects
                .into_iter()
                .filter(|(path, _)| path.starts_with(&filter_path))
                .collect()
        }
        _ => projects,
    };

    if projects.is_empty() {
        tracing::info!(
            filter = path_filter.unwrap_or("."),
            "No projects under path filter; skipping"
        );
        return Ok(());
    }

    // Build project maps for cross-project dependency resolution and hook lookup.
    let mut project_map = HashMap::new();
    let mut project_configs = HashMap::new();
    for (path, config) in &projects {
        let name = config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), (path.clone(), config.clone()));
        }

        project_configs.insert(path.clone(), config.clone());
        if let Ok(canonical) = path.canonicalize() {
            project_configs.insert(canonical, config.clone());
        }
    }

    // Track failures with structured errors
    let mut failures: Vec<(String, cuenv_core::Error)> = Vec::new();

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

        let resolved_environment =
            resolve_environment(environment.as_deref(), pipeline.environment.as_deref());

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

        if !dry_run.is_dry_run() {
            let result = execute_project_pipeline(&PipelineExecutionRequest {
                project_path,
                config,
                pipeline_name: &pipeline_name,
                tasks_to_run: &tasks_to_run,
                environment: resolved_environment.as_deref(),
                context,
                changed_files: &changed_files,
                provider: provider.as_ref(),
                project_configs: &project_configs,
            })
            .await;

            match result {
                Err(e) => {
                    tracing::error!(error = %e, "Pipeline execution error");
                    let project_name = project_path.display().to_string();
                    failures.push((project_name, e));
                }
                Ok((status, task_errors)) => {
                    if status == PipelineStatus::Failed {
                        failures.extend(task_errors);
                    }
                }
            }
        }
    }

    if !failures.is_empty() {
        let details = failures
            .iter()
            .map(|(project, err)| format!("  [{project}]\n    {err}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        return Err(cuenv_core::Error::execution(format!(
            "CI pipeline failed:\n\n{details}"
        )));
    }

    Ok(())
}

/// All parameters needed to execute a project pipeline.
pub struct PipelineExecutionRequest<'a> {
    pub project_path: &'a Path,
    pub config: &'a Project,
    pub pipeline_name: &'a str,
    pub tasks_to_run: &'a [String],
    pub environment: Option<&'a str>,
    pub context: &'a crate::context::CIContext,
    pub changed_files: &'a [PathBuf],
    pub provider: &'a dyn CIProvider,
    pub project_configs: &'a HashMap<PathBuf, Project>,
}

/// Execute a project's pipeline and handle reporting
///
/// Returns the pipeline status and a list of task failures (project path, error).
#[allow(clippy::too_many_lines)] // Complex orchestration logic
async fn execute_project_pipeline(
    request: &PipelineExecutionRequest<'_>,
) -> Result<(PipelineStatus, Vec<(String, cuenv_core::Error)>)> {
    let project_path = request.project_path;
    let config = request.config;
    let pipeline_name = request.pipeline_name;
    let tasks_to_run = request.tasks_to_run;
    let environment = request.environment;
    let context = request.context;
    let changed_files = request.changed_files;
    let provider = request.provider;
    let project_configs = request.project_configs;

    let start_time = Utc::now();
    let mut tasks_reports = Vec::new();
    let mut pipeline_status = PipelineStatus::Success;
    let mut task_errors: Vec<(String, cuenv_core::Error)> = Vec::new();
    let project_display = project_path.display().to_string();
    // Collect resolved captures per task for annotation resolution
    let mut all_captures: HashMap<String, HashMap<String, String>> = HashMap::new();

    // Determine cache policy override based on context
    let cache_policy_override = if is_fork_pr(context) {
        Some(CachePolicy::Readonly)
    } else {
        None
    };

    // Create executor configuration with salt rotation support
    let mut executor_config = CIExecutorConfig::new(project_path.to_path_buf())
        .with_capture_output(cuenv_core::OutputCapture::Capture)
        .with_dry_run(DryRun::No)
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

    // Merge static + hook-generated environment once per project, then reuse for all tasks.
    let hook_env = build_hook_environment(project_path, config, project_configs).await?;

    // Build task index for resolving nested task names (e.g., "deploy.preview")
    let task_index = TaskIndex::build(&config.tasks)?;

    // Execute tasks
    for task_name in tasks_to_run {
        let inputs_matched =
            matched_inputs_for_task(task_name, config, changed_files, project_path);
        let outputs = task_index
            .resolve(task_name)
            .ok()
            .and_then(|indexed| indexed.node.as_task())
            .map(|task| task.outputs.clone())
            .unwrap_or_default();

        cuenv_events::emit_ci_task_executing!(&project_display, task_name);
        let task_start = std::time::Instant::now();

        // Execute the task with all dependencies (uses TaskGraph for proper ordering)
        let result = execute_task_with_deps(
            config,
            task_name,
            project_path,
            cache_policy_override,
            environment,
            &hook_env,
        )
        .await;

        let duration = u64::try_from(task_start.elapsed().as_millis()).unwrap_or(0);

        let (status, exit_code, cache_key, task_captures) = match result {
            Ok(output) => {
                // Resolve captures from task output
                let task_captures = task_index
                    .resolve(task_name)
                    .ok()
                    .and_then(|indexed| indexed.node.as_task())
                    .filter(|task| !task.captures.is_empty())
                    .map(|task| resolve_captures(&task.captures, &output.stdout, &output.stderr))
                    .unwrap_or_default();

                if !task_captures.is_empty() {
                    all_captures.insert(task_name.clone(), task_captures.clone());
                }

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
                        task_captures,
                    )
                } else {
                    cuenv_events::emit_ci_task_result!(&project_display, task_name, false);
                    pipeline_status = PipelineStatus::Failed;
                    // Capture task failure with structured error
                    task_errors.push((
                        project_display.clone(),
                        cuenv_core::Error::task_failed(
                            task_name,
                            output.exit_code,
                            &output.stdout,
                            &output.stderr,
                        ),
                    ));
                    (
                        TaskStatus::Failed,
                        Some(output.exit_code),
                        None,
                        task_captures,
                    )
                }
            }
            Err(e) => {
                tracing::error!(error = %e, task = task_name, "Task execution error");
                cuenv_events::emit_ci_task_result!(&project_display, task_name, false);
                pipeline_status = PipelineStatus::Failed;
                // Capture execution error with structured error
                task_errors.push((project_display.clone(), e.into()));
                (TaskStatus::Failed, None, None, HashMap::new())
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
            captures: task_captures,
        });
    }

    let completed_at = Utc::now();
    #[allow(clippy::cast_sign_loss)]
    let duration_ms = (completed_at - start_time).num_milliseconds() as u64;

    // Resolve pipeline annotations from capture refs
    let Some(ci) = &config.ci else {
        unreachable!("CI config already validated above");
    };
    let resolved_annotations = ci
        .pipelines
        .get(pipeline_name)
        .map(|p| resolve_annotations(&p.annotations, &all_captures))
        .unwrap_or_default();

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
        annotations: resolved_annotations,
    };

    // Write reports and notify provider
    write_pipeline_report(&report, context, project_path);
    notify_provider(provider, &report, pipeline_name).await;

    Ok((pipeline_status, task_errors))
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

/// Resolve pipeline annotation values from capture refs and literals.
fn resolve_annotations(
    annotations: &HashMap<String, AnnotationValue>,
    all_captures: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, String> {
    annotations
        .iter()
        .filter_map(|(label, value)| {
            let resolved = match value {
                AnnotationValue::Literal(s) => Some(s.clone()),
                AnnotationValue::CaptureRef {
                    cuenv_capture_ref,
                    cuenv_task,
                    cuenv_capture,
                } => {
                    if !cuenv_capture_ref {
                        tracing::warn!(label, "Annotation has cuenvCaptureRef=false, skipping");
                        return None;
                    }
                    all_captures
                        .get(cuenv_task.as_str())
                        .and_then(|caps| caps.get(cuenv_capture.as_str()))
                        .cloned()
                }
            };
            resolved.map(|v| (label.clone(), v))
        })
        .collect()
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

fn resolve_environment(
    cli_environment: Option<&str>,
    pipeline_environment: Option<&str>,
) -> Option<String> {
    if let Some(env) = cli_environment.filter(|name| !name.is_empty()) {
        return Some(env.to_string());
    }

    if let Ok(env) = std::env::var("CUENV_ENVIRONMENT")
        && !env.is_empty()
    {
        return Some(env);
    }

    pipeline_environment
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
}

/// Build the project environment by merging static env with hook-generated values.
async fn build_hook_environment(
    project_root: &Path,
    config: &Project,
    project_configs: &HashMap<PathBuf, Project>,
) -> Result<BTreeMap<String, String>> {
    let static_env = extract_static_env_vars(config);
    let hooks = collect_hooks_from_ancestors(project_root, config, project_configs)?;

    if hooks.is_empty() {
        return Ok(static_env);
    }

    let config_hash = cuenv_hooks::compute_execution_hash(&hooks, project_root);
    let instance_hash = compute_instance_hash(project_root, &config_hash);

    let state_dir = if let Ok(dir) = std::env::var("CUENV_STATE_DIR") {
        PathBuf::from(dir)
    } else {
        StateManager::default_state_dir()?
    };
    let state_manager = StateManager::new(state_dir);

    let hook_config = HookExecutionConfig {
        default_timeout_seconds: 600,
        fail_fast: true,
        state_dir: None,
    };

    let mut state = HookExecutionState::new(
        project_root.to_path_buf(),
        instance_hash,
        config_hash,
        hooks.clone(),
    );

    execute_hooks(
        hooks,
        project_root,
        &hook_config,
        &state_manager,
        &mut state,
    )
    .await?;

    match state.status {
        ExecutionStatus::Completed | ExecutionStatus::Failed => {
            Ok(collect_all_env_vars(config, &state.environment_vars))
        }
        ExecutionStatus::Running | ExecutionStatus::Cancelled => Ok(static_env),
    }
}

/// Collect `onEnter` hooks from ancestor env.cue files (root-to-leaf order).
fn collect_hooks_from_ancestors(
    project_root: &Path,
    config: &Project,
    project_configs: &HashMap<PathBuf, Project>,
) -> Result<Vec<cuenv_hooks::Hook>> {
    let ancestors = find_ancestor_env_files(project_root, "cuenv")?;
    let ancestors_len = ancestors.len();
    let mut all_hooks = Vec::new();

    for (idx, ancestor_dir) in ancestors.into_iter().enumerate() {
        let is_current_dir = idx + 1 == ancestors_len;
        let source_config = if is_current_dir {
            Some(config)
        } else {
            project_configs.get(&ancestor_dir).or_else(|| {
                ancestor_dir
                    .canonicalize()
                    .ok()
                    .and_then(|canonical| project_configs.get(&canonical))
            })
        };

        let Some(source_config) = source_config else {
            continue;
        };

        let mut hooks = source_config.on_enter_hooks();
        for hook in &mut hooks {
            resolve_hook_dir(hook, &ancestor_dir);
        }

        // Ancestor hooks only run when propagate=true.
        if !is_current_dir {
            hooks.retain(|hook| hook.propagate);
        }

        all_hooks.extend(hooks);
    }

    Ok(all_hooks)
}

/// Resolve hook.dir relative to the env.cue directory where the hook is defined.
fn resolve_hook_dir(hook: &mut cuenv_hooks::Hook, env_cue_dir: &Path) {
    let relative_dir = hook.dir.as_deref().unwrap_or(".");
    let absolute_dir = env_cue_dir.join(relative_dir);
    let resolved = absolute_dir.canonicalize().unwrap_or(absolute_dir);
    hook.dir = Some(resolved.to_string_lossy().to_string());
}

/// Extract static (non-secret) environment variables from config.
fn extract_static_env_vars(config: &Project) -> BTreeMap<String, String> {
    let mut env_vars = BTreeMap::new();
    if let Some(env) = &config.env {
        for (key, value) in &env.base {
            if value.is_secret() {
                continue;
            }
            env_vars.insert(key.clone(), value.to_string_value());
        }
    }
    env_vars
}

/// Merge static config env vars with hook-generated values (hooks win).
fn collect_all_env_vars(
    config: &Project,
    hook_env: &std::collections::HashMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = extract_static_env_vars(config);
    for (key, value) in hook_env {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Execute a task with all its dependencies in correct order.
///
/// Uses TaskIndex to flatten nested tasks and TaskGraph to resolve dependencies,
/// ensuring tasks run in proper topological order (same as CLI).
async fn execute_task_with_deps(
    config: &Project,
    task_name: &str,
    project_root: &Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&str>,
    hook_env: &BTreeMap<String, String>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    // 1. Build TaskIndex (same flattening as CLI)
    let index =
        TaskIndex::build(&config.tasks).map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    // 2. Resolve to canonical name
    let entry = index
        .resolve(task_name)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;
    let canonical_name = entry.name.clone();

    // 3. Get flattened tasks where all names are top-level
    let flattened_tasks = index.to_tasks();

    // 4. Build TaskGraph (respects dependsOn!)
    let mut graph = TaskGraph::new();
    graph
        .build_for_task(&canonical_name, &flattened_tasks)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    // 5. Get topological execution order
    let execution_order = graph
        .topological_sort()
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    tracing::info!(
        task = task_name,
        canonical = %canonical_name,
        execution_order = ?execution_order.iter().map(|n| &n.name).collect::<Vec<_>>(),
        "Resolved task dependencies"
    );

    // 6. Execute each task in dependency order
    let mut final_output = None;
    for node in execution_order {
        let output = compile_and_execute_ir(
            config,
            &node.name,
            project_root,
            cache_policy_override,
            environment,
            hook_env,
        )
        .await?;

        if !output.success {
            return Ok(output); // Stop on first failure
        }
        final_output = Some(output);
    }

    final_output.ok_or_else(|| ExecutorError::Compilation("No tasks to execute".into()))
}

/// Compile a single task to IR and execute it.
///
/// This is the inner execution loop - it does NOT handle dependencies.
/// Dependencies are resolved by the outer loop using TaskGraph.
/// Uses the Compiler to convert task definitions to IR.
async fn compile_and_execute_ir(
    config: &Project,
    task_name: &str,
    project_root: &Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&str>,
    hook_env: &BTreeMap<String, String>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let start = std::time::Instant::now();

    // Use the Compiler to compile the task (handles both single tasks and groups)
    let options = crate::compiler::CompilerOptions {
        project_root: Some(project_root.to_path_buf()),
        ..Default::default()
    };
    let compiler = Compiler::with_options(config.clone(), options);
    let ir = compiler
        .compile_task(task_name)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    if ir.tasks.is_empty() {
        return Err(ExecutorError::Compilation(format!(
            "Task '{task_name}' produced no executable tasks"
        )));
    }

    // Resolve secrets from project environment (same as CLI).
    // Prefer an explicit environment name, then fall back to CUENV_ENVIRONMENT.
    let env_name = environment
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CUENV_ENVIRONMENT")
                .ok()
                .filter(|name| !name.is_empty())
        });
    let project_env_vars = config
        .env
        .as_ref()
        .map(|env| match env_name.as_deref() {
            Some(name) => env.for_environment(name),
            None => env.base.clone(),
        })
        .unwrap_or_default();
    let (resolved_env, secrets) =
        cuenv_core::environment::Environment::resolve_for_task_with_secrets(
            task_name,
            &project_env_vars,
        )
        .await
        .map_err(|e| ExecutorError::Compilation(format!("Secret resolution failed: {e}")))?;

    // Register resolved secrets for redaction
    cuenv_events::register_secrets(secrets.into_iter());

    // Execute all compiled IR tasks sequentially
    let runner = IRTaskRunner::new(
        project_root.to_path_buf(),
        cuenv_core::OutputCapture::Capture,
    );
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut all_success = true;
    let mut last_exit_code = 0;

    // Ensure tools are downloaded before getting activation paths.
    // Tool activation failures are fatal.
    ensure_tools_downloaded(project_root).await?;
    let activation_steps = resolve_tool_activation_steps(project_root)?;
    if !activation_steps.is_empty() {
        tracing::debug!(
            steps = activation_steps.len(),
            "Applying configured tool activation operations for CI task execution"
        );
    }

    for ir_task in &ir.tasks {
        // Build environment with explicit precedence:
        // task env > resolved project env > hook/static env.
        let mut env: BTreeMap<String, String> = hook_env.clone();
        for (key, value) in &resolved_env {
            env.insert(key.clone(), value.clone());
        }
        for (key, value) in &ir_task.env {
            env.insert(key.clone(), value.clone());
        }

        for step in &activation_steps {
            let current = env.get(&step.var).map(String::as_str);
            if let Some(new_value) = apply_resolved_tool_activation(current, step) {
                env.insert(step.var.clone(), new_value);
            }
        }

        if !env.contains_key("HOME")
            && let Ok(home) = std::env::var("HOME")
        {
            env.insert("HOME".to_string(), home);
        }

        // Apply cache policy override if specified
        let mut task_to_run = ir_task.clone();
        if let Some(policy) = cache_policy_override {
            task_to_run.cache_policy = policy;
        }

        let output = runner.execute(&task_to_run, env).await?;

        combined_stdout.push_str(&output.stdout);
        combined_stderr.push_str(&output.stderr);
        last_exit_code = output.exit_code;

        if !output.success {
            all_success = false;
            break; // Stop on first failure
        }
    }

    let duration = start.elapsed();
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);

    Ok(TaskOutput {
        task_id: task_name.to_string(),
        exit_code: last_exit_code,
        stdout: combined_stdout,
        stderr: combined_stderr,
        success: all_success,
        from_cache: false,
        duration_ms,
        captures: HashMap::new(),
    })
}

// ============================================================================
// Tool Activation Helpers
// ============================================================================

/// Find the lockfile starting from a directory.
fn find_lockfile(start_dir: &Path) -> Option<PathBuf> {
    let lockfile_path = start_dir.join(LOCKFILE_NAME);
    if lockfile_path.exists() {
        return Some(lockfile_path);
    }

    // Check parent directories
    let mut current = start_dir.parent();
    while let Some(dir) = current {
        let lockfile_path = dir.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }
        current = dir.parent();
    }

    None
}

/// Create a tool registry with all available providers.
fn create_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(cuenv_tools_nix::NixToolProvider::new());
    registry.register(cuenv_tools_github::GitHubToolProvider::new());
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());
    registry.register(cuenv_tools_url::UrlToolProvider::new());

    registry
}

/// Convert a lockfile entry to a `ToolSource`.
fn lockfile_entry_to_source(locked: &LockedToolPlatform) -> Option<ToolSource> {
    match locked.provider.as_str() {
        "oci" => {
            let image = locked
                .source
                .get("image")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let path = locked
                .source
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Some(ToolSource::Oci {
                image: image.to_string(),
                path: path.to_string(),
            })
        }
        "github" => {
            let repo = locked
                .source
                .get("repo")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let tag = locked
                .source
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let asset = locked
                .source
                .get("asset")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::GitHub {
                repo: repo.to_string(),
                tag: tag.to_string(),
                asset: asset.to_string(),
                extract,
            })
        }
        "nix" => {
            let flake = locked
                .source
                .get("flake")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let package = locked
                .source
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let output = locked
                .source
                .get("output")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(ToolSource::Nix {
                flake: flake.to_string(),
                package: package.to_string(),
                output,
            })
        }
        "rustup" => {
            let toolchain = locked
                .source
                .get("toolchain")
                .and_then(|v| v.as_str())
                .unwrap_or("stable");
            let profile = locked
                .source
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let components: Vec<String> = locked
                .source
                .get("components")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let targets: Vec<String> = locked
                .source
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(ToolSource::Rustup {
                toolchain: toolchain.to_string(),
                profile,
                components,
                targets,
            })
        }
        "url" => {
            let url = locked
                .source
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::Url {
                url: url.to_string(),
                extract,
            })
        }
        _ => None,
    }
}

fn parse_github_extract_list(source: &serde_json::Value) -> Vec<ToolExtract> {
    let mut extract = source
        .get("extract")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<ToolExtract>>(value).ok())
        .unwrap_or_default();

    if extract.is_empty()
        && let Some(path) = source.get("path").and_then(|v| v.as_str())
    {
        if path_looks_like_library(path) {
            extract.push(ToolExtract::Lib {
                path: path.to_string(),
                env: None,
            });
        } else {
            extract.push(ToolExtract::Bin {
                path: path.to_string(),
                as_name: None,
            });
        }
    }

    extract
}

fn path_looks_like_library(path: &str) -> bool {
    let ext_is = |target: &str| {
        std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case(target))
    };
    ext_is("dylib") || ext_is("so") || path.to_ascii_lowercase().contains(".so.") || ext_is("dll")
}

/// Resolve activation steps from lockfile for CI execution.
fn resolve_tool_activation_steps(
    project_root: &Path,
) -> std::result::Result<Vec<ResolvedToolActivationStep>, ExecutorError> {
    let Some(lockfile_path) = find_lockfile(project_root) else {
        return Ok(Vec::new());
    };

    let lockfile = match Lockfile::load(&lockfile_path) {
        Ok(Some(lf)) => lf,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => {
            return Err(ExecutorError::Compilation(format!(
                "Failed to load lockfile: {e}"
            )));
        }
    };

    let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    resolve_tool_activation(&options).map_err(|e| {
        ExecutorError::Compilation(format!("Invalid tool activation configuration: {e}"))
    })
}

/// Ensure all tools from the lockfile are downloaded for the current platform.
async fn ensure_tools_downloaded(project_root: &Path) -> std::result::Result<(), ExecutorError> {
    let Some(lockfile_path) = find_lockfile(project_root) else {
        tracing::debug!("No lockfile found - skipping tool download");
        return Ok(());
    };

    let lockfile = match Lockfile::load(&lockfile_path) {
        Ok(Some(lf)) => lf,
        Ok(None) => {
            tracing::debug!("Empty lockfile - skipping tool download");
            return Ok(());
        }
        Err(e) => {
            return Err(ExecutorError::Compilation(format!(
                "Failed to load lockfile: {e}"
            )));
        }
    };

    if lockfile.tools.is_empty() {
        tracing::debug!("No tools in lockfile - skipping download");
        return Ok(());
    }

    let activation_options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    validate_tool_activation(&activation_options).map_err(|e| {
        ExecutorError::Compilation(format!("Invalid tool activation configuration: {e}"))
    })?;

    let platform = Platform::current();
    let platform_str = platform.to_string();
    let options = ToolOptions::default();
    let registry = create_tool_registry();

    // Check prerequisites for all providers we'll use
    let mut providers_used = HashSet::new();
    for tool in lockfile.tools.values() {
        if let Some(locked) = tool.platforms.get(&platform_str) {
            providers_used.insert(locked.provider.clone());
        }
    }

    for provider_name in &providers_used {
        if let Some(provider) = registry.get(provider_name)
            && let Err(e) = provider.check_prerequisites().await
        {
            tracing::warn!(
                "Provider '{}' prerequisites check failed: {} - skipping tools from this provider",
                provider_name,
                e
            );
        }
    }

    // Download tools that aren't cached
    let mut errors: Vec<String> = Vec::new();

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            continue;
        };

        let Some(source) = lockfile_entry_to_source(locked) else {
            tracing::debug!(
                "Unknown provider '{}' for tool '{}' - skipping",
                locked.provider,
                name
            );
            continue;
        };

        let Some(provider) = registry.find_for_source(&source) else {
            tracing::debug!("No provider found for tool '{}' - skipping", name);
            continue;
        };

        let resolved = ResolvedTool {
            name: name.clone(),
            version: tool.version.clone(),
            platform: platform.clone(),
            source,
        };

        // Check if already cached
        if provider.is_cached(&resolved, &options) {
            continue;
        }

        // Fetch the tool
        tracing::info!("Downloading {} v{}...", name, tool.version);
        match provider.fetch(&resolved, &options).await {
            Ok(fetched) => {
                tracing::info!("Downloaded {} -> {}", name, fetched.binary_path.display());
            }
            Err(e) => {
                tracing::warn!("Failed to download tool '{}': {}", name, e);
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    if !errors.is_empty() {
        return Err(ExecutorError::Compilation(format!(
            "Failed to download tools: {}",
            errors.join(", ")
        )));
    }

    Ok(())
}
