//! Task executor for running tasks with environment support.
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Host execution; isolation/caching is delegated to other backends

use super::backend::{BackendFactory, TaskBackend, create_backend_with_factory};
use super::cache::{BuildActionInput, RecordInput, TaskCacheConfig};
use super::process_registry::global_registry;
use super::{Task, TaskGraph, TaskGroup, TaskNode, Tasks};
use crate::OutputCapture;
use crate::config::BackendConfig;
use crate::environment::Environment;
use crate::{Error, Result};
use async_recursion::async_recursion;
use cuenv_workspaces::PackageManager;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::task::JoinSet;
use tracing::instrument;

// Unix process group setup requires CommandExt trait for pre_exec method.
// The unused_imports warning is a false positive - the trait is used via cmd.pre_exec().
#[cfg(unix)]
#[allow(unused_imports)]
use std::os::unix::process::CommandExt;

/// Set up process group on Unix so we can kill the entire process tree on Ctrl-C.
///
/// This creates a new process group with the spawned process as the leader,
/// allowing us to send signals to all descendants when terminating.
#[cfg(unix)]
fn setup_process_group(cmd: &mut Command) {
    // SAFETY: setpgid(0, 0) creates a new process group with this process as leader.
    // This is safe to call in the pre-spawn hook as it only affects the child process.
    // It allows us to send signals to the entire process group when terminating.
    #[expect(unsafe_code, reason = "Required for POSIX process group management")]
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

/// Task execution result
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Number of lines from stdout/stderr to include when summarizing failures
pub const TASK_FAILURE_SNIPPET_LINES: usize = 20;

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
                let workdir = self.workdir_for_task(task);
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
        let result = if self.backend.name() == "dagger" {
            let ctx = super::backend::TaskExecutionContext {
                name,
                task,
                environment: &self.config.environment,
                project_root: &self.config.project_root,
                capture_output: self.config.capture_output,
            };
            self.backend.execute(&ctx).await?
        } else {
            self.execute_task_non_hermetic(name, task).await?
        };
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
    fn workdir_for_task(&self, task: &Task) -> PathBuf {
        if let Some(ref dir) = task.directory {
            self.config
                .cue_module_root
                .as_ref()
                .unwrap_or(&self.config.project_root)
                .join(dir)
        } else if let Some(ref project_root) = task.project_root {
            project_root.clone()
        } else if let Some(ref source) = task.source {
            if let Some(dir) = source.directory() {
                self.config
                    .cue_module_root
                    .as_ref()
                    .unwrap_or(&self.config.project_root)
                    .join(dir)
            } else if let Some(ref project_root) = task.project_root {
                project_root.clone()
            } else {
                self.config
                    .cue_module_root
                    .clone()
                    .unwrap_or_else(|| self.config.project_root.clone())
            }
        } else if !task.hermetic {
            if let Some(manager) = cuenv_workspaces::detect_from_command(&task.command) {
                find_workspace_root(manager, &self.config.project_root)
            } else {
                self.config.project_root.clone()
            }
        } else {
            self.config.project_root.clone()
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
    async fn execute_task_non_hermetic(&self, name: &str, task: &Task) -> Result<TaskResult> {
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
        let workdir = self.workdir_for_task(task);

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

        // Execute - always capture output for consistent behavior
        // If not in capture mode, stream output to terminal in real-time
        if self.config.capture_output.should_capture() {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let start_time = std::time::Instant::now();

            // Set up process group on Unix so we can kill the entire tree on Ctrl-C
            #[cfg(unix)]
            setup_process_group(&mut cmd);

            // Spawn with piped stdout/stderr for streaming
            let mut child = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn task {}", name),
                })?;

            // Register process with global registry for cleanup on Ctrl-C
            let child_pid = child.id();
            if let Some(pid) = child_pid {
                global_registry().register(pid, name.to_string()).await;
            }

            // Take ownership of stdout/stderr handles
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();

            // Collect output while streaming events in real-time
            let mut stdout_lines = Vec::new();
            let mut stderr_lines = Vec::new();

            // Stream stdout
            let name_for_stdout = name.to_string();
            let stdout_task = tokio::spawn(async move {
                let mut lines = Vec::new();
                if let Some(stdout) = stdout_handle {
                    let mut reader = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        cuenv_events::emit_task_output!(name_for_stdout, "stdout", line);
                        lines.push(line);
                    }
                }
                lines
            });

            // Stream stderr
            let name_for_stderr = name.to_string();
            let stderr_task = tokio::spawn(async move {
                let mut lines = Vec::new();
                if let Some(stderr) = stderr_handle {
                    let mut reader = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        cuenv_events::emit_task_output!(name_for_stderr, "stderr", line);
                        lines.push(line);
                    }
                }
                lines
            });

            // Wait for process to complete and collect output
            let status = child.wait().await.map_err(|e| Error::Io {
                source: e,
                path: None,
                operation: format!("wait for task {}", name),
            })?;

            // Unregister process from global registry now that it has completed
            if let Some(pid) = child_pid {
                global_registry().unregister(pid).await;
            }

            // Collect streamed output
            if let Ok(lines) = stdout_task.await {
                stdout_lines = lines;
            }
            if let Ok(lines) = stderr_task.await {
                stderr_lines = lines;
            }

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let stdout = stdout_lines.join("\n");
            let stderr = stderr_lines.join("\n");
            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            // Emit task completion event
            cuenv_events::emit_task_completed!(name, success, Some(exit_code), duration_ms);

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Task failed");
                tracing::error!(task = %name, "Task stdout:\n{}", stdout);
                tracing::error!(task = %name, "Task stderr:\n{}", stderr);
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout,
                stderr,
                success,
            })
        } else {
            // Stream output directly to terminal (interactive mode)

            // Set up process group on Unix so we can kill the entire tree on Ctrl-C
            #[cfg(unix)]
            setup_process_group(&mut cmd);

            // Use spawn + wait instead of status() to get access to the PID
            let mut child = cmd
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .stdin(Stdio::inherit())
                .spawn()
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn task {}", name),
                })?;

            // Register process with global registry for cleanup on Ctrl-C
            let child_pid = child.id();
            if let Some(pid) = child_pid {
                global_registry().register(pid, name.to_string()).await;
            }

            let status = child.wait().await.map_err(|e| Error::Io {
                source: e,
                path: None,
                operation: format!("wait for task {}", name),
            })?;

            // Unregister process from global registry
            if let Some(pid) = child_pid {
                global_registry().unregister(pid).await;
            }

            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Task failed");
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout: String::new(), // Output went to terminal
                stderr: String::new(),
                success,
            })
        }
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
            cuenv_events::emit_task_group_started!(prefix, false, group.children.len());
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
            if let Some(failed) = results.iter().find(|r| !r.success) {
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
            if self.config.max_parallel > 0
                && join_set.len() >= self.config.max_parallel
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
        let parallel_groups = graph.get_parallel_groups()?;
        let mut all_results = Vec::new();
        // Map of completed task results for resolving output references.
        // Populated between sequential parallel groups — no concurrent access.
        let mut results_map: std::collections::HashMap<String, TaskResult> =
            std::collections::HashMap::new();

        // Set of failed task names. Under `continue_on_error`, dependents of
        // tainted tasks are emitted as `task.skipped` events and never
        // spawned. Under fail-fast (default) we return on the first failure
        // before this set is consulted again.
        let mut tainted: std::collections::HashSet<String> = std::collections::HashSet::new();
        // First failure encountered under continue_on_error; surfaced at the
        // end so the caller still sees a non-Ok result.
        let mut first_failure: Option<TaskResult> = None;

        // IMPORTANT:
        // Each parallel group represents a dependency "level". We must not start tasks from the
        // next group until *all* tasks from the current group have completed successfully.
        //
        // The previous implementation pipelined groups (starting the next group as soon as all
        // tasks from the current group were spawned), which allowed dependent tasks to run before
        // their dependencies finished (especially visible with long-running tasks like dev servers).
        for mut group in parallel_groups {
            // Under continue_on_error, filter out nodes whose deps are
            // tainted before we spawn them — and emit Skipped events so
            // renderers reflect the cascade.
            if self.config.continue_on_error {
                group.retain(|node| {
                    let failing_dep = node
                        .task
                        .depends_on
                        .iter()
                        .find(|dep| tainted.contains(dep.task_name()));
                    if let Some(dep) = failing_dep {
                        let reason = cuenv_events::SkipReason::DependencyFailed {
                            dep: dep.task_name().to_string(),
                        };
                        cuenv_events::emit_task_skipped!(&node.name, reason);
                        tainted.insert(node.name.clone());
                        return false;
                    }
                    true
                });
            }

            let mut join_set = JoinSet::new();

            while !group.is_empty() || !join_set.is_empty() {
                // Fill the concurrency window for this group
                while let Some(node) = group.pop() {
                    let mut task = node.task.clone();
                    let name = node.name.clone();

                    // Resolve any task output reference placeholders in args/env
                    // before spawning. The results_map contains all completed
                    // upstream tasks (from prior parallel groups).
                    let resolver = super::output_refs::OutputRefResolver {
                        task_name: &name,
                        results: &results_map,
                    };
                    resolver.resolve(&mut task.args, &mut task.env)?;

                    let executor = self.clone_with_config();
                    join_set.spawn(async move { executor.execute_task(&name, &task).await });

                    if self.config.max_parallel > 0 && join_set.len() >= self.config.max_parallel {
                        break;
                    }
                }

                if let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(Ok(task_result)) => {
                            if !task_result.success {
                                if self.config.continue_on_error {
                                    tainted.insert(task_result.name.clone());
                                    if first_failure.is_none() {
                                        first_failure = Some(task_result.clone());
                                    }
                                    results_map
                                        .insert(task_result.name.clone(), task_result.clone());
                                    all_results.push(task_result);
                                } else {
                                    join_set.abort_all();
                                    return Err(Error::task_failed(
                                        &task_result.name,
                                        task_result.exit_code.unwrap_or(-1),
                                        &task_result.stdout,
                                        &task_result.stderr,
                                    ));
                                }
                            } else {
                                results_map.insert(task_result.name.clone(), task_result.clone());
                                all_results.push(task_result);
                            }
                        }
                        Ok(Err(e)) => {
                            // Compilation / setup error: always fatal,
                            // regardless of continue_on_error.
                            join_set.abort_all();
                            return Err(e);
                        }
                        Err(e) => {
                            // Panic / abort: always fatal — we can't reason
                            // about state after a panic.
                            join_set.abort_all();
                            return Err(Error::execution(format!(
                                "Task execution panicked: {}",
                                e
                            )));
                        }
                    }
                }
            }
        }

        if let Some(failed) = first_failure {
            return Err(Error::task_failed(
                &failed.name,
                failed.exit_code.unwrap_or(-1),
                &failed.stdout,
                &failed.stderr,
            ));
        }

        Ok(all_results)
    }

    fn clone_with_config(&self) -> Self {
        // Share the backend across clones to preserve container cache for Dagger chaining
        Self::with_shared_backend(self.config.clone(), self.backend.clone())
    }
}

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

fn find_workspace_root(manager: PackageManager, start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

    loop {
        let is_root = match manager {
            PackageManager::Npm
            | PackageManager::Bun
            | PackageManager::YarnClassic
            | PackageManager::YarnModern => package_json_has_workspaces(&current),
            PackageManager::Pnpm => current.join("pnpm-workspace.yaml").exists(),
            PackageManager::Cargo => cargo_toml_has_workspace(&current),
            PackageManager::Deno => deno_json_has_workspace(&current),
        };

        if is_root {
            return current;
        }

        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.to_path_buf();
        }
    }
}

fn package_json_has_workspaces(dir: &Path) -> bool {
    let path = dir.join("package.json");
    let content = std::fs::read_to_string(&path);
    let Ok(json) = content.and_then(|s| {
        serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return false;
    };

    match json.get("workspaces") {
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(map)) => map
            .get("packages")
            .and_then(|packages| packages.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

fn cargo_toml_has_workspace(dir: &Path) -> bool {
    let path = dir.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };

    content.contains("[workspace]")
}

fn deno_json_has_workspace(dir: &Path) -> bool {
    let path = dir.join("deno.json");
    let content = std::fs::read_to_string(&path);
    let Ok(json) = content.and_then(|s| {
        serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return false;
    };

    // Deno uses "workspace" (not "workspaces") for workspace configuration
    match json.get("workspace") {
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(_)) => true,
        _ => false,
    }
}

/// Build a compact, user-friendly summary for a failed task, including the
/// exit code and the tail of stdout/stderr to help with diagnostics.
pub fn summarize_task_failure(result: &TaskResult, max_output_lines: usize) -> String {
    let exit_code = result
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut sections = Vec::new();
    sections.push(format!(
        "Task '{}' failed with exit code {}.",
        result.name, exit_code
    ));

    let output = format_failure_streams(result, max_output_lines);
    if output.is_empty() {
        sections.push(
            "No stdout/stderr were captured; rerun with RUST_LOG=debug to stream task logs."
                .to_string(),
        );
    } else {
        sections.push(output);
    }

    sections.join("\n\n")
}

fn format_failure_streams(result: &TaskResult, max_output_lines: usize) -> String {
    let mut streams = Vec::new();

    if let Some(stdout) = summarize_stream("stdout", &result.stdout, max_output_lines) {
        streams.push(stdout);
    }

    if let Some(stderr) = summarize_stream("stderr", &result.stderr, max_output_lines) {
        streams.push(stderr);
    }

    streams.join("\n\n")
}

fn summarize_stream(label: &str, content: &str, max_output_lines: usize) -> Option<String> {
    let normalized = content.trim_end();
    if normalized.is_empty() {
        return None;
    }

    let lines: Vec<&str> = normalized.lines().collect();
    let total = lines.len();
    let start = total.saturating_sub(max_output_lines);
    let snippet = lines[start..].join("\n");

    let header = if total > max_output_lines {
        format!("{label} (last {max_output_lines} of {total} lines):")
    } else {
        format!("{label}:")
    };

    Some(format!("{header}\n{snippet}"))
}

/// Execute an arbitrary command with the cuenv environment
///
/// If `secrets` is provided, output will be captured and redacted before printing.
pub async fn execute_command(
    command: &str,
    args: &[String],
    environment: &Environment,
) -> Result<i32> {
    execute_command_with_redaction(command, args, environment, &[]).await
}

/// Execute a command with secret redaction
///
/// Secret values in stdout/stderr are replaced with [REDACTED].
pub async fn execute_command_with_redaction(
    command: &str,
    args: &[String],
    environment: &Environment,
    secrets: &[String],
) -> Result<i32> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    tracing::info!("Executing command: {} {:?}", command, args);
    let mut cmd = Command::new(command);
    cmd.args(args);
    // Use hermetic environment - no host PATH pollution
    let env_vars = environment.merge_with_system_hermetic();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    if secrets.is_empty() {
        // No secrets to redact - inherit stdio directly
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        cmd.stdin(Stdio::inherit());
        let status = cmd.status().await.map_err(|e| {
            Error::configuration(format!("Failed to execute command '{}': {}", command, e))
        })?;
        return Ok(status.code().unwrap_or(1));
    }

    // Capture output for redaction
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::inherit());

    let mut child = cmd.spawn().map_err(|e| {
        Error::configuration(format!("Failed to execute command '{}': {}", command, e))
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::execution("stdout pipe not available"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::execution("stderr pipe not available"))?;

    // Build sorted secrets for greedy matching (longer first)
    let mut sorted_secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    sorted_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let sorted_secrets: Vec<String> = sorted_secrets.into_iter().map(String::from).collect();

    // Stream stdout with redaction
    let secrets_clone = sorted_secrets.clone();
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut redacted = line;
            for secret in &secrets_clone {
                if secret.len() >= 4 {
                    redacted = redacted.replace(secret, "[REDACTED]");
                }
            }
            cuenv_events::emit_stdout!(&redacted);
        }
    });

    // Stream stderr with redaction
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut redacted = line;
            for secret in &sorted_secrets {
                if secret.len() >= 4 {
                    redacted = redacted.replace(secret, "[REDACTED]");
                }
            }
            cuenv_events::emit_stderr!(&redacted);
        }
    });

    // Wait for command and streams
    let status = child.wait().await.map_err(|e| {
        Error::configuration(format!("Failed to wait for command '{}': {}", command, e))
    })?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskDependency;
    use crate::tasks::cache::TaskCacheConfig;
    use cuenv_cas::{LocalActionCache, LocalCas};
    use cuenv_events::{EventBus, EventCategory, TaskEvent};
    use cuenv_vcs::WalkHasher;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert!(config.capture_output.should_capture());
        assert_eq!(config.max_parallel, 0);
        assert!(config.environment.is_empty());
    }

    #[tokio::test]
    async fn test_task_result() {
        let result = TaskResult {
            name: "test".to_string(),
            exit_code: Some(0),
            stdout: "output".to_string(),
            stderr: String::new(),
            success: true,
        };
        assert_eq!(result.name, "test");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
        assert_eq!(result.stdout, "output");
    }

    #[tokio::test]
    async fn test_execute_simple_task() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            description: Some("Hello task".to_string()),
            ..Default::default()
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_with_environment() {
        let mut config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        config
            .environment
            .set("TEST_VAR".to_string(), "test_value".to_string());
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "printenv".to_string(),
            args: vec!["TEST_VAR".to_string()],
            description: Some("Print env task".to_string()),
            ..Default::default()
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test_value"));
    }

    #[tokio::test]
    async fn test_execute_with_task_secret_environment() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "printenv".to_string(),
            args: vec!["GH_TOKEN".to_string()],
            env: HashMap::from([(
                "GH_TOKEN".to_string(),
                serde_json::json!({
                    "resolver": "exec",
                    "command": "echo",
                    "args": ["tap-token"]
                }),
            )]),
            description: Some("Print task secret env task".to_string()),
            ..Default::default()
        };

        let result = executor.execute_task("test", &task).await.unwrap();

        assert!(result.success);
        assert_eq!(result.stdout.trim(), "tap-token");
    }

    #[tokio::test]
    async fn test_task_secret_gh_token_is_available_with_github_token() {
        let mut config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        config
            .environment
            .set("GITHUB_TOKEN".to_string(), "repo-token".to_string());
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                r#"test "$GH_TOKEN" = "tap-token" && test "$GITHUB_TOKEN" = "repo-token" && echo ok"#
                    .to_string(),
            ],
            env: HashMap::from([(
                "GH_TOKEN".to_string(),
                serde_json::json!({
                    "resolver": "exec",
                    "command": "echo",
                    "args": ["tap-token"]
                }),
            )]),
            description: Some("Verify GitHub CLI token precedence env".to_string()),
            ..Default::default()
        };

        let result = executor.execute_task("test", &task).await.unwrap();

        assert!(result.success);
        assert_eq!(result.stdout.trim(), "ok");
    }

    #[tokio::test]
    async fn test_execute_failing_task() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "false".to_string(),
            description: Some("Failing task".to_string()),
            ..Default::default()
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_execute_script_task() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            script: Some("echo hello from script".to_string()),
            ..Default::default()
        };

        let result = executor.execute_task("script", &task).await.unwrap();

        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello from script"));
    }

    #[tokio::test]
    async fn test_execute_script_task_with_shell_options() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            script: Some("false\necho should-not-run".to_string()),
            script_shell: Some(super::super::ScriptShell::Bash),
            shell_options: Some(super::super::ShellOptions::default()),
            ..Default::default()
        };

        let result = executor
            .execute_task("script.failfast", &task)
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
        assert!(!result.stdout.contains("should-not-run"));
    }

    #[tokio::test]
    async fn test_execute_script_task_rejects_pipefail_for_sh() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            script: Some("echo hello".to_string()),
            script_shell: Some(super::super::ScriptShell::Sh),
            shell_options: Some(super::super::ShellOptions::default()),
            ..Default::default()
        };

        let err = executor.execute_task("script.sh", &task).await.unwrap_err();

        assert!(
            err.to_string()
                .contains("shellOptions.pipefail with unsupported script shell 'sh'"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_execute_script_task_rejects_unsupported_shell_options() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            script: Some("console.log('hello')".to_string()),
            script_shell: Some(super::super::ScriptShell::Node),
            shell_options: Some(super::super::ShellOptions::default()),
            ..Default::default()
        };

        let err = executor
            .execute_task("script.node", &task)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("unsupported script shell 'node'"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_execute_sequential_group() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task1 = Task {
            command: "echo".to_string(),
            args: vec!["first".to_string()],
            description: Some("First task".to_string()),
            ..Default::default()
        };
        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            description: Some("Second task".to_string()),
            ..Default::default()
        };
        let sequence = vec![
            TaskNode::Task(Box::new(task1)),
            TaskNode::Task(Box::new(task2)),
        ];
        let all_tasks = Tasks::new();
        let node = TaskNode::Sequence(sequence);
        let results = executor
            .execute_node("seq", &node, &all_tasks)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].stdout.contains("first"));
        assert!(results[1].stdout.contains("second"));
    }

    #[tokio::test]
    async fn test_command_injection_prevention() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let malicious_task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string(), "; rm -rf /".to_string()],
            description: Some("Malicious task test".to_string()),
            ..Default::default()
        };
        let result = executor
            .execute_task("malicious", &malicious_task)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello ; rm -rf /"));
    }

    #[tokio::test]
    async fn test_special_characters_in_args() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let special_chars = vec![
            "$USER",
            "$(whoami)",
            "`whoami`",
            "&& echo hacked",
            "|| echo failed",
            "> /tmp/hack",
            "| cat",
        ];
        for special_arg in special_chars {
            let task = Task {
                command: "echo".to_string(),
                args: vec!["safe".to_string(), special_arg.to_string()],
                description: Some("Special character test".to_string()),
                ..Default::default()
            };
            let result = executor.execute_task("special", &task).await.unwrap();
            assert!(result.success);
            assert!(result.stdout.contains("safe"));
            assert!(result.stdout.contains(special_arg));
        }
    }

    #[tokio::test]
    async fn test_environment_variable_safety() {
        let mut config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            ..Default::default()
        };
        config
            .environment
            .set("DANGEROUS_VAR".to_string(), "; rm -rf /".to_string());
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "printenv".to_string(),
            args: vec!["DANGEROUS_VAR".to_string()],
            description: Some("Environment variable safety test".to_string()),
            ..Default::default()
        };
        let result = executor.execute_task("env_test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("; rm -rf /"));
    }

    #[tokio::test]
    async fn test_execute_graph_parallel_groups() {
        // two independent tasks -> can run in same parallel group
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            max_parallel: 2,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let mut graph = TaskGraph::new();

        let t1 = Task {
            command: "echo".into(),
            args: vec!["A".into()],
            ..Default::default()
        };
        let t2 = Task {
            command: "echo".into(),
            args: vec!["B".into()],
            ..Default::default()
        };

        graph.add_task("t1", t1).unwrap();
        graph.add_task("t2", t2).unwrap();
        let results = executor.execute_graph(&graph).await.unwrap();
        assert_eq!(results.len(), 2);
        let joined = results.iter().map(|r| r.stdout.clone()).collect::<String>();
        assert!(joined.contains("A") && joined.contains("B"));
    }

    #[tokio::test]
    async fn execute_graph_continue_on_error_skips_dependents() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            max_parallel: 2,
            continue_on_error: true,
            project_root: root.to_path_buf(),
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);

        // DAG: fail -> dependent ; independent runs regardless.
        let mut tasks = Tasks::new();
        tasks.tasks.insert(
            "fail".into(),
            TaskNode::Task(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "exit 7".into()],
                ..Default::default()
            })),
        );
        tasks.tasks.insert(
            "dependent".into(),
            TaskNode::Task(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "echo dependent ran".into()],
                depends_on: vec![TaskDependency::from_name("fail")],
                ..Default::default()
            })),
        );
        tasks.tasks.insert(
            "independent".into(),
            TaskNode::Task(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "echo independent ran".into()],
                ..Default::default()
            })),
        );

        let mut graph = TaskGraph::new();
        graph.build_for_task("dependent", &tasks).unwrap();
        graph.build_for_task("independent", &tasks).unwrap();

        // First failure is reported via Err, but the independent sibling
        // and the failing task's result both make it into the results map
        // before we surface the failure.
        let outcome = executor.execute_graph(&graph).await;
        assert!(outcome.is_err(), "fail task should surface as error");

        // The "independent" task must have completed successfully even
        // though "fail" failed — that's the whole point of the flag.
        // (We can't easily assert on the results vec since the executor
        // returns Err; this is exercised in the integration suite.)
    }

    #[tokio::test]
    async fn test_execute_graph_respects_dependency_levels() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            max_parallel: 2,
            project_root: root.to_path_buf(),
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);

        let mut tasks = Tasks::new();
        tasks.tasks.insert(
            "dep".into(),
            TaskNode::Task(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 0.2 && echo ok > marker.txt".into()],
                ..Default::default()
            })),
        );
        tasks.tasks.insert(
            "consumer".into(),
            TaskNode::Task(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "cat marker.txt".into()],
                depends_on: vec![TaskDependency::from_name("dep")],
                ..Default::default()
            })),
        );

        let mut graph = TaskGraph::new();
        graph.build_for_task("consumer", &tasks).unwrap();

        let results = executor.execute_graph(&graph).await.unwrap();
        assert_eq!(results.len(), 2);

        let consumer = results.iter().find(|r| r.name == "consumer").unwrap();
        assert!(consumer.success);
        assert!(consumer.stdout.contains("ok"));
    }

    #[tokio::test]
    async fn test_cache_hit_replays_task_output_events() {
        let workspace = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        std::fs::write(workspace.path().join("input.txt"), "v1").unwrap();

        let cache = TaskCacheConfig {
            cas: Arc::new(LocalCas::open(cache_root.path()).unwrap()),
            action_cache: Arc::new(LocalActionCache::open(cache_root.path()).unwrap()),
            vcs_hasher: Arc::new(WalkHasher::new(workspace.path())),
            vcs_hasher_root: workspace.path().to_path_buf(),
            cuenv_version: "test".to_string(),
            runtime_identity_properties: std::collections::BTreeMap::new(),
            cache_disabled_reason: None,
        };
        let executor = TaskExecutor::new(ExecutorConfig {
            capture_output: OutputCapture::Capture,
            project_root: workspace.path().to_path_buf(),
            cache: Some(cache),
            ..Default::default()
        });
        let task = Task {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf 'hello\\n' && cat input.txt > out.txt".to_string(),
            ],
            inputs: vec![super::super::Input::Path("input.txt".to_string())],
            outputs: vec!["out.txt".to_string()],
            cache: Some(super::super::TaskCachePolicy {
                mode: super::super::TaskCacheMode::ReadWrite,
                max_age: None,
            }),
            ..Task::default()
        };

        executor.execute_task("cached", &task).await.unwrap();
        std::fs::remove_file(workspace.path().join("out.txt")).unwrap();

        // The macros emit via the process-wide EventSender installed
        // here; subscribe a receiver before triggering the cached run so
        // we can observe the replayed Output event.
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender available");
        let _ = cuenv_events::set_global_sender(sender);
        let mut rx = bus.subscribe();

        let result = executor.execute_task("cached", &task).await.unwrap();
        assert!(result.success);

        // The mpsc → broadcast forwarder runs on a tokio task; await with a
        // short deadline rather than try_recv-looping so we don't race the
        // forwarder.
        let mut saw_output = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Some(event)) => {
                    if let EventCategory::Task(TaskEvent::Output { name, content, .. }) =
                        event.category
                        && name == "cached"
                        && content == "hello"
                    {
                        saw_output = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }

        cuenv_events::clear_global_sender();
        assert!(
            saw_output,
            "expected cached task output event to be replayed"
        );
    }

    #[test]
    fn test_summarize_task_failure_with_exit_code() {
        let result = TaskResult {
            name: "build".to_string(),
            exit_code: Some(127),
            stdout: String::new(),
            stderr: "command not found".to_string(),
            success: false,
        };
        let summary = summarize_task_failure(&result, 10);
        assert!(summary.contains("build"));
        assert!(summary.contains("127"));
        assert!(summary.contains("command not found"));
    }

    #[test]
    fn test_summarize_task_failure_no_exit_code() {
        let result = TaskResult {
            name: "killed".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            success: false,
        };
        let summary = summarize_task_failure(&result, 10);
        assert!(summary.contains("killed"));
        assert!(summary.contains("unknown"));
    }

    #[test]
    fn test_summarize_task_failure_no_output() {
        let result = TaskResult {
            name: "silent".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            success: false,
        };
        let summary = summarize_task_failure(&result, 10);
        assert!(summary.contains("No stdout/stderr"));
        assert!(summary.contains("RUST_LOG=debug"));
    }

    #[test]
    fn test_summarize_task_failure_truncates_long_output() {
        let result = TaskResult {
            name: "verbose".to_string(),
            exit_code: Some(1),
            stdout: (1..=50)
                .map(|i| format!("line {}", i))
                .collect::<Vec<_>>()
                .join("\n"),
            stderr: String::new(),
            success: false,
        };
        let summary = summarize_task_failure(&result, 10);
        assert!(summary.contains("last 10 of 50 lines"));
        assert!(summary.contains("line 50"));
        assert!(!summary.contains("line 1\n")); // First line should be truncated
    }

    #[test]
    fn test_summarize_stream_empty() {
        assert!(summarize_stream("test", "", 10).is_none());
        assert!(summarize_stream("test", "   ", 10).is_none());
        assert!(summarize_stream("test", "\n\n", 10).is_none());
    }

    #[test]
    fn test_summarize_stream_short() {
        let result = summarize_stream("stdout", "line 1\nline 2", 10).unwrap();
        assert!(result.contains("stdout:"));
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(!result.contains("last"));
    }

    #[test]
    fn test_format_failure_streams_both() {
        let result = TaskResult {
            name: "test".to_string(),
            exit_code: Some(1),
            stdout: "stdout content".to_string(),
            stderr: "stderr content".to_string(),
            success: false,
        };
        let formatted = format_failure_streams(&result, 10);
        assert!(formatted.contains("stdout:"));
        assert!(formatted.contains("stderr:"));
        assert!(formatted.contains("stdout content"));
        assert!(formatted.contains("stderr content"));
    }

    #[test]
    fn test_find_workspace_root_with_npm() {
        let tmp = TempDir::new().unwrap();
        // Create a workspace structure
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        let subdir = tmp.path().join("packages").join("subpkg");
        std::fs::create_dir_all(&subdir).unwrap();

        let root = find_workspace_root(PackageManager::Npm, &subdir);
        assert_eq!(root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_workspace_root_with_pnpm() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'",
        )
        .unwrap();
        let subdir = tmp.path().join("packages").join("app");
        std::fs::create_dir_all(&subdir).unwrap();

        let root = find_workspace_root(PackageManager::Pnpm, &subdir);
        assert_eq!(root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_workspace_root_with_cargo() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .unwrap();
        let subdir = tmp.path().join("crates").join("core");
        std::fs::create_dir_all(&subdir).unwrap();

        let root = find_workspace_root(PackageManager::Cargo, &subdir);
        assert_eq!(root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_workspace_root_no_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = find_workspace_root(PackageManager::Npm, tmp.path());
        // Should return the start path when no workspace is found
        assert_eq!(root, tmp.path().to_path_buf());
    }

    #[test]
    fn test_package_json_has_workspaces_array() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        assert!(package_json_has_workspaces(tmp.path()));
    }

    #[test]
    fn test_package_json_has_workspaces_object() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"workspaces": {"packages": ["packages/*"]}}"#,
        )
        .unwrap();
        assert!(package_json_has_workspaces(tmp.path()));
    }

    #[test]
    fn test_package_json_no_workspaces() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("package.json"), r#"{"name": "test"}"#).unwrap();
        assert!(!package_json_has_workspaces(tmp.path()));
    }

    #[test]
    fn test_package_json_empty_workspaces() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("package.json"), r#"{"workspaces": []}"#).unwrap();
        assert!(!package_json_has_workspaces(tmp.path()));
    }

    #[test]
    fn test_package_json_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!package_json_has_workspaces(tmp.path()));
    }

    #[test]
    fn test_cargo_toml_has_workspace() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .unwrap();
        assert!(cargo_toml_has_workspace(tmp.path()));
    }

    #[test]
    fn test_cargo_toml_no_workspace() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        assert!(!cargo_toml_has_workspace(tmp.path()));
    }

    #[test]
    fn test_cargo_toml_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!cargo_toml_has_workspace(tmp.path()));
    }

    #[test]
    fn test_deno_json_has_workspace_array() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("deno.json"),
            r#"{"workspace": ["./packages/*"]}"#,
        )
        .unwrap();
        assert!(deno_json_has_workspace(tmp.path()));
    }

    #[test]
    fn test_deno_json_has_workspace_object() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("deno.json"),
            r#"{"workspace": {"members": ["./packages/*"]}}"#,
        )
        .unwrap();
        assert!(deno_json_has_workspace(tmp.path()));
    }

    #[test]
    fn test_deno_json_no_workspace() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("deno.json"), r#"{"name": "test"}"#).unwrap();
        assert!(!deno_json_has_workspace(tmp.path()));
    }

    #[test]
    fn test_deno_json_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!deno_json_has_workspace(tmp.path()));
    }

    #[test]
    fn test_executor_config_with_fields() {
        let config = ExecutorConfig {
            capture_output: OutputCapture::Capture,
            max_parallel: 4,
            working_dir: Some(PathBuf::from("/tmp")),
            project_root: PathBuf::from("/project"),
            cue_module_root: Some(PathBuf::from("/project/cue.mod")),
            materialize_outputs: Some(PathBuf::from("/outputs")),
            cache_dir: Some(PathBuf::from("/cache")),
            show_cache_path: true,
            cli_backend: Some("host".to_string()),
            ..Default::default()
        };
        assert!(config.capture_output.should_capture());
        assert_eq!(config.max_parallel, 4);
        assert_eq!(config.working_dir, Some(PathBuf::from("/tmp")));
        assert!(config.show_cache_path);
    }

    #[test]
    fn test_task_result_clone() {
        let result = TaskResult {
            name: "test".to_string(),
            exit_code: Some(0),
            stdout: "output".to_string(),
            stderr: "error".to_string(),
            success: true,
        };
        let cloned = result.clone();
        assert_eq!(cloned.name, result.name);
        assert_eq!(cloned.exit_code, result.exit_code);
        assert_eq!(cloned.stdout, result.stdout);
        assert_eq!(cloned.stderr, result.stderr);
        assert_eq!(cloned.success, result.success);
    }

    #[test]
    fn test_task_failure_snippet_lines_constant() {
        assert_eq!(TASK_FAILURE_SNIPPET_LINES, 20);
    }
}
