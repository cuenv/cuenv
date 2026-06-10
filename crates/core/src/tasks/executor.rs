//! Task executor for running tasks with environment support.
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Host execution; isolation/caching is delegated to other backends

use super::backend::{BackendFactory, TaskBackend, create_backend_with_factory};
use super::cache::{BuildActionInput, RecordInput, TaskCacheConfig};
pub use super::command::{execute_command, execute_command_with_redaction};
use super::process::TaskAttempt;
pub use super::result::{TASK_FAILURE_SNIPPET_LINES, TaskResult, summarize_task_failure};
#[cfg(test)]
use super::result::{format_failure_streams, summarize_stream};
#[cfg(test)]
use super::workspace::{
    cargo_toml_has_workspace, deno_json_has_workspace, package_json_has_workspaces,
};
use super::workspace::{find_workspace_root, normalize_join};
use super::{Task, TaskDirectory, TaskDirectoryBase, TaskGraph, TaskGroup, TaskNode, Tasks};
use crate::OutputCapture;
use crate::config::BackendConfig;
use crate::environment::Environment;
use crate::{Error, Result};
use async_recursion::async_recursion;
#[cfg(test)]
use cuenv_workspaces::PackageManager;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinSet;
use tracing::instrument;

/// Emit a `task.group_completed` event with succeeded/failed/skipped counts
/// derived from the inner result.
///
/// On `Err` the group is reported as failed with all children counted as
/// failed (we lack per-child results once a sequence/parallel aborts).
fn emit_group_completion(
    prefix: &str,
    started: std::time::Instant,
    outcome: &Result<Vec<TaskResult>>,
    total_children: usize,
) {
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (success, succeeded, failed, skipped) = match outcome {
        Ok(results) => {
            let succeeded = results.iter().filter(|r| r.success).count();
            let failed = results.iter().filter(|r| !r.success).count();
            let skipped = total_children.saturating_sub(succeeded + failed);
            (failed == 0, succeeded, failed, skipped)
        }
        Err(_) => (false, 0_usize, total_children, 0_usize),
    };
    cuenv_events::emit_task_group_completed!(
        prefix,
        success,
        duration_ms,
        succeeded,
        failed,
        skipped
    );
}

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: OutputCapture,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
    /// When `true`, a failing task does not abort the run: its dependents
    /// in later parallel groups are emitted as `task.skipped` (with a
    /// `DependencyFailed` reason) and unrelated sibling chains continue.
    /// A panic / `JoinError` is always fatal — we don't reason about
    /// state after a panic. Mirrors `ci.pipelines[*].continueOnError`.
    pub continue_on_error: bool,
    /// Environment variables to propagate (resolved via policies)
    pub environment: Environment,
    /// Optional working directory override (reserved for future backends)
    pub working_dir: Option<PathBuf>,
    /// Project root for resolving inputs/outputs (env.cue root)
    pub project_root: PathBuf,
    /// Path to cue.mod root for resolving relative source paths
    pub cue_module_root: Option<PathBuf>,
    /// Optional: materialize cached outputs on cache hit
    pub materialize_outputs: Option<PathBuf>,
    /// Optional: cache directory override
    pub cache_dir: Option<PathBuf>,
    /// Optional: print cache path on hits/misses
    pub show_cache_path: bool,
    /// Backend configuration
    pub backend_config: Option<BackendConfig>,
    /// CLI backend selection override
    pub cli_backend: Option<String>,
    /// Optional task-result caching infrastructure (CAS + action cache + VCS hasher).
    /// When `None`, the executor behaves exactly as it did before content-addressed
    /// caching was wired in: tasks always run, nothing is persisted, nothing is read.
    pub cache: Option<TaskCacheConfig>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: OutputCapture::Capture,
            max_parallel: 0,
            continue_on_error: false,
            environment: Environment::new(),
            working_dir: None,
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cue_module_root: None,
            materialize_outputs: None,
            cache_dir: None,
            show_cache_path: false,
            backend_config: None,
            cli_backend: None,
            cache: None,
        }
    }
}

/// Task executor
pub struct TaskExecutor {
    config: ExecutorConfig,
    backend: Arc<dyn TaskBackend>,
}
impl TaskExecutor {
    /// Create a new executor with host backend only
    pub fn new(config: ExecutorConfig) -> Self {
        Self::with_dagger_factory(config, None)
    }

    /// Create a new executor with optional dagger backend support.
    ///
    /// Pass `Some(cuenv_dagger::create_dagger_backend)` to enable dagger backend.
    pub fn with_dagger_factory(
        config: ExecutorConfig,
        dagger_factory: Option<BackendFactory>,
    ) -> Self {
        let backend = create_backend_with_factory(
            config.backend_config.as_ref(),
            config.project_root.clone(),
            config.cli_backend.as_deref(),
            dagger_factory,
        );
        Self { config, backend }
    }

    /// Create a new executor with the given config but sharing the backend
    fn with_shared_backend(config: ExecutorConfig, backend: Arc<dyn TaskBackend>) -> Self {
        Self { config, backend }
    }

    /// Execute a single task, consulting the action cache when configured.
    ///
    /// Flow:
    /// 1. If a [`TaskCacheConfig`] is set and the task declares `inputs`,
    ///    build the [`cuenv_cas::Action`] envelope and look it up in the
    ///    action cache. On a hit, materialize cached outputs into the
    ///    workdir and return without spawning anything.
    /// 2. Otherwise dispatch to the configured backend (host or dagger).
    /// 3. On a successful miss, persist outputs + result to the cache so
    ///    the next invocation hits.
    #[instrument(name = "execute_task", skip(self, task), fields(task_name = %name))]
    pub async fn execute_task(&self, name: &str, task: &Task) -> Result<TaskResult> {
        // Cache plumbing — None means no caching, behave as before.
        // We compute the action digest up front so we can reuse it on the
        // record path without re-walking the inputs.
        let cache_handle: Option<(TaskCacheConfig, cuenv_cas::Digest, PathBuf)> =
            if let Some(cache) = self.config.cache.clone() {
                let workdir = self.workdir_for_task(task)?;
                let outcome = super::cache::build_action(BuildActionInput {
                    task,
                    task_name: name,
                    environment: &self.config.environment,
                    cache: &cache,
                    workdir: &workdir,
                    project_root: self.project_root_for_task(task),
                    module_root: self
                        .config
                        .cue_module_root
                        .as_deref()
                        .unwrap_or(&self.config.project_root),
                })
                .await?;
                match outcome {
                    super::cache::CacheOutcome::Eligible(_, action_digest) => {
                        // Cache lookup. On a hit, short-circuit execution.
                        if let Some(cached) = super::cache::lookup(&cache, &action_digest, task)? {
                            tracing::debug!(task = %name, "action cache hit");
                            cuenv_events::emit_task_cache_hit!(name, action_digest.to_string());
                            return self.return_cache_hit(CacheHitInput {
                                name,
                                task,
                                cache: &cache,
                                workdir: &workdir,
                                cached: &cached,
                            });
                        }
                        tracing::debug!(task = %name, "action cache miss");
                        cuenv_events::emit_task_cache_miss!(name);
                        Some((cache, action_digest, workdir))
                    }
                    super::cache::CacheOutcome::Skipped(reason) => {
                        cuenv_events::emit_task_cache_skipped!(name, reason);
                        None
                    }
                }
            } else {
                None
            };

        // Real execution. Both backends produce the same `TaskResult`.
        let start = std::time::Instant::now();
        let result = self.execute_task_with_retries(name, task).await?;
        let duration_ms = start.elapsed().as_millis();

        // Persist on successful miss. Cache writes are best-effort: a write
        // failure logs but does not fail the user's task.
        if let Some((cache, action_digest, workdir)) = cache_handle
            && super::cache::effective_policy(task).mode.allows_write()
            && result.exit_code == Some(0)
            && let Err(e) = super::cache::record(RecordInput {
                cache: &cache,
                action_digest: &action_digest,
                workdir: &workdir,
                task,
                stdout: &result.stdout,
                stderr: &result.stderr,
                exit_code: 0,
                duration_ms,
            })
        {
            tracing::warn!(task = %name, error = %e, "cache write failed");
        }

        Ok(result)
    }

    async fn execute_task_with_retries(&self, name: &str, task: &Task) -> Result<TaskResult> {
        // Timeout on the dagger backend would drop the future without tearing
        // down the remote container, leaking the running task. Reject it
        // explicitly rather than silently leak (host-backend timeout is killed
        // via the process group in `process::terminate_child`).
        if self.backend.name() == "dagger" && task.timeout.is_some() {
            return Err(Error::configuration(format!(
                "task `timeout` is not yet supported on the dagger backend: the container \
                 would keep running past the deadline. Remove `timeout` from task '{name}' \
                 or run it on the host backend."
            )));
        }

        let retry_attempts = task.retry.as_ref().map_or(0, |retry| retry.attempts);
        let max_attempts = retry_attempts.saturating_add(1);
        let retry_delay = task
            .retry
            .as_ref()
            .and_then(|retry| retry.delay.as_deref())
            .map(|delay| parse_task_duration("retry.delay", delay))
            .transpose()?;

        let mut attempt = 1_u32;
        loop {
            // A timeout is a hard policy violation, not a transient failure:
            // retrying would re-incur the full timeout each attempt, so a
            // timed-out attempt ends the task immediately.
            let result = match self.execute_task_once(name, task).await? {
                TaskAttempt::TimedOut(result) => return Ok(result),
                TaskAttempt::Completed(result) => result,
            };
            if result.success || attempt >= max_attempts {
                return Ok(result);
            }

            attempt = attempt.saturating_add(1);
            cuenv_events::emit_task_retrying!(name, attempt, max_attempts);
            if let Some(delay) = retry_delay {
                tokio::time::sleep(delay).await;
            }
        }
    }

    async fn execute_task_once(&self, name: &str, task: &Task) -> Result<TaskAttempt> {
        if self.backend.name() == "dagger" {
            // Dagger has no host timeout (rejected in `execute_task_with_retries`),
            // so every dagger attempt runs to completion.
            let ctx = super::backend::TaskExecutionContext {
                name,
                task,
                environment: &self.config.environment,
                project_root: &self.config.project_root,
                capture_output: self.config.capture_output,
            };
            Ok(TaskAttempt::Completed(self.backend.execute(&ctx).await?))
        } else {
            self.execute_task_non_hermetic(name, task).await
        }
    }

    /// Reproduce a [`TaskResult`] from a cache hit and emit the same
    /// lifecycle events the executor would emit on a normal run, so
    /// downstream renderers (CLI / TUI / JSON) see no behavioral
    /// difference between a cached and an uncached task.
    fn return_cache_hit(&self, input: CacheHitInput<'_>) -> Result<TaskResult> {
        let CacheHitInput {
            name,
            task,
            cache,
            workdir,
            cached,
        } = input;

        let (stdout, stderr, exit_code) = super::cache::materialize_hit(cache, workdir, cached)?;
        let success = exit_code == 0;

        let cmd_str = if let Some(script) = &task.script {
            format!("[script: {} bytes] (cached)", script.len())
        } else if task.command.is_empty() {
            format!("{} (cached)", task.args.join(" "))
        } else {
            format!("{} {} (cached)", task.command, task.args.join(" "))
        };
        cuenv_events::emit_task_started!(name, cmd_str, false);
        emit_cached_output_events(name, "stdout", &stdout);
        emit_cached_output_events(name, "stderr", &stderr);
        cuenv_events::emit_task_completed!(
            name,
            success,
            Some(exit_code),
            u64::try_from(cached.execution_metadata.duration_ms).unwrap_or(0)
        );

        Ok(TaskResult {
            name: name.to_string(),
            exit_code: Some(exit_code),
            stdout,
            stderr,
            success,
        })
    }

    /// Compute the working directory the executor would use for a task.
    ///
    /// Extracted from `execute_task_non_hermetic` so that the cache wrapper
    /// in `execute_task` can resolve outputs against the same path the
    /// task itself ran in.
    fn workdir_for_task(&self, task: &Task) -> Result<PathBuf> {
        if let Some(ref dir) = task.directory {
            self.resolve_task_directory(task, dir)
        } else if let Some(ref project_root) = task.project_root {
            Ok(project_root.clone())
        } else if let Some(ref source) = task.source {
            if let Some(dir) = source.directory() {
                Ok(self
                    .config
                    .cue_module_root
                    .as_ref()
                    .unwrap_or(&self.config.project_root)
                    .join(dir))
            } else if let Some(ref project_root) = task.project_root {
                Ok(project_root.clone())
            } else {
                Ok(self
                    .config
                    .cue_module_root
                    .clone()
                    .unwrap_or_else(|| self.config.project_root.clone()))
            }
        } else if !task.hermetic {
            if let Some(manager) = cuenv_workspaces::detect_from_command(&task.command) {
                Ok(find_workspace_root(manager, &self.config.project_root))
            } else {
                Ok(self.config.project_root.clone())
            }
        } else {
            Ok(self.config.project_root.clone())
        }
    }

    fn resolve_task_directory(&self, task: &Task, directory: &TaskDirectory) -> Result<PathBuf> {
        let module_root = self
            .config
            .cue_module_root
            .as_ref()
            .unwrap_or(&self.config.project_root);

        let base = match directory.from {
            TaskDirectoryBase::Definition => {
                self.source_base(task.source.as_ref(), "definition")?
            }
            TaskDirectoryBase::Caller => self.source_base(task.caller_source.as_ref(), "caller")?,
            TaskDirectoryBase::Module => module_root.clone(),
        };

        self.resolve_directory_path(module_root, &base, &directory.path)
    }

    fn source_base(
        &self,
        source: Option<&super::SourceLocation>,
        base_name: &str,
    ) -> Result<PathBuf> {
        let module_root = self
            .config
            .cue_module_root
            .as_ref()
            .unwrap_or(&self.config.project_root);

        let source = source.ok_or_else(|| {
            Error::configuration(format!(
                "Task dir from '{base_name}' requires source metadata"
            ))
        })?;

        Ok(source
            .directory()
            .map_or_else(|| module_root.clone(), |dir| module_root.join(dir)))
    }

    fn resolve_directory_path(
        &self,
        module_root: &Path,
        base: &Path,
        path: &str,
    ) -> Result<PathBuf> {
        let resolved = normalize_join(base, path);
        if resolved.starts_with(module_root) {
            Ok(resolved)
        } else {
            Err(Error::configuration(format!(
                "Task dir '{}' resolves outside the CUE module root '{}'",
                path,
                module_root.display()
            )))
        }
    }

    fn project_root_for_task<'a>(&'a self, task: &'a Task) -> &'a Path {
        task.project_root
            .as_deref()
            .unwrap_or(&self.config.project_root)
    }

    /// Execute a task non-hermetically (directly in workspace/project root)
    ///
    /// Used for tasks like `bun install` that need to write to the real filesystem.
    async fn execute_task_non_hermetic(&self, name: &str, task: &Task) -> Result<TaskAttempt> {
        // Check if this is an unresolved TaskRef (should have been resolved before execution)
        if task.is_task_ref() && task.project_root.is_none() {
            return Err(Error::configuration(format!(
                "Task '{}' references another project's task ({}) but the reference could not be resolved.\n\
                 This usually means:\n\
                 - The referenced project doesn't exist or has no 'name' field in env.cue\n\
                 - The referenced task '{}' doesn't exist in that project\n\
                 - There was an error loading the referenced project's env.cue\n\
                 Run with RUST_LOG=debug for more details.",
                name,
                task.task_ref.as_deref().unwrap_or("unknown"),
                task.task_ref
                    .as_deref()
                    .and_then(|r| r.split(':').next_back())
                    .unwrap_or("unknown")
            )));
        }

        // Determine working directory (in priority order: see `workdir_for_task`).
        let workdir = self.workdir_for_task(task)?;

        tracing::info!(
            task = %name,
            workdir = %workdir.display(),
            hermetic = false,
            "Executing non-hermetic task"
        );

        // Emit command being run - always emit task_started for all modes
        // (TUI needs events even when capture_output is true)
        let cmd_str = if let Some(script) = &task.script {
            format!("[script: {} bytes]", script.len())
        } else if task.command.is_empty() {
            task.args.join(" ")
        } else {
            format!("{} {}", task.command, task.args.join(" "))
        };

        cuenv_events::emit_task_started!(name, cmd_str, false);

        // Build command - handle script mode vs command mode
        let command_spec =
            task.command_spec(|command| self.config.environment.resolve_command(command))?;
        let mut cmd = Command::new(&command_spec.program);
        cmd.args(&command_spec.args);

        // Set working directory and environment (hermetic - no host PATH pollution)
        cmd.current_dir(&workdir);
        let env_vars = self.config.environment.merge_with_system_hermetic();
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }

        // Apply task-level env vars, including secrets resolved at execution time.
        let (task_env, secrets) = super::env::resolve_task_env(name, &task.env).await?;
        cuenv_events::register_secrets(secrets);
        for (key, value) in task_env {
            cmd.env(key, value);
        }

        // Force color output even when stdout is piped (for capture mode)
        // These are widely supported: FORCE_COLOR by Node.js/chalk, CLICOLOR_FORCE by BSD/macOS
        if !env_vars.contains_key("FORCE_COLOR") {
            cmd.env("FORCE_COLOR", "1");
        }
        if !env_vars.contains_key("CLICOLOR_FORCE") {
            cmd.env("CLICOLOR_FORCE", "1");
        }

        let timeout = task
            .timeout
            .as_deref()
            .map(|timeout| parse_task_duration("timeout", timeout))
            .transpose()?;
        super::process::run_task_process(name, cmd, self.config.capture_output, timeout).await
    }

    /// Execute a task node (single task, group, or list)
    #[async_recursion]
    pub async fn execute_node(
        &self,
        name: &str,
        node: &TaskNode,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match node {
            TaskNode::Task(task) => {
                let result = self.execute_task(name, task.as_ref()).await?;
                Ok(vec![result])
            }
            TaskNode::Group(group) => self.execute_parallel(name, group, all_tasks).await,
            TaskNode::Sequence(seq) => self.execute_sequential(name, seq, all_tasks).await,
        }
    }

    /// Execute a task definition (legacy alias for execute_node)
    #[async_recursion]
    pub async fn execute_definition(
        &self,
        name: &str,
        node: &TaskNode,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        self.execute_node(name, node, all_tasks).await
    }

    async fn execute_sequential(
        &self,
        prefix: &str,
        sequence: &[TaskNode],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let emit_lifecycle = !self.config.capture_output.should_capture();
        if emit_lifecycle {
            cuenv_events::emit_task_group_started!(prefix, true, sequence.len());
        }
        let started = std::time::Instant::now();
        let outcome = self
            .execute_sequential_inner(prefix, sequence, all_tasks)
            .await;
        if emit_lifecycle {
            emit_group_completion(prefix, started, &outcome, sequence.len());
        }
        outcome
    }

    async fn execute_sequential_inner(
        &self,
        prefix: &str,
        sequence: &[TaskNode],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut results = Vec::new();
        // Track completed step results for output ref resolution within sequences.
        let mut seq_results: std::collections::HashMap<String, TaskResult> =
            std::collections::HashMap::new();

        for (i, step) in sequence.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);

            // For Task steps, resolve any output ref placeholders from prior steps.
            // Only clone when placeholders are actually present to avoid
            // unnecessary allocations in the common (no-refs) case.
            let step = if let TaskNode::Task(task) = step
                && super::output_refs::has_output_refs(&task.args, &task.env)
            {
                let mut resolved_task = (**task).clone();
                let resolver = super::output_refs::OutputRefResolver {
                    task_name: &task_name,
                    results: &seq_results,
                };
                resolver.resolve(&mut resolved_task.args, &mut resolved_task.env)?;
                TaskNode::Task(Box::new(resolved_task))
            } else {
                step.clone()
            };

            let task_results = self.execute_node(&task_name, &step, all_tasks).await?;
            for result in &task_results {
                // Sequences always stop on first error (no configuration option)
                if !result.success {
                    return Err(Error::task_failed(
                        &result.name,
                        result.exit_code.unwrap_or(-1),
                        &result.stdout,
                        &result.stderr,
                    ));
                }
                seq_results.insert(result.name.clone(), result.clone());
            }
            results.extend(task_results);
        }
        Ok(results)
    }

    async fn execute_parallel(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let emit_lifecycle = !self.config.capture_output.should_capture();
        // Check for "default" task to override parallel execution
        if let Some(default_task) = group.children.get("default") {
            if emit_lifecycle {
                cuenv_events::emit_task_group_started!(prefix, true, 1_usize);
            }
            let started = std::time::Instant::now();
            // Execute only the default task, using the group prefix directly
            // since "default" is implicit when invoking the group name
            let task_name = format!("{}.default", prefix);
            let outcome = self.execute_node(&task_name, default_task, all_tasks).await;
            if emit_lifecycle {
                emit_group_completion(prefix, started, &outcome, 1);
            }
            return outcome;
        }

        if emit_lifecycle {
            cuenv_events::emit_task_group_started!(
                prefix,
                false,
                group.children.len(),
                None::<&str>,
                group.max_concurrency
            );
        }
        let started = std::time::Instant::now();
        let total_children = group.children.len();
        let outcome = self.execute_parallel_inner(prefix, group, all_tasks).await;
        if emit_lifecycle {
            emit_group_completion(prefix, started, &outcome, total_children);
        }
        outcome
    }

    async fn execute_parallel_inner(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());
        let mut all_results = Vec::new();
        let mut merge_results = |results: Vec<TaskResult>| -> Result<()> {
            if let Some(failed) = results.iter().find(|r| !r.success)
                && !self.config.continue_on_error
            {
                return Err(Error::task_failed(
                    &failed.name,
                    failed.exit_code.unwrap_or(-1),
                    &failed.stdout,
                    &failed.stderr,
                ));
            }
            all_results.extend(results);
            Ok(())
        };
        let max_parallel = group_concurrency_limit(self.config.max_parallel, group.max_concurrency);
        for (name, child_node) in &group.children {
            let task_name = format!("{}.{}", prefix, name);
            let child_node = child_node.clone();
            let all_tasks = Arc::clone(&all_tasks);
            let executor = self.clone_with_config();
            join_set.spawn(async move {
                executor
                    .execute_node(&task_name, &child_node, &all_tasks)
                    .await
            });
            if max_parallel > 0
                && join_set.len() >= max_parallel
                && let Some(result) = join_set.join_next().await
            {
                match result {
                    Ok(Ok(results)) => merge_results(results)?,
                    Ok(Err(e)) => return Err(e),
                    Err(e) => {
                        return Err(Error::execution(format!("Task execution panicked: {}", e)));
                    }
                }
            }
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => merge_results(results)?,
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    return Err(Error::execution(format!("Task execution panicked: {}", e)));
                }
            }
        }
        Ok(all_results)
    }

    #[instrument(name = "execute_graph", skip(self, graph), fields(task_count = graph.task_count()))]
    pub async fn execute_graph(&self, graph: &TaskGraph) -> Result<Vec<TaskResult>> {
        use super::graph_walk::{WalkPolicy, walk_parallel_graph};

        let policy = WalkPolicy {
            max_parallel: self.config.max_parallel,
            continue_on_error: self.config.continue_on_error,
        };

        let summary = walk_parallel_graph(
            graph.inner(),
            policy,
            // Resolve output-ref placeholders in args/env against tasks
            // completed in prior parallel groups. Intra-group siblings
            // are independent by definition so they don't appear here.
            |mut node, outcomes_so_far| -> Result<_> {
                // The walker tracks `TaskWalkOutcome` (result + continue_on_error),
                // but the resolver only needs the prior `TaskResult`s. Project to
                // them — and only when this node actually references an output, so
                // ref-free nodes don't pay for the copy.
                if super::output_refs::has_output_refs(&node.task.args, &node.task.env) {
                    let results: std::collections::HashMap<String, TaskResult> = outcomes_so_far
                        .iter()
                        .map(|(name, outcome)| (name.clone(), outcome.result.clone()))
                        .collect();
                    let resolver = super::output_refs::OutputRefResolver {
                        task_name: &node.name,
                        results: &results,
                    };
                    resolver.resolve(&mut node.task.args, &mut node.task.env)?;
                }
                Ok(node)
            },
            {
                let executor = Arc::new(self.clone_with_config());
                move |node| {
                    let executor = Arc::clone(&executor);
                    async move {
                        let continue_on_error = node.task.continue_on_error;
                        executor
                            .execute_task(&node.name, &node.task)
                            .await
                            .map(|result| TaskWalkOutcome {
                                result,
                                continue_on_error,
                            })
                    }
                }
            },
            |join_err| Error::execution(format!("Task execution panicked: {join_err}")),
        )
        .await?;

        let mut all_results: Vec<TaskResult> = summary
            .outcomes
            .into_iter()
            .map(|(_name, outcome)| outcome.result)
            .collect();

        // Under fail-fast, the walker short-circuits on the first failure
        // and we surface it via Err so callers see the failing task's
        // diagnostics. Under continue_on_error, every outcome is returned
        // and the first failure is surfaced via Err at the end so callers
        // still observe a non-Ok run.
        if !self.config.continue_on_error
            && let Some(failed) = all_results.iter().find(|r| !r.success).cloned()
        {
            return Err(Error::task_failed(
                &failed.name,
                failed.exit_code.unwrap_or(-1),
                &failed.stdout,
                &failed.stderr,
            ));
        }
        if self.config.continue_on_error
            && let Some(failed) = all_results.iter().find(|r| !r.success).cloned()
        {
            // Preserve historical ordering: when failing, only the
            // already-collected outcomes come back via Err; reset
            // all_results to avoid the caller "succeeding" with mixed
            // results.
            let err = Error::task_failed(
                &failed.name,
                failed.exit_code.unwrap_or(-1),
                &failed.stdout,
                &failed.stderr,
            );
            all_results.clear();
            return Err(err);
        }

        Ok(all_results)
    }

    fn clone_with_config(&self) -> Self {
        // Share the backend across clones to preserve container cache for Dagger chaining
        Self::with_shared_backend(self.config.clone(), self.backend.clone())
    }
}

#[derive(Clone)]
struct TaskWalkOutcome {
    result: TaskResult,
    continue_on_error: bool,
}

impl super::graph_walk::WalkOutcome for TaskWalkOutcome {
    fn is_success(&self) -> bool {
        self.result.success
    }

    fn continue_on_error(&self) -> bool {
        self.continue_on_error
    }
}

#[derive(Clone, Copy)]
struct CacheHitInput<'a> {
    name: &'a str,
    task: &'a Task,
    cache: &'a TaskCacheConfig,
    workdir: &'a Path,
    cached: &'a cuenv_cas::ActionResult,
}

fn emit_cached_output_events(name: &str, stream: &'static str, content: &str) {
    for line in content.lines() {
        cuenv_events::emit_task_output!(name, stream, line);
    }
}

fn group_concurrency_limit(global: usize, group: Option<u32>) -> usize {
    let group = group.and_then(|value| usize::try_from(value).ok());
    match (global, group) {
        (0, Some(group)) => group,
        (global, Some(group)) if group > 0 => global.min(group),
        (global, _) => global,
    }
}

fn parse_task_duration(field: &str, spec: &str) -> Result<Duration> {
    let raw = spec.trim();
    if raw.is_empty() {
        return Err(Error::configuration(format!("{field} must not be empty")));
    }

    let digits_len = raw.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    if digits_len == 0 || digits_len == raw.len() {
        return Err(Error::configuration(format!(
            "invalid task {field} '{raw}': expected <int><unit> (e.g. 30m, 1h)"
        )));
    }

    let quantity: u64 = raw[..digits_len]
        .parse()
        .map_err(|e| Error::configuration(format!("invalid task {field} '{raw}': {e}")))?;
    let unit = raw[digits_len..].trim().to_ascii_lowercase();

    // Convert `quantity` units of `factor` seconds each into a Duration,
    // rejecting overflow with the field/spec context already in scope.
    let secs = |factor: u64| -> Result<Duration> {
        quantity
            .checked_mul(factor)
            .map(Duration::from_secs)
            .ok_or_else(|| Error::configuration(format!("task {field} '{raw}' is too large")))
    };

    match unit.as_str() {
        "ms" => Ok(Duration::from_millis(quantity)),
        "s" => secs(1),
        "m" => secs(60),
        "h" => secs(60 * 60),
        "d" => secs(24 * 60 * 60),
        _ => Err(Error::configuration(format!(
            "invalid task {field} unit in '{raw}': use ms|s|m|h|d"
        ))),
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
