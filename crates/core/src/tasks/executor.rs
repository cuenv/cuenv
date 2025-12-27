//! Task executor for running tasks with environment support.
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Host execution; isolation/caching is delegated to other backends

use super::backend::{BackendFactory, TaskBackend, create_backend_with_factory};
use super::{ParallelGroup, Task, TaskDefinition, TaskGraph, TaskGroup, Tasks};
use crate::config::BackendConfig;
use crate::environment::Environment;
use crate::manifest::WorkspaceConfig;
use crate::{Error, Result};
use async_recursion::async_recursion;
use cuenv_workspaces::PackageManager;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::task::JoinSet;

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

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: bool,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
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
    /// Global workspace configuration
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,
    /// Backend configuration
    pub backend_config: Option<BackendConfig>,
    /// CLI backend selection override
    pub cli_backend: Option<String>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: false,
            max_parallel: 0,
            environment: Environment::new(),
            working_dir: None,
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cue_module_root: None,
            materialize_outputs: None,
            cache_dir: None,
            show_cache_path: false,
            workspaces: None,
            backend_config: None,
            cli_backend: None,
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

    /// Execute a single task
    pub async fn execute_task(&self, name: &str, task: &Task) -> Result<TaskResult> {
        // Delegate execution to the configured backend.
        // The backend implementation handles the specific execution details.

        // If using Dagger backend, execute in a containerized context.
        if self.backend.name() == "dagger" {
            return self
                .backend
                .execute(
                    name,
                    task,
                    &self.config.environment,
                    &self.config.project_root,
                    self.config.capture_output,
                )
                .await;
        }

        // Host backend runs tasks directly in the workspace.
        self.execute_task_non_hermetic(name, task).await
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

        // Determine working directory (in priority order):
        // 1. Explicit directory field on task (relative to cue.mod root)
        // 2. TaskRef project_root (from resolution)
        // 3. Source file directory (from _source metadata)
        // 4. Install tasks (hermetic: false with workspaces) run from workspace root
        // 5. Default to project root
        let workdir = if let Some(ref dir) = task.directory {
            // Explicit directory override: resolve relative to cue.mod root or project root
            self.config
                .cue_module_root
                .as_ref()
                .unwrap_or(&self.config.project_root)
                .join(dir)
        } else if let Some(ref project_root) = task.project_root {
            // TaskRef tasks run in their original project directory
            project_root.clone()
        } else if let Some(ref source) = task.source {
            // Default: run in the directory of the source file
            if let Some(dir) = source.directory() {
                self.config
                    .cue_module_root
                    .as_ref()
                    .unwrap_or(&self.config.project_root)
                    .join(dir)
            } else {
                // Source is at root (e.g., "env.cue"), use cue_module_root if available
                // This ensures tasks defined in root env.cue run from module root,
                // even when invoked from a subdirectory
                self.config
                    .cue_module_root
                    .clone()
                    .unwrap_or_else(|| self.config.project_root.clone())
            }
        } else if !task.hermetic && task.workspaces.as_ref().is_some_and(|ws| !ws.is_empty()) {
            // Find workspace root for install tasks
            let workspace_name = &task.workspaces.as_ref().unwrap()[0];
            let manager = match workspace_name.as_str() {
                "bun" => PackageManager::Bun,
                "npm" => PackageManager::Npm,
                "pnpm" => PackageManager::Pnpm,
                "yarn" => PackageManager::YarnModern,
                "cargo" => PackageManager::Cargo,
                _ => PackageManager::Npm, // fallback
            };
            find_workspace_root(manager, &self.config.project_root)
        } else {
            self.config.project_root.clone()
        };

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
        let mut cmd = if let Some(script) = &task.script {
            // Script mode: use shell to execute the script
            let (shell_cmd, shell_flag) = if let Some(shell) = &task.shell {
                (
                    shell.command.clone().unwrap_or_else(|| "bash".to_string()),
                    shell.flag.clone().unwrap_or_else(|| "-c".to_string()),
                )
            } else {
                // Default to bash for scripts
                ("bash".to_string(), "-c".to_string())
            };

            let resolved_shell = self.config.environment.resolve_command(&shell_cmd);
            let mut cmd = Command::new(&resolved_shell);
            cmd.arg(&shell_flag);
            cmd.arg(script);
            cmd
        } else {
            // Command mode: existing behavior
            let resolved_command = self.config.environment.resolve_command(&task.command);

            if let Some(shell) = &task.shell {
                if let (Some(shell_command), Some(shell_flag)) = (&shell.command, &shell.flag) {
                    let resolved_shell = self.config.environment.resolve_command(shell_command);
                    let mut cmd = Command::new(&resolved_shell);
                    cmd.arg(shell_flag);
                    if task.args.is_empty() {
                        cmd.arg(&resolved_command);
                    } else {
                        let full_command = if task.command.is_empty() {
                            task.args.join(" ")
                        } else {
                            format!("{} {}", resolved_command, task.args.join(" "))
                        };
                        cmd.arg(full_command);
                    }
                    cmd
                } else {
                    let mut cmd = Command::new(&resolved_command);
                    for arg in &task.args {
                        cmd.arg(arg);
                    }
                    cmd
                }
            } else {
                let mut cmd = Command::new(&resolved_command);
                for arg in &task.args {
                    cmd.arg(arg);
                }
                cmd
            }
        };

        // Set working directory and environment
        cmd.current_dir(&workdir);
        let env_vars = self.config.environment.merge_with_system();
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }

        // Execute - always capture output for consistent behavior
        // If not in capture mode, stream output to terminal in real-time
        if self.config.capture_output {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let start_time = std::time::Instant::now();

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
            cuenv_events::emit_task_completed!(name, success, exit_code, duration_ms);

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
            let status = cmd
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn task {}", name),
                })?;

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

    /// Execute a task definition (single task or group)
    #[async_recursion]
    pub async fn execute_definition(
        &self,
        name: &str,
        definition: &TaskDefinition,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match definition {
            TaskDefinition::Single(task) => {
                let result = self.execute_task(name, task.as_ref()).await?;
                Ok(vec![result])
            }
            TaskDefinition::Group(group) => self.execute_group(name, group, all_tasks).await,
        }
    }

    async fn execute_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match group {
            TaskGroup::Sequential(tasks) => self.execute_sequential(prefix, tasks, all_tasks).await,
            TaskGroup::Parallel(group) => self.execute_parallel(prefix, group, all_tasks).await,
        }
    }

    async fn execute_sequential(
        &self,
        prefix: &str,
        tasks: &[TaskDefinition],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        if !self.config.capture_output {
            cuenv_events::emit_task_group_started!(prefix, true, tasks.len());
        }
        let mut results = Vec::new();
        for (i, task_def) in tasks.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_results = self
                .execute_definition(&task_name, task_def, all_tasks)
                .await?;
            for result in &task_results {
                if !result.success {
                    let message = format!(
                        "Sequential task group '{prefix}' halted.\n\n{}",
                        summarize_task_failure(result, TASK_FAILURE_SNIPPET_LINES)
                    );
                    return Err(Error::configuration(message));
                }
            }
            results.extend(task_results);
        }
        Ok(results)
    }

    async fn execute_parallel(
        &self,
        prefix: &str,
        group: &ParallelGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        // Check for "default" task to override parallel execution
        if let Some(default_task) = group.tasks.get("default") {
            if !self.config.capture_output {
                cuenv_events::emit_task_group_started!(prefix, true, 1_usize);
            }
            // Execute only the default task, using the group prefix directly
            // since "default" is implicit when invoking the group name
            let task_name = format!("{}.default", prefix);
            return self
                .execute_definition(&task_name, default_task, all_tasks)
                .await;
        }

        if !self.config.capture_output {
            cuenv_events::emit_task_group_started!(prefix, false, group.tasks.len());
        }
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());
        let mut all_results = Vec::new();
        let mut merge_results = |results: Vec<TaskResult>| -> Result<()> {
            if let Some(failed) = results.iter().find(|r| !r.success) {
                let message = format!(
                    "Parallel task group '{prefix}' halted.\n\n{}",
                    summarize_task_failure(failed, TASK_FAILURE_SNIPPET_LINES)
                );
                return Err(Error::configuration(message));
            }
            all_results.extend(results);
            Ok(())
        };
        for (name, task_def) in &group.tasks {
            let task_name = format!("{}.{}", prefix, name);
            let task_def = task_def.clone();
            let all_tasks = Arc::clone(&all_tasks);
            let executor = self.clone_with_config();
            join_set.spawn(async move {
                executor
                    .execute_definition(&task_name, &task_def, &all_tasks)
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
                        return Err(Error::configuration(format!(
                            "Task execution panicked: {}",
                            e
                        )));
                    }
                }
            }
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => merge_results(results)?,
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    return Err(Error::configuration(format!(
                        "Task execution panicked: {}",
                        e
                    )));
                }
            }
        }
        Ok(all_results)
    }

    pub async fn execute_graph(&self, graph: &TaskGraph) -> Result<Vec<TaskResult>> {
        let parallel_groups = graph.get_parallel_groups()?;
        let mut all_results = Vec::new();

        // IMPORTANT:
        // Each parallel group represents a dependency "level". We must not start tasks from the
        // next group until *all* tasks from the current group have completed successfully.
        //
        // The previous implementation pipelined groups (starting the next group as soon as all
        // tasks from the current group were spawned), which allowed dependent tasks to run before
        // their dependencies finished (especially visible with long-running tasks like dev servers).
        for mut group in parallel_groups {
            let mut join_set = JoinSet::new();

            while !group.is_empty() || !join_set.is_empty() {
                // Fill the concurrency window for this group
                while let Some(node) = group.pop() {
                    let task = node.task.clone();
                    let name = node.name.clone();
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
                                join_set.abort_all();
                                let message = format!(
                                    "Task graph execution halted.\n\n{}",
                                    summarize_task_failure(
                                        &task_result,
                                        TASK_FAILURE_SNIPPET_LINES,
                                    )
                                );
                                return Err(Error::configuration(message));
                            }
                            all_results.push(task_result);
                        }
                        Ok(Err(e)) => {
                            join_set.abort_all();
                            return Err(e);
                        }
                        Err(e) => {
                            join_set.abort_all();
                            return Err(Error::configuration(format!(
                                "Task execution panicked: {}",
                                e
                            )));
                        }
                    }
                }
            }
        }

        Ok(all_results)
    }

    fn clone_with_config(&self) -> Self {
        // Share the backend across clones to preserve container cache for Dagger chaining
        Self::with_shared_backend(self.config.clone(), self.backend.clone())
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
    let env_vars = environment.merge_with_system();
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
    use crate::tasks::Input;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert!(!config.capture_output);
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
            capture_output: true,
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
            capture_output: true,
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
    async fn test_workspace_inputs_include_workspace_root_when_project_is_nested() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Workspace root with workspaces + lockfile
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "root-app",
  "version": "0.0.0",
  "workspaces": ["packages/*", "apps/*"],
  "dependencies": {
    "@rawkodeacademy/content-technologies": "workspace:*"
  }
}"#,
        )
        .unwrap();
        // Deliberately omit the workspace member name for apps/site to mimic lockfiles
        // that only record member paths, ensuring we can still discover dependencies.
        fs::write(
            root.join("bun.lock"),
            r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "name": "root-app",
      "dependencies": {
        "@rawkodeacademy/content-technologies": "workspace:*"
      }
    },
    "packages/content-technologies": {
      "name": "@rawkodeacademy/content-technologies",
      "version": "0.0.1"
    },
    "apps/site": {
      "version": "0.0.0",
      "dependencies": {
        "@rawkodeacademy/content-technologies": "workspace:*"
      }
    }
  },
  "packages": {}
}"#,
        )
        .unwrap();

        // Workspace member packages
        fs::create_dir_all(root.join("packages/content-technologies")).unwrap();
        fs::write(
            root.join("packages/content-technologies/package.json"),
            r#"{
  "name": "@rawkodeacademy/content-technologies",
  "version": "0.0.1"
}"#,
        )
        .unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(
            root.join("apps/site/package.json"),
            r#"{
  "name": "site",
  "version": "0.0.0",
  "dependencies": {
    "@rawkodeacademy/content-technologies": "workspace:*"
  }
}"#,
        )
        .unwrap();

        let mut workspaces = HashMap::new();
        workspaces.insert(
            "bun".to_string(),
            WorkspaceConfig {
                enabled: true,
                package_manager: Some("bun".to_string()),
                root: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::new(),
            },
        );

        let config = ExecutorConfig {
            capture_output: true,
            project_root: root.join("apps/site"),
            workspaces: Some(workspaces),
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "find ../.. -maxdepth 4 -type d | sort".to_string(),
            ],
            inputs: vec![Input::Path("package.json".to_string())],
            workspaces: Some(vec!["bun".to_string()]),
            ..Default::default()
        };

        let result = executor.execute_task("install", &task).await.unwrap();
        assert!(
            result.success,
            "command failed stdout='{}' stderr='{}'",
            result.stdout, result.stderr
        );
        assert!(
            result
                .stdout
                .split_whitespace()
                .any(|line| line.ends_with("packages/content-technologies")),
            "should include workspace member from workspace root; stdout='{}' stderr='{}'",
            result.stdout,
            result.stderr
        );
    }

    #[tokio::test]
    async fn test_execute_failing_task() {
        let config = ExecutorConfig {
            capture_output: true,
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
    async fn test_execute_sequential_group() {
        let config = ExecutorConfig {
            capture_output: true,
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
        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(Box::new(task1)),
            TaskDefinition::Single(Box::new(task2)),
        ]);
        let all_tasks = Tasks::new();
        let results = executor
            .execute_group("seq", &group, &all_tasks)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].stdout.contains("first"));
        assert!(results[1].stdout.contains("second"));
    }

    #[tokio::test]
    async fn test_command_injection_prevention() {
        let config = ExecutorConfig {
            capture_output: true,
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
            capture_output: true,
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
            capture_output: true,
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
            capture_output: true,
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
    async fn test_execute_graph_respects_dependency_levels() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let config = ExecutorConfig {
            capture_output: true,
            max_parallel: 2,
            project_root: root.to_path_buf(),
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);

        let mut tasks = Tasks::new();
        tasks.tasks.insert(
            "dep".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 0.2 && echo ok > marker.txt".into()],
                ..Default::default()
            })),
        );
        tasks.tasks.insert(
            "consumer".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "sh".into(),
                args: vec!["-c".into(), "cat marker.txt".into()],
                depends_on: vec!["dep".into()],
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
}
