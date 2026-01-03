//! Parallel Execution Engine
//!
//! Executes CI pipelines with bounded parallelism, progress reporting,
//! and fail-fast behavior.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::compiler::Compiler;
use crate::ir::IntermediateRepresentation;
use crate::report::progress::{LiveTaskProgress, ProgressReporter};
use crate::report::{
    ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus as ReportTaskStatus,
};

use super::ExecutorError;
use super::cache::{self, TaskLogs};
use super::config::CIExecutorConfig;
use super::graph::{CITaskGraph, CITaskNode};
use super::runner::{IRTaskRunner, TaskOutput};
use super::secrets::{CIResolvedSecrets, SecretError};

use chrono::Utc;
use cuenv_core::manifest::Project;

/// Result of engine execution.
#[derive(Debug)]
pub struct EngineResult {
    /// Overall success status.
    pub success: bool,
    /// Individual task outputs.
    pub task_outputs: Vec<TaskOutput>,
    /// Total execution duration.
    pub duration: Duration,
    /// Generated pipeline report.
    pub report: PipelineReport,
}

/// Configuration for the execution engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum parallel tasks (0 = num_cpus).
    pub max_parallel: usize,
    /// Project root directory.
    pub project_root: PathBuf,
    /// Whether to capture task output.
    pub capture_output: bool,
    /// Dry run mode (don't execute tasks).
    pub dry_run: bool,
    /// Secret salt for fingerprinting.
    pub secret_salt: Option<String>,
    /// Cache policy override.
    pub cache_policy_override: Option<crate::ir::CachePolicy>,
    /// Cache root directory.
    pub cache_root: Option<PathBuf>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_parallel: 0, // Will use num_cpus
            project_root: PathBuf::from("."),
            capture_output: true,
            dry_run: false,
            secret_salt: None,
            cache_policy_override: None,
            cache_root: None,
        }
    }
}

impl EngineConfig {
    /// Create a new engine config with project root.
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            ..Default::default()
        }
    }

    /// Set maximum parallel tasks.
    #[must_use]
    pub const fn with_max_parallel(mut self, max: usize) -> Self {
        self.max_parallel = max;
        self
    }

    /// Enable dry run mode.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Set secret salt.
    #[must_use]
    pub fn with_secret_salt(mut self, salt: impl Into<String>) -> Self {
        self.secret_salt = Some(salt.into());
        self
    }

    /// Set cache root directory.
    #[must_use]
    pub fn with_cache_root(mut self, cache_root: impl Into<PathBuf>) -> Self {
        self.cache_root = Some(cache_root.into());
        self
    }

    /// Set cache policy override.
    #[must_use]
    pub fn with_cache_policy_override(mut self, policy: crate::ir::CachePolicy) -> Self {
        self.cache_policy_override = Some(policy);
        self
    }

    /// Get effective parallelism level.
    #[must_use]
    pub fn effective_parallelism(&self) -> usize {
        if self.max_parallel == 0 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
        } else {
            self.max_parallel
        }
    }

    /// Get effective cache root.
    #[must_use]
    pub fn effective_cache_root(&self) -> PathBuf {
        self.cache_root
            .clone()
            .unwrap_or_else(|| self.project_root.join(".cuenv/cache"))
    }
}

impl From<CIExecutorConfig> for EngineConfig {
    fn from(config: CIExecutorConfig) -> Self {
        Self {
            max_parallel: config.max_parallel,
            project_root: config.project_root,
            capture_output: config.capture_output,
            dry_run: config.dry_run,
            secret_salt: config.secret_salt,
            cache_policy_override: config.cache_policy_override,
            cache_root: config.cache_root,
        }
    }
}

/// Parallel execution engine with progress reporting.
///
/// Executes tasks from IR with:
/// - Bounded parallelism via semaphore
/// - Progress reporting via trait
/// - Fail-fast on task failure
/// - Topological execution order
pub struct ExecutionEngine<R: ProgressReporter> {
    config: EngineConfig,
    reporter: Arc<R>,
}

impl<R: ProgressReporter + 'static> ExecutionEngine<R> {
    /// Create a new execution engine.
    #[must_use]
    pub fn new(config: EngineConfig, reporter: Arc<R>) -> Self {
        Self { config, reporter }
    }

    /// Execute a pipeline from a project configuration.
    ///
    /// # Arguments
    /// * `project` - The project configuration
    /// * `pipeline_name` - Pipeline name to execute
    /// * `context` - CI execution context
    ///
    /// # Errors
    /// Returns error if compilation fails, tasks fail, or secrets can't be resolved.
    #[tracing::instrument(
        name = "engine_execute",
        fields(
            project_root = %self.config.project_root.display(),
            pipeline = pipeline_name
        ),
        skip(self, project, context)
    )]
    pub async fn execute(
        &self,
        project: &Project,
        pipeline_name: &str,
        context: ContextReport,
    ) -> Result<EngineResult, ExecutorError> {
        let start = std::time::Instant::now();
        let started_at = Utc::now();

        // Step 1: Compile to IR
        tracing::info!("Compiling project to IR");
        let compiler = Compiler::new(project.clone());
        let ir = compiler
            .compile()
            .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

        tracing::info!(task_count = ir.tasks.len(), "IR compilation complete");

        // Notify reporter
        self.reporter
            .pipeline_started(pipeline_name, ir.tasks.len())
            .await;

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

        // Propagate deployment cache policy
        task_graph.propagate_deployment_cache_policy();

        // Step 5: Get parallel groups for execution
        let parallel_groups = task_graph.get_parallel_groups()?;
        tracing::info!(groups = parallel_groups.len(), "Execution groups computed");

        // Step 6: Execute groups with bounded parallelism
        let cache_root = self.config.effective_cache_root();
        let semaphore = Arc::new(Semaphore::new(self.config.effective_parallelism()));

        let mut all_outputs = Vec::new();
        let mut task_reports = Vec::new();
        let mut pipeline_success = true;

        for (group_idx, group) in parallel_groups.iter().enumerate() {
            tracing::info!(
                group = group_idx,
                tasks = group.len(),
                "Executing task group"
            );

            let (group_outputs, group_reports, group_success) = self
                .execute_group(
                    group,
                    &ir,
                    &cache_root,
                    &all_secrets,
                    Arc::clone(&semaphore),
                )
                .await?;

            all_outputs.extend(group_outputs);
            task_reports.extend(group_reports);

            if !group_success {
                pipeline_success = false;
                tracing::warn!("Pipeline failed, aborting remaining groups");
                break;
            }
        }

        let duration = start.elapsed();
        let completed_at = Utc::now();

        // Build pipeline report
        let report = PipelineReport {
            version: cuenv_core::VERSION.to_string(),
            project: project.name.clone(),
            pipeline: pipeline_name.to_string(),
            context,
            started_at,
            completed_at: Some(completed_at),
            duration_ms: Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            status: if pipeline_success {
                PipelineStatus::Success
            } else {
                PipelineStatus::Failed
            },
            tasks: task_reports,
        };

        // Notify reporter of completion
        self.reporter.pipeline_completed(&report).await;

        Ok(EngineResult {
            success: pipeline_success,
            task_outputs: all_outputs,
            duration,
            report,
        })
    }

    /// Execute a single group of tasks with bounded parallelism.
    async fn execute_group(
        &self,
        group: &[&CITaskNode],
        ir: &IntermediateRepresentation,
        cache_root: &std::path::Path,
        all_secrets: &HashMap<String, CIResolvedSecrets>,
        semaphore: Arc<Semaphore>,
    ) -> Result<(Vec<TaskOutput>, Vec<TaskReport>, bool), ExecutorError> {
        let mut outputs = Vec::new();
        let mut reports = Vec::new();
        let mut group_success = true;

        // Check if sequential execution needed
        if self.config.effective_parallelism() <= 1 || group.len() == 1 {
            for node in group {
                let (output, report) = self
                    .execute_single_task(node, ir, cache_root, all_secrets)
                    .await?;

                if !output.success {
                    group_success = false;
                }

                outputs.push(output);
                reports.push(report);

                // Fail fast
                if !group_success {
                    break;
                }
            }
            return Ok((outputs, reports, group_success));
        }

        // Parallel execution with JoinSet and semaphore
        let mut join_set = JoinSet::new();

        for node in group {
            // Check cache first (synchronously to avoid spawning unnecessary tasks)
            let cache_result = cache::check_cache(
                &node.task,
                &node.digest,
                cache_root,
                self.config.cache_policy_override,
            );

            if cache_result.hit {
                tracing::info!(task = %node.id, "Cache hit, skipping execution");

                // Report cached
                self.reporter.task_cached(&node.id, &node.task.id).await;

                let output = TaskOutput::from_cache(node.id.clone(), 0);
                let report = TaskReport {
                    name: node.id.clone(),
                    status: ReportTaskStatus::Cached,
                    duration_ms: 0,
                    exit_code: Some(0),
                    inputs_matched: vec![],
                    cache_key: Some(node.digest.clone()),
                    outputs: vec![],
                };

                outputs.push(output);
                reports.push(report);
                continue;
            }

            if self.config.dry_run {
                tracing::info!(task = %node.id, "Would execute (dry-run)");

                let progress = LiveTaskProgress::pending(&node.id, &node.task.id)
                    .completed(true, Duration::ZERO);
                self.reporter.task_completed(&progress).await;

                let output = TaskOutput::dry_run(node.id.clone());
                let report = TaskReport {
                    name: node.id.clone(),
                    status: ReportTaskStatus::Success,
                    duration_ms: 0,
                    exit_code: Some(0),
                    inputs_matched: vec![],
                    cache_key: None,
                    outputs: vec![],
                };

                outputs.push(output);
                reports.push(report);
                continue;
            }

            // Prepare execution context for spawning
            let task = node.task.clone();
            let task_id = node.id.clone();
            let digest = node.digest.clone();
            let project_root = self.config.project_root.clone();
            let capture = self.config.capture_output;
            let cache_root_owned = cache_root.to_path_buf();
            let policy_override = self.config.cache_policy_override;
            let semaphore = Arc::clone(&semaphore);
            let reporter = Arc::clone(&self.reporter);

            // Build environment with secrets
            let mut env = task.env.clone();
            if let Some(resolved) = all_secrets.get(&task.id) {
                for (name, value) in &resolved.values {
                    env.insert(name.clone(), value.clone());
                }
            }

            // Spawn task execution
            join_set.spawn(async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await;

                // Report task started
                reporter.task_started(&task_id, &task.id).await;

                let start = std::time::Instant::now();
                let runner = IRTaskRunner::new(project_root, capture);
                let result = runner.execute(&task, env).await;
                let duration = start.elapsed();

                (
                    task,
                    task_id,
                    digest,
                    cache_root_owned,
                    policy_override,
                    result,
                    duration,
                )
            });
        }

        // Collect results with fail-fast behavior
        while let Some(join_result) = join_set.join_next().await {
            let (task, task_id, digest, cache_root_owned, policy_override, exec_result, duration) =
                join_result.map_err(|e| ExecutorError::TaskPanic(e.to_string()))?;

            match exec_result {
                Ok(output) => {
                    // Report completion
                    let progress = LiveTaskProgress::pending(&task_id, &task.id)
                        .completed(output.success, duration);
                    self.reporter.task_completed(&progress).await;

                    // Store in cache if successful
                    if output.success
                        && let Err(e) = cache::store_result(
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
                        )
                    {
                        tracing::warn!(
                            task = %task_id,
                            error = %e,
                            "Failed to store task result in cache"
                        );
                    }

                    let report = TaskReport {
                        name: task_id.clone(),
                        status: if output.success {
                            ReportTaskStatus::Success
                        } else {
                            ReportTaskStatus::Failed
                        },
                        duration_ms: output.duration_ms,
                        exit_code: Some(output.exit_code),
                        inputs_matched: vec![],
                        cache_key: if output.success { Some(digest) } else { None },
                        outputs: vec![],
                    };

                    if !output.success {
                        group_success = false;
                        // Fail-fast: abort remaining tasks
                        tracing::warn!(
                            task = %task_id,
                            "Task failed, aborting remaining tasks in group"
                        );
                        join_set.abort_all();
                    }

                    outputs.push(output);
                    reports.push(report);
                }
                Err(e) => {
                    tracing::error!(task = %task_id, error = %e, "Task execution failed");

                    let progress = LiveTaskProgress::pending(&task_id, &task.id)
                        .failed(e.to_string(), duration);
                    self.reporter.task_completed(&progress).await;

                    let output = TaskOutput {
                        task_id: task_id.clone(),
                        exit_code: 1,
                        stdout: String::new(),
                        stderr: e.to_string(),
                        success: false,
                        from_cache: false,
                        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    };

                    let report = TaskReport {
                        name: task_id,
                        status: ReportTaskStatus::Failed,
                        duration_ms: output.duration_ms,
                        exit_code: Some(1),
                        inputs_matched: vec![],
                        cache_key: None,
                        outputs: vec![],
                    };

                    group_success = false;
                    // Fail-fast: abort remaining tasks
                    tracing::warn!("Task execution error, aborting remaining tasks in group");
                    join_set.abort_all();

                    outputs.push(output);
                    reports.push(report);
                }
            }
        }

        Ok((outputs, reports, group_success))
    }

    /// Execute a single task with cache checking.
    async fn execute_single_task(
        &self,
        node: &CITaskNode,
        _ir: &IntermediateRepresentation,
        cache_root: &std::path::Path,
        all_secrets: &HashMap<String, CIResolvedSecrets>,
    ) -> Result<(TaskOutput, TaskReport), ExecutorError> {
        // Check cache
        let cache_result = cache::check_cache(
            &node.task,
            &node.digest,
            cache_root,
            self.config.cache_policy_override,
        );

        if cache_result.hit {
            tracing::info!(task = %node.id, "Cache hit, skipping execution");

            self.reporter.task_cached(&node.id, &node.task.id).await;

            let output = TaskOutput::from_cache(node.id.clone(), 0);
            let report = TaskReport {
                name: node.id.clone(),
                status: ReportTaskStatus::Cached,
                duration_ms: 0,
                exit_code: Some(0),
                inputs_matched: vec![],
                cache_key: Some(node.digest.clone()),
                outputs: vec![],
            };

            return Ok((output, report));
        }

        if self.config.dry_run {
            tracing::info!(task = %node.id, "Would execute (dry-run)");

            let progress =
                LiveTaskProgress::pending(&node.id, &node.task.id).completed(true, Duration::ZERO);
            self.reporter.task_completed(&progress).await;

            let output = TaskOutput::dry_run(node.id.clone());
            let report = TaskReport {
                name: node.id.clone(),
                status: ReportTaskStatus::Success,
                duration_ms: 0,
                exit_code: Some(0),
                inputs_matched: vec![],
                cache_key: None,
                outputs: vec![],
            };

            return Ok((output, report));
        }

        // Report task started
        self.reporter.task_started(&node.id, &node.task.id).await;

        // Build environment with secrets
        let mut env = node.task.env.clone();
        if let Some(resolved) = all_secrets.get(&node.id) {
            for (name, value) in &resolved.values {
                env.insert(name.clone(), value.clone());
            }
        }

        // Execute
        let start = std::time::Instant::now();
        let runner =
            IRTaskRunner::new(self.config.project_root.clone(), self.config.capture_output);
        let result = runner.execute(&node.task, env).await;
        let duration = start.elapsed();

        match result {
            Ok(output) => {
                // Report completion
                let progress = LiveTaskProgress::pending(&node.id, &node.task.id)
                    .completed(output.success, duration);
                self.reporter.task_completed(&progress).await;

                // Store in cache if successful
                if output.success {
                    let _ = cache::store_result(
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
                    );
                }

                let report = TaskReport {
                    name: node.id.clone(),
                    status: if output.success {
                        ReportTaskStatus::Success
                    } else {
                        ReportTaskStatus::Failed
                    },
                    duration_ms: output.duration_ms,
                    exit_code: Some(output.exit_code),
                    inputs_matched: vec![],
                    cache_key: if output.success {
                        Some(node.digest.clone())
                    } else {
                        None
                    },
                    outputs: vec![],
                };

                Ok((output, report))
            }
            Err(e) => {
                tracing::error!(task = %node.id, error = %e, "Task execution failed");

                let progress = LiveTaskProgress::pending(&node.id, &node.task.id)
                    .failed(e.to_string(), duration);
                self.reporter.task_completed(&progress).await;

                let output = TaskOutput {
                    task_id: node.id.clone(),
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: e.to_string(),
                    success: false,
                    from_cache: false,
                    duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                };

                let report = TaskReport {
                    name: node.id.clone(),
                    status: ReportTaskStatus::Failed,
                    duration_ms: output.duration_ms,
                    exit_code: Some(1),
                    inputs_matched: vec![],
                    cache_key: None,
                    outputs: vec![],
                };

                Ok((output, report))
            }
        }
    }

    /// Resolve secrets for all tasks.
    fn resolve_all_secrets(
        &self,
        ir: &IntermediateRepresentation,
    ) -> Result<HashMap<String, CIResolvedSecrets>, SecretError> {
        super::secrets::resolve_all_task_secrets(&ir.tasks, self.config.secret_salt.as_deref())
    }

    /// Extract fingerprints from resolved secrets.
    fn extract_fingerprints(
        all_secrets: &HashMap<String, CIResolvedSecrets>,
    ) -> HashMap<String, BTreeMap<String, String>> {
        all_secrets
            .iter()
            .map(|(task_id, resolved)| {
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
    use crate::report::progress::NoOpReporter;

    #[test]
    fn test_engine_config_default() {
        let config = EngineConfig::default();
        assert_eq!(config.max_parallel, 0);
        assert!(!config.dry_run);
        assert!(config.capture_output);
    }

    #[test]
    fn test_engine_config_builder() {
        let config = EngineConfig::new("/project")
            .with_max_parallel(4)
            .with_dry_run(true)
            .with_secret_salt("test-salt");

        assert_eq!(config.max_parallel, 4);
        assert!(config.dry_run);
        assert_eq!(config.secret_salt, Some("test-salt".to_string()));
    }

    #[test]
    fn test_engine_config_effective_parallelism() {
        let config = EngineConfig::default();
        assert!(config.effective_parallelism() >= 1);

        let config = EngineConfig::default().with_max_parallel(8);
        assert_eq!(config.effective_parallelism(), 8);
    }

    #[test]
    fn test_engine_config_effective_cache_root() {
        let config = EngineConfig::new("/project");
        assert_eq!(
            config.effective_cache_root(),
            PathBuf::from("/project/.cuenv/cache")
        );

        let config = EngineConfig::new("/project").with_cache_root("/custom/cache");
        assert_eq!(
            config.effective_cache_root(),
            PathBuf::from("/custom/cache")
        );
    }

    #[test]
    fn test_engine_creation() {
        let config = EngineConfig::new("/project");
        let reporter = Arc::new(NoOpReporter);
        let _engine = ExecutionEngine::new(config, reporter);
    }

    #[test]
    fn test_extract_fingerprints_empty() {
        let secrets: HashMap<String, CIResolvedSecrets> = HashMap::new();
        let fingerprints = ExecutionEngine::<NoOpReporter>::extract_fingerprints(&secrets);
        assert!(fingerprints.is_empty());
    }
}
