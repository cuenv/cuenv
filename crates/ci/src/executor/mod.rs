//! CI Pipeline Executor
//!
//! Executes CI pipelines with proper dependency ordering, caching,
//! and parallel execution.

// CI executor outputs to stdout/stderr as part of its normal operation
#![allow(clippy::print_stdout, clippy::print_stderr)]

pub mod backend;
pub mod cache;
pub mod config;
pub mod graph;
pub mod lock;
pub mod metrics;
pub mod redact;
pub mod remote;
pub mod runner;
pub mod secrets;

pub use backend::{BackendError, BackendResult, CacheBackend, CacheEntry, CacheLookupResult, CacheOutput};
pub use cache::LocalCacheBackend;
pub use config::CIExecutorConfig;
pub use lock::{ConcurrencyLock, LockConfig, LockError, LockGuard};
pub use metrics::{CacheMetrics, RestoreErrorType, global_metrics};
pub use redact::{LogRedactor, ShortSecretWarning, redact_secrets};
pub use remote::{RemoteCacheBackend, RemoteCacheConfig};
pub use runner::TaskOutput;
pub use secrets::SaltConfig;

use crate::affected::{compute_affected_tasks, matched_inputs_for_task};
use crate::compiler::Compiler;
use crate::discovery::discover_projects;
use crate::ir::{CachePolicy, IntermediateRepresentation};
use crate::provider::CIProvider;
use crate::report::json::write_report;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use cache::TaskLogs;
use chrono::Utc;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use graph::{CITaskGraph, CITaskNode};
use runner::IRTaskRunner;
use secrets::ResolvedSecrets;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::task::JoinSet;

/// Error types for CI execution
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// Compilation error
    #[error("Failed to compile project to IR: {0}")]
    Compilation(String),

    /// Task graph error
    #[error(transparent)]
    Graph(#[from] graph::GraphError),

    /// Secret resolution error
    #[error(transparent)]
    Secret(#[from] secrets::SecretError),

    /// Cache error
    #[error(transparent)]
    Cache(#[from] cache::CacheError),

    /// Task execution error
    #[error(transparent)]
    Runner(#[from] runner::RunnerError),

    /// Task panicked during execution
    #[error("Task panicked: {0}")]
    TaskPanic(String),

    /// Pipeline not found
    #[error("Pipeline '{name}' not found. Available: {available}")]
    PipelineNotFound { name: String, available: String },

    /// No CI configuration
    #[error("Project has no CI configuration")]
    NoCIConfig,
}

/// Result of pipeline execution
#[derive(Debug)]
pub struct PipelineResult {
    /// Whether all tasks succeeded
    pub success: bool,
    /// Results for each task
    pub tasks: Vec<TaskOutput>,
    /// Total execution time in milliseconds
    pub duration_ms: u64,
}

/// CI Pipeline Executor
///
/// Executes CI pipelines with:
/// - IR compilation and validation
/// - Dependency-ordered parallel execution
/// - Content-addressable caching
/// - Secret resolution and injection
pub struct CIExecutor {
    config: CIExecutorConfig,
}

impl CIExecutor {
    /// Create a new executor with the given configuration
    #[must_use]
    pub fn new(config: CIExecutorConfig) -> Self {
        Self { config }
    }

    /// Execute a pipeline from a project configuration
    ///
    /// # Arguments
    /// * `project` - The project configuration
    /// * `pipeline_name` - Optional specific pipeline (defaults to "default")
    ///
    /// # Errors
    /// Returns error if compilation fails, tasks fail, or secrets can't be resolved
    #[tracing::instrument(
        name = "ci_execute_pipeline",
        fields(project_root = %self.config.project_root.display()),
        skip(self, project)
    )]
    pub async fn execute_pipeline(
        &self,
        project: &Project,
        pipeline_name: Option<&str>,
    ) -> std::result::Result<PipelineResult, ExecutorError> {
        let start = std::time::Instant::now();

        // Step 1: Compile to IR
        tracing::info!("Compiling project to IR");
        let compiler = Compiler::new(project.clone());
        let ir = compiler
            .compile()
            .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

        tracing::info!(task_count = ir.tasks.len(), "IR compilation complete");

        // Step 2: Build task graph
        tracing::info!("Building task graph");
        let mut task_graph = CITaskGraph::from_ir(&ir)?;

        // Step 3: Resolve secrets for all tasks
        tracing::info!("Resolving secrets");
        let all_secrets = self.resolve_all_secrets(&ir)?;
        let fingerprints = self.extract_fingerprints(&all_secrets);

        // Step 4: Compute digests with secret fingerprints
        tracing::info!("Computing task digests");
        task_graph.compute_digests(&ir, &fingerprints, self.config.secret_salt.as_deref());

        // Step 5: Get parallel groups for execution
        let parallel_groups = task_graph.get_parallel_groups()?;
        tracing::info!(groups = parallel_groups.len(), "Execution groups computed");

        // Step 6: Execute groups
        let cache_root = self.config.effective_cache_root();
        let mut all_results = Vec::new();
        let mut pipeline_success = true;

        for (group_idx, group) in parallel_groups.iter().enumerate() {
            tracing::info!(
                group = group_idx,
                tasks = group.len(),
                "Executing task group"
            );

            let group_results = self
                .execute_group(group, &ir, &cache_root, &all_secrets)
                .await?;

            // Check for failures
            for result in &group_results {
                if !result.success {
                    tracing::warn!(task = %result.task_id, "Task failed");
                    pipeline_success = false;
                }
            }

            all_results.extend(group_results);

            // Fail fast: stop if any task in the group failed
            if !pipeline_success {
                tracing::warn!("Pipeline failed, aborting remaining groups");
                break;
            }
        }

        let duration = start.elapsed();

        Ok(PipelineResult {
            success: pipeline_success,
            tasks: all_results,
            duration_ms: duration.as_millis() as u64,
        })
    }

    /// Execute a single group of tasks (can run in parallel)
    async fn execute_group(
        &self,
        group: &[&CITaskNode],
        ir: &IntermediateRepresentation,
        cache_root: &std::path::Path,
        all_secrets: &HashMap<String, ResolvedSecrets>,
    ) -> std::result::Result<Vec<TaskOutput>, ExecutorError> {
        let mut results = Vec::new();

        if self.config.max_parallel <= 1 || group.len() == 1 {
            // Sequential execution
            for node in group {
                let result = self
                    .execute_single_task(node, ir, cache_root, all_secrets)
                    .await?;
                results.push(result);
            }
        } else {
            // Parallel execution with JoinSet
            let mut join_set = JoinSet::new();

            for node in group {
                // Check cache first
                let cache_result = cache::check_cache(
                    &node.task,
                    &node.digest,
                    cache_root,
                    self.config.cache_policy_override,
                );

                if cache_result.hit {
                    tracing::info!(task = %node.id, "Cache hit, skipping execution");
                    results.push(TaskOutput::from_cache(node.id.clone(), 0));
                    continue;
                }

                if self.config.dry_run {
                    tracing::info!(task = %node.id, "Would execute (dry-run)");
                    results.push(TaskOutput::dry_run(node.id.clone()));
                    continue;
                }

                // Prepare execution context
                let task = node.task.clone();
                let digest = node.digest.clone();
                let project_root = self.config.project_root.clone();
                let capture = self.config.capture_output;
                let cache_root_owned = cache_root.to_path_buf();
                let policy_override = self.config.cache_policy_override;

                // Build environment with secrets
                let mut env = task.env.clone();
                if let Some(resolved) = all_secrets.get(&task.id) {
                    for (name, value) in &resolved.values {
                        env.insert(name.clone(), value.clone());
                    }
                }

                // Spawn task execution
                join_set.spawn(async move {
                    let runner = IRTaskRunner::new(project_root, capture);
                    let result = runner.execute(&task, env).await;
                    (task, digest, cache_root_owned, policy_override, result)
                });
            }

            // Collect results
            while let Some(join_result) = join_set.join_next().await {
                let (task, digest, cache_root_owned, policy_override, exec_result) =
                    join_result.map_err(|e| ExecutorError::TaskPanic(e.to_string()))?;

                let output = exec_result?;

                // Store in cache if successful
                if output.success {
                    cache::store_result(
                        &task,
                        &digest,
                        &cache_root_owned,
                        &TaskLogs {
                            stdout: Some(output.stdout.clone()),
                            stderr: Some(output.stderr.clone()),
                        },
                        output.duration_ms,
                        output.exit_code,
                        policy_override,
                    )?;
                }

                results.push(output);
            }
        }

        Ok(results)
    }

    /// Execute a single task with cache checking
    async fn execute_single_task(
        &self,
        node: &CITaskNode,
        _ir: &IntermediateRepresentation,
        cache_root: &std::path::Path,
        all_secrets: &HashMap<String, ResolvedSecrets>,
    ) -> std::result::Result<TaskOutput, ExecutorError> {
        // Check cache
        let cache_result = cache::check_cache(
            &node.task,
            &node.digest,
            cache_root,
            self.config.cache_policy_override,
        );

        if cache_result.hit {
            tracing::info!(task = %node.id, "Cache hit, skipping execution");
            return Ok(TaskOutput::from_cache(node.id.clone(), 0));
        }

        if self.config.dry_run {
            tracing::info!(task = %node.id, "Would execute (dry-run)");
            return Ok(TaskOutput::dry_run(node.id.clone()));
        }

        // Build environment with secrets
        let mut env = node.task.env.clone();
        if let Some(resolved) = all_secrets.get(&node.id) {
            for (name, value) in &resolved.values {
                env.insert(name.clone(), value.clone());
            }
        }

        // Execute
        let runner =
            IRTaskRunner::new(self.config.project_root.clone(), self.config.capture_output);
        let output = runner.execute(&node.task, env).await?;

        // Store in cache if successful
        if output.success {
            cache::store_result(
                &node.task,
                &node.digest,
                cache_root,
                &TaskLogs {
                    stdout: Some(output.stdout.clone()),
                    stderr: Some(output.stderr.clone()),
                },
                output.duration_ms,
                output.exit_code,
                self.config.cache_policy_override,
            )?;
        }

        Ok(output)
    }

    /// Resolve secrets for all tasks
    fn resolve_all_secrets(
        &self,
        ir: &IntermediateRepresentation,
    ) -> std::result::Result<HashMap<String, ResolvedSecrets>, secrets::SecretError> {
        secrets::resolve_all_task_secrets(&ir.tasks, self.config.secret_salt.as_deref())
    }

    /// Extract fingerprints from resolved secrets
    fn extract_fingerprints(
        &self,
        all_secrets: &HashMap<String, ResolvedSecrets>,
    ) -> HashMap<String, HashMap<String, String>> {
        all_secrets
            .iter()
            .map(|(task_id, resolved)| (task_id.clone(), resolved.fingerprints.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CachePolicy, PipelineMetadata, Task as IRTask};

    #[allow(dead_code)]
    fn make_simple_ir(tasks: Vec<IRTask>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.3".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                trigger: None,
            },
            runtimes: vec![],
            tasks,
        }
    }

    #[allow(dead_code)]
    fn make_task(id: &str, deps: Vec<&str>) -> IRTask {
        IRTask {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string(), id.to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        }
    }

    #[test]
    fn test_executor_config_builder() {
        let config = CIExecutorConfig::new(std::path::PathBuf::from("/project"))
            .with_max_parallel(8)
            .with_dry_run(true);

        assert_eq!(config.max_parallel, 8);
        assert!(config.dry_run);
    }

    #[test]
    fn test_extract_fingerprints() {
        let executor = CIExecutor::new(CIExecutorConfig::default());

        let mut secrets = HashMap::new();
        let mut resolved = ResolvedSecrets::default();
        resolved
            .fingerprints
            .insert("api_key".to_string(), "fp123".to_string());
        secrets.insert("task1".to_string(), resolved);

        let fingerprints = executor.extract_fingerprints(&secrets);

        assert!(fingerprints.contains_key("task1"));
        assert_eq!(
            fingerprints["task1"].get("api_key"),
            Some(&"fp123".to_string())
        );
    }
}

// ============================================================================
// run_ci - Main entry point for CI pipeline execution
// ============================================================================

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
            let start_time = Utc::now();
            let mut tasks_reports = Vec::new();
            let mut pipeline_status = PipelineStatus::Success;

            // Determine cache policy override based on context
            let cache_policy_override = if is_fork_pr(&context) {
                Some(CachePolicy::Readonly)
            } else {
                None
            };

            // Create executor configuration with salt rotation support
            let mut executor_config = CIExecutorConfig::new(project_root.to_path_buf())
                .with_capture_output(true)
                .with_dry_run(dry_run)
                .with_secret_salt(std::env::var("CUENV_SECRET_SALT").unwrap_or_default());

            // Add previous salt for rotation support
            if let Ok(prev_salt) = std::env::var("CUENV_SECRET_SALT_PREV") {
                if !prev_salt.is_empty() {
                    executor_config = executor_config.with_secret_salt_prev(prev_salt);
                }
            }

            let executor_config = if let Some(policy) = cache_policy_override {
                executor_config.with_cache_policy_override(policy)
            } else {
                executor_config
            };

            // Execute pipeline using CIExecutor
            let executor = CIExecutor::new(executor_config);

            // For now, execute tasks individually to maintain compatibility with
            // existing affected task filtering and reporting. Future optimization
            // can use execute_pipeline directly once IR supports affected task filtering.
            for task_name in &tasks_to_run {
                let inputs_matched =
                    matched_inputs_for_task(task_name, config, &changed_files, project_root);
                let outputs = config
                    .tasks
                    .get(task_name)
                    .and_then(|def| def.as_single())
                    .map(|task| task.outputs.clone())
                    .unwrap_or_default();

                println!("  -> Executing {task_name}");
                let task_start = std::time::Instant::now();

                // Execute the task using the new runner
                let result = execute_single_task_by_name(
                    &executor,
                    config,
                    task_name,
                    project_root,
                    cache_policy_override,
                )
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
                pipeline: pipeline.name.clone(),
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

            // Ensure report directory exists
            let report_dir = std::path::Path::new(".cuenv/reports");
            if let Err(e) = std::fs::create_dir_all(report_dir) {
                println!("Failed to create report directory: {e}");
            } else {
                let sha_dir = report_dir.join(&context.sha);
                let _ = std::fs::create_dir_all(&sha_dir);

                let project_filename =
                    project.path.display().to_string().replace(['/', '\\'], "-") + ".json";
                let report_path = sha_dir.join(project_filename);

                if let Err(e) = write_report(&report, &report_path) {
                    println!("Failed to write report: {e}");
                } else {
                    println!("Report written to: {}", report_path.display());
                }
            }

            // Write GitHub Job Summary
            if let Err(e) = crate::report::markdown::write_job_summary(&report) {
                eprintln!("Warning: Failed to write job summary: {e}");
            }

            // Post results to CI provider
            let check_name = format!("cuenv: {}", pipeline.name);
            match provider.create_check(&check_name).await {
                Ok(handle) => {
                    if let Err(e) = provider.complete_check(&handle, &report).await {
                        eprintln!("Warning: Failed to complete check run: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to create check run: {e}");
                }
            }

            // Post PR comment with report summary
            if let Err(e) = provider.upload_report(&report).await {
                eprintln!("Warning: Failed to post PR comment: {e}");
            }

            if pipeline_status == PipelineStatus::Failed {
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

/// Check if this is a fork PR (should use readonly cache)
fn is_fork_pr(context: &crate::context::CIContext) -> bool {
    // Fork PRs typically have a different head repo than base repo
    // This is a simplified check - providers may need more sophisticated detection
    context.event == "pull_request" && context.ref_name.starts_with("refs/pull/")
}

/// Execute a single task by name using the existing project config
///
/// This bridges the gap between the task name-based execution in run_ci
/// and the IR-based execution in CIExecutor.
async fn execute_single_task_by_name(
    _executor: &CIExecutor,
    config: &Project,
    task_name: &str,
    project_root: &std::path::Path,
    cache_policy_override: Option<CachePolicy>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    // Get task definition
    let Some(task_def) = config.tasks.get(task_name) else {
        return Err(ExecutorError::Compilation(format!(
            "Task '{}' not found in project config",
            task_name
        )));
    };

    let Some(task) = task_def.as_single() else {
        return Err(ExecutorError::Compilation(format!(
            "Task '{}' is a group, not a single task",
            task_name
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
