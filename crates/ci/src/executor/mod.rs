//! CI Pipeline Executor
//!
//! Executes CI pipelines with proper dependency ordering, caching,
//! and parallel execution.

// CI executor outputs to stdout/stderr as part of its normal operation
// Executor has inherent complexity from parallel task management and caching
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cognitive_complexity,
    clippy::too_many_lines
)]

pub mod backend;
pub mod cache;
pub mod config;
pub mod engine;
pub mod graph;
pub mod lock;
pub mod metrics;
mod orchestrator;
pub mod redact;
pub mod remote;
pub mod runner;
pub mod secrets;

pub use backend::{
    BackendError, BackendResult, CacheBackend, CacheEntry, CacheLookupResult, CacheOutput,
};
pub use cache::LocalCacheBackend;
pub use config::CIExecutorConfig;
pub use engine::{EngineConfig, EngineResult, ExecutionEngine};
pub use lock::{ConcurrencyLock, LockConfig, LockError, LockGuard};
pub use metrics::{CacheMetrics, RestoreErrorType, global_metrics};
pub use orchestrator::run_ci;
pub use redact::{LogRedactor, ShortSecretWarning, redact_secrets};
pub use remote::{RemoteCacheBackend, RemoteCacheConfig};
pub use runner::TaskOutput;
pub use secrets::{EnvSecretResolver, MockSecretResolver, SaltConfig, SecretResolver};

use crate::compiler::Compiler;
use crate::ir::IntermediateRepresentation;
use cache::TaskLogs;
use cuenv_core::manifest::Project;
use graph::{CITaskGraph, CITaskNode};
use runner::IRTaskRunner;
use secrets::CIResolvedSecrets;
use std::collections::{BTreeMap, HashMap};
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

use std::sync::Arc;

/// CI Pipeline Executor
///
/// Executes CI pipelines with:
/// - IR compilation and validation
/// - Dependency-ordered parallel execution
/// - Content-addressable caching (pluggable backends)
/// - Secret resolution and injection
pub struct CIExecutor {
    config: CIExecutorConfig,
    /// Optional injected cache backend (uses local cache if None)
    cache_backend: Option<Arc<dyn CacheBackend>>,
}

impl CIExecutor {
    /// Create a new executor with the given configuration
    #[must_use]
    pub fn new(config: CIExecutorConfig) -> Self {
        Self {
            config,
            cache_backend: None,
        }
    }

    /// Create an executor with an injected cache backend
    ///
    /// This enables using custom cache backends (e.g., remote cache) or
    /// mock backends for testing.
    #[must_use]
    pub fn with_cache_backend(config: CIExecutorConfig, backend: Arc<dyn CacheBackend>) -> Self {
        Self {
            config,
            cache_backend: Some(backend),
        }
    }

    /// Check if a custom cache backend is configured
    #[must_use]
    pub fn has_custom_cache_backend(&self) -> bool {
        self.cache_backend.is_some()
    }

    /// Get the cache backend name (for logging/metrics)
    #[must_use]
    pub fn cache_backend_name(&self) -> &'static str {
        self.cache_backend.as_ref().map_or("local", |b| b.name())
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
        let fingerprints = Self::extract_fingerprints(&all_secrets);

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
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// Execute a single group of tasks (can run in parallel)
    async fn execute_group(
        &self,
        group: &[&CITaskNode],
        ir: &IntermediateRepresentation,
        cache_root: &std::path::Path,
        all_secrets: &HashMap<String, CIResolvedSecrets>,
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
        all_secrets: &HashMap<String, CIResolvedSecrets>,
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
    ) -> std::result::Result<HashMap<String, CIResolvedSecrets>, secrets::SecretError> {
        secrets::resolve_all_task_secrets(&ir.tasks, self.config.secret_salt.as_deref())
    }

    /// Extract fingerprints from resolved secrets
    fn extract_fingerprints(
        all_secrets: &HashMap<String, CIResolvedSecrets>,
    ) -> HashMap<String, BTreeMap<String, String>> {
        all_secrets
            .iter()
            .map(|(task_id, resolved)| {
                // Convert HashMap to BTreeMap for deterministic ordering
                let fingerprints: BTreeMap<String, String> = resolved
                    .fingerprints()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                (task_id.clone(), fingerprints)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CachePolicy, PipelineMetadata, Task as IRTask};
    use cuenv_core::ci::PipelineMode;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn make_simple_ir(tasks: Vec<IRTask>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            tasks,
        }
    }

    fn make_task(id: &str, deps: &[&str]) -> IRTask {
        IRTask {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string(), id.to_string()],
            shell: false,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: deps.iter().map(|s| (*s).to_string()).collect(),
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }

    // ==========================================================================
    // ExecutorError display tests
    // ==========================================================================

    #[test]
    fn test_executor_error_compilation_display() {
        let err = ExecutorError::Compilation("Syntax error in line 5".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Failed to compile"));
        assert!(msg.contains("Syntax error in line 5"));
    }

    #[test]
    fn test_executor_error_task_panic_display() {
        let err = ExecutorError::TaskPanic("thread 'main' panicked".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Task panicked"));
        assert!(msg.contains("thread 'main' panicked"));
    }

    #[test]
    fn test_executor_error_pipeline_not_found_display() {
        let err = ExecutorError::PipelineNotFound {
            name: "build".to_string(),
            available: "default, test, deploy".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Pipeline 'build' not found"));
        assert!(msg.contains("default, test, deploy"));
    }

    #[test]
    fn test_executor_error_no_ci_config_display() {
        let err = ExecutorError::NoCIConfig;
        let msg = err.to_string();
        assert!(msg.contains("has no CI configuration"));
    }

    // ==========================================================================
    // PipelineResult tests
    // ==========================================================================

    #[test]
    fn test_pipeline_result_fields() {
        let result = PipelineResult {
            success: true,
            tasks: vec![TaskOutput::dry_run("task1".to_string())],
            duration_ms: 1500,
        };

        assert!(result.success);
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.duration_ms, 1500);
    }

    #[test]
    fn test_pipeline_result_failed() {
        let result = PipelineResult {
            success: false,
            tasks: vec![],
            duration_ms: 0,
        };

        assert!(!result.success);
    }

    // ==========================================================================
    // CIExecutor construction tests
    // ==========================================================================

    #[test]
    fn test_ci_executor_new() {
        let config = CIExecutorConfig::default();
        let executor = CIExecutor::new(config);

        assert!(!executor.has_custom_cache_backend());
        assert_eq!(executor.cache_backend_name(), "local");
    }

    #[test]
    fn test_ci_executor_with_cache_backend() {
        let config = CIExecutorConfig::default();
        let temp_dir = tempfile::TempDir::new().unwrap();
        let backend = Arc::new(LocalCacheBackend::new(temp_dir.path()));
        let executor = CIExecutor::with_cache_backend(config, backend);

        assert!(executor.has_custom_cache_backend());
        // LocalCacheBackend::name() returns "local"
        assert_eq!(executor.cache_backend_name(), "local");
    }

    #[test]
    fn test_ci_executor_has_custom_cache_backend_false() {
        let executor = CIExecutor::new(CIExecutorConfig::default());
        assert!(!executor.has_custom_cache_backend());
    }

    #[test]
    fn test_ci_executor_cache_backend_name_default() {
        let executor = CIExecutor::new(CIExecutorConfig::default());
        assert_eq!(executor.cache_backend_name(), "local");
    }

    // ==========================================================================
    // CIExecutorConfig builder tests
    // ==========================================================================

    #[test]
    fn test_executor_config_builder() {
        let config = CIExecutorConfig::new(std::path::PathBuf::from("/project"))
            .with_max_parallel(8)
            .with_dry_run(true);

        assert_eq!(config.max_parallel, 8);
        assert!(config.dry_run);
    }

    #[test]
    fn test_executor_config_default() {
        let config = CIExecutorConfig::default();
        assert!(!config.dry_run);
        assert!(config.max_parallel >= 1);
    }

    #[test]
    fn test_executor_config_with_capture_output() {
        let config =
            CIExecutorConfig::new(std::path::PathBuf::from("/project")).with_capture_output(true);

        assert!(config.capture_output);
    }

    // ==========================================================================
    // Helper function tests
    // ==========================================================================

    #[test]
    fn test_make_simple_ir() {
        let task = make_task("build", &[]);
        let ir = make_simple_ir(vec![task]);

        assert_eq!(ir.version, "1.5");
        assert_eq!(ir.pipeline.name, "test");
        assert_eq!(ir.tasks.len(), 1);
        assert_eq!(ir.tasks[0].id, "build");
    }

    #[test]
    fn test_make_task_with_dependencies() {
        let task = make_task("test", &["build", "lint"]);

        assert_eq!(task.id, "test");
        assert_eq!(task.depends_on, vec!["build", "lint"]);
        assert_eq!(task.command, vec!["echo", "test"]);
        assert!(!task.shell);
    }

    #[test]
    fn test_make_task_no_dependencies() {
        let task = make_task("root", &[]);

        assert_eq!(task.id, "root");
        assert!(task.depends_on.is_empty());
    }

    // ==========================================================================
    // extract_fingerprints tests
    // ==========================================================================

    #[test]
    fn test_extract_fingerprints() {
        temp_env::with_var("TEST_EXTRACT_FP_SECRET", Some("test_value"), || {
            let executor = CIExecutor::new(CIExecutorConfig::default());

            let mut secrets = HashMap::new();

            let secret_configs = BTreeMap::from([(
                "api_key".to_string(),
                crate::ir::SecretConfig {
                    source: "TEST_EXTRACT_FP_SECRET".to_string(),
                    cache_key: true,
                },
            )]);

            let resolved = CIResolvedSecrets::from_env(&secret_configs, Some("test-salt")).unwrap();
            secrets.insert("task1".to_string(), resolved);

            let _ = executor; // silence unused warning
            let fingerprints = CIExecutor::extract_fingerprints(&secrets);

            assert!(fingerprints.contains_key("task1"));
            assert!(fingerprints["task1"].contains_key("api_key"));
        });
    }

    #[test]
    fn test_extract_fingerprints_empty() {
        let secrets: HashMap<String, CIResolvedSecrets> = HashMap::new();
        let fingerprints = CIExecutor::extract_fingerprints(&secrets);
        assert!(fingerprints.is_empty());
    }

    #[test]
    fn test_extract_fingerprints_multiple_tasks() {
        temp_env::with_vars(
            [
                ("TEST_FP_SECRET1", Some("value1")),
                ("TEST_FP_SECRET2", Some("value2")),
            ],
            || {
                let mut secrets = HashMap::new();

                let secret_configs1 = BTreeMap::from([(
                    "secret1".to_string(),
                    crate::ir::SecretConfig {
                        source: "TEST_FP_SECRET1".to_string(),
                        cache_key: true,
                    },
                )]);
                let resolved1 =
                    CIResolvedSecrets::from_env(&secret_configs1, Some("salt")).unwrap();
                secrets.insert("task1".to_string(), resolved1);

                let secret_configs2 = BTreeMap::from([(
                    "secret2".to_string(),
                    crate::ir::SecretConfig {
                        source: "TEST_FP_SECRET2".to_string(),
                        cache_key: true,
                    },
                )]);
                let resolved2 =
                    CIResolvedSecrets::from_env(&secret_configs2, Some("salt")).unwrap();
                secrets.insert("task2".to_string(), resolved2);

                let fingerprints = CIExecutor::extract_fingerprints(&secrets);

                assert_eq!(fingerprints.len(), 2);
                assert!(fingerprints.contains_key("task1"));
                assert!(fingerprints.contains_key("task2"));
            },
        );
    }

    // ==========================================================================
    // TaskOutput helper tests
    // ==========================================================================

    #[test]
    fn test_task_output_dry_run() {
        let output = TaskOutput::dry_run("my-task".to_string());
        assert!(output.success);
        assert_eq!(output.task_id, "my-task");
    }

    #[test]
    fn test_task_output_from_cache() {
        let output = TaskOutput::from_cache("cached-task".to_string(), 500);
        assert!(output.success);
        assert_eq!(output.task_id, "cached-task");
        assert_eq!(output.duration_ms, 500);
    }
}
