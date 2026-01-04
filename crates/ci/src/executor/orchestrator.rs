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
use cuenv_core::Result;
use cuenv_core::lockfile::{LOCKFILE_NAME, LockedToolPlatform, Lockfile};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{TaskGraph, TaskIndex};
use cuenv_core::tools::{Platform, ResolvedTool, ToolOptions, ToolRegistry, ToolSource};
use std::collections::{BTreeMap, HashSet};
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
/// * `environment` - Optional environment override for secrets resolution
/// * `path_filter` - If set, only process projects under this path (relative to module root)
///
/// # Errors
/// Returns error if IO errors occur or tasks fail
#[allow(clippy::too_many_lines)]
pub async fn run_ci(
    provider: Arc<dyn CIProvider>,
    dry_run: bool,
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
        return Err(cuenv_core::Error::configuration(format!(
            "No cuenv projects found under path '{}'",
            path_filter.unwrap_or(".")
        )));
    }

    // Build project map for cross-project dependency resolution
    let mut project_map = std::collections::HashMap::new();
    for (path, config) in &projects {
        let name = config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), (path.clone(), config.clone()));
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

        if !dry_run {
            let result = execute_project_pipeline(
                project_path,
                config,
                &pipeline_name,
                &tasks_to_run,
                resolved_environment.as_deref(),
                context,
                &changed_files,
                provider.as_ref(),
            )
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

/// Execute a project's pipeline and handle reporting
///
/// Returns the pipeline status and a list of task failures (project path, error).
#[allow(clippy::too_many_arguments)] // Pipeline execution requires many context params
#[allow(clippy::too_many_lines)] // Complex orchestration logic
async fn execute_project_pipeline(
    project_path: &Path,
    config: &Project,
    pipeline_name: &str,
    tasks_to_run: &[String],
    environment: Option<&str>,
    context: &crate::context::CIContext,
    changed_files: &[PathBuf],
    provider: &dyn CIProvider,
) -> Result<(PipelineStatus, Vec<(String, cuenv_core::Error)>)> {
    let start_time = Utc::now();
    let mut tasks_reports = Vec::new();
    let mut pipeline_status = PipelineStatus::Success;
    let mut task_errors: Vec<(String, cuenv_core::Error)> = Vec::new();
    let project_display = project_path.display().to_string();

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

        cuenv_events::emit_ci_task_executing!(&project_display, task_name);
        let task_start = std::time::Instant::now();

        // Execute the task with all dependencies (uses TaskGraph for proper ordering)
        let result = execute_task_with_deps(
            config,
            task_name,
            project_path,
            cache_policy_override,
            environment,
        )
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
                    (TaskStatus::Failed, Some(output.exit_code), None)
                }
            }
            Err(e) => {
                tracing::error!(error = %e, task = task_name, "Task execution error");
                cuenv_events::emit_ci_task_result!(&project_display, task_name, false);
                pipeline_status = PipelineStatus::Failed;
                // Capture execution error with structured error
                task_errors.push((project_display.clone(), e.into()));
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
    let runner = IRTaskRunner::new(project_root.to_path_buf(), true);
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut all_success = true;
    let mut last_exit_code = 0;

    // Ensure tools are downloaded before getting their paths
    ensure_tools_downloaded(project_root).await;

    // Get tool bin directories from lockfile
    let tool_bin_dirs = get_tool_bin_dirs(project_root);
    let tool_path_prepend = if tool_bin_dirs.is_empty() {
        String::new()
    } else {
        tool_bin_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    };

    for ir_task in &ir.tasks {
        // Build environment: start with IR task env (task-specific vars)
        let mut env: BTreeMap<String, String> = ir_task.env.clone();

        // Merge project-level resolved environment (includes secrets)
        // Project env is lower priority - task-specific overrides win
        for (key, value) in &resolved_env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }

        // Add PATH with tool directories prepended, then HOME
        if let Ok(system_path) = std::env::var("PATH") {
            let path = if tool_path_prepend.is_empty() {
                system_path
            } else {
                format!("{tool_path_prepend}:{system_path}")
            };
            env.insert("PATH".to_string(), path);
        } else if !tool_path_prepend.is_empty() {
            env.insert("PATH".to_string(), tool_path_prepend.clone());
        }
        if let Ok(home) = std::env::var("HOME") {
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
    })
}

// ============================================================================
// Tool Activation Helpers
//
// These functions match the CLI's implementation in commands/tools.rs.
// They are duplicated here because cuenv-core cannot depend on tool providers
// (it would create a cyclic dependency). A future refactor could extract these
// into a dedicated cuenv-tools-activation crate that both CLI and CI use.
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

    registry
}

/// Convert a lockfile entry to a `ToolSource`.
#[allow(dead_code)]
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
            let path = locked
                .source
                .get("path")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(ToolSource::GitHub {
                repo: repo.to_string(),
                tag: tag.to_string(),
                asset: asset.to_string(),
                path,
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
        _ => None,
    }
}

/// Get tool bin directories from the lockfile for PATH injection.
fn get_tool_bin_dirs(project_root: &Path) -> Vec<PathBuf> {
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();

    let Some(lockfile_path) = find_lockfile(project_root) else {
        return vec![];
    };

    let Ok(Some(lockfile)) = Lockfile::load(&lockfile_path) else {
        return vec![];
    };

    if lockfile.tools.is_empty() {
        return vec![];
    }

    let platform = Platform::current();
    let platform_str = platform.to_string();
    let cache_dir = ToolOptions::default().cache_dir();

    // 1. Check for Nix profile
    let lockfile_dir = lockfile_path.parent().unwrap_or(Path::new("."));
    if let Ok(profile_path) = cuenv_tools_nix::profile::profile_path_for_project(lockfile_dir) {
        let bin = profile_path.join("bin");
        if bin.exists() {
            bin_dirs.insert(bin);
        }
    }

    // 2. Process non-Nix tools
    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            continue;
        };

        // Skip Nix tools - they use profile
        if locked.provider == "nix" {
            continue;
        }

        // Handle rustup tools
        if locked.provider == "rustup" {
            if let Some(toolchain) = locked.source.get("toolchain").and_then(|v| v.as_str()) {
                let rustup_home = std::env::var("RUSTUP_HOME").map_or_else(
                    |_| {
                        dirs::home_dir()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join(".rustup")
                    },
                    PathBuf::from,
                );

                let host_triple = format!(
                    "{}-{}",
                    match platform.arch {
                        cuenv_core::tools::Arch::Arm64 => "aarch64",
                        cuenv_core::tools::Arch::X86_64 => "x86_64",
                    },
                    match platform.os {
                        cuenv_core::tools::Os::Darwin => "apple-darwin",
                        cuenv_core::tools::Os::Linux => "unknown-linux-gnu",
                    }
                );
                let toolchain_name = format!("{toolchain}-{host_triple}");
                let bin = rustup_home
                    .join("toolchains")
                    .join(toolchain_name)
                    .join("bin");
                if bin.exists() {
                    bin_dirs.insert(bin);
                }
            }
            continue;
        }

        // GitHub and other cache-based tools
        let tool_dir = cache_dir
            .join(&locked.provider)
            .join(name)
            .join(&tool.version);

        if tool_dir.exists() {
            // Check if tool_dir itself contains the binary (flat structure)
            if tool_dir.join(name).exists() || tool_dir.join(format!("{name}.exe")).exists() {
                bin_dirs.insert(tool_dir.clone());
            }
            // Also check bin subdirectory
            let bin = tool_dir.join("bin");
            if bin.exists() {
                bin_dirs.insert(bin);
            }
        }
    }

    bin_dirs.into_iter().collect()
}

/// Ensure all tools from the lockfile are downloaded for the current platform.
async fn ensure_tools_downloaded(project_root: &Path) {
    let Some(lockfile_path) = find_lockfile(project_root) else {
        tracing::debug!("No lockfile found - skipping tool download");
        return;
    };

    let lockfile = match Lockfile::load(&lockfile_path) {
        Ok(Some(lf)) => lf,
        Ok(None) => {
            tracing::debug!("Empty lockfile - skipping tool download");
            return;
        }
        Err(e) => {
            tracing::warn!("Failed to load lockfile: {e}");
            return;
        }
    };

    if lockfile.tools.is_empty() {
        tracing::debug!("No tools in lockfile - skipping download");
        return;
    }

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
            }
        }
    }
}
