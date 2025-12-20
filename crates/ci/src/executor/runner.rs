//! IR Task Runner
//!
//! Executes individual IR tasks with proper command handling, environment
//! injection, and output capture.

use crate::ir::Task as IRTask;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

/// Error types for task execution
#[derive(Debug, Error)]
pub enum RunnerError {
    /// Task command is empty
    #[error("Task '{task}' has empty command")]
    EmptyCommand { task: String },

    /// Process spawn failed
    #[error("Failed to spawn task '{task}': {source}")]
    SpawnFailed {
        task: String,
        #[source]
        source: std::io::Error,
    },

    /// Process execution failed
    #[error("Task '{task}' execution failed: {source}")]
    ExecutionFailed {
        task: String,
        #[source]
        source: std::io::Error,
    },
}

/// Output from task execution
#[derive(Debug, Clone)]
pub struct TaskOutput {
    /// Task ID
    pub task_id: String,
    /// Process exit code
    pub exit_code: i32,
    /// Captured stdout
    pub stdout: String,
    /// Captured stderr
    pub stderr: String,
    /// Whether the task succeeded
    pub success: bool,
    /// Whether result was from cache
    pub from_cache: bool,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

impl TaskOutput {
    /// Create a cached result (no actual execution)
    #[must_use]
    pub fn from_cache(task_id: String, duration_ms: u64) -> Self {
        Self {
            task_id,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            success: true,
            from_cache: true,
            duration_ms,
        }
    }

    /// Create a dry-run result
    #[must_use]
    pub fn dry_run(task_id: String) -> Self {
        Self {
            task_id,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            success: true,
            from_cache: false,
            duration_ms: 0,
        }
    }
}

/// Default shell path for task execution
pub const DEFAULT_SHELL: &str = "/bin/sh";

/// Runner for executing IR tasks
pub struct IRTaskRunner {
    /// Working directory for task execution
    project_root: PathBuf,
    /// Whether to capture output
    capture_output: bool,
    /// Shell path for shell-mode execution
    shell_path: String,
}

impl IRTaskRunner {
    /// Create a new task runner with default shell
    #[must_use]
    pub fn new(project_root: PathBuf, capture_output: bool) -> Self {
        Self {
            project_root,
            capture_output,
            shell_path: DEFAULT_SHELL.to_string(),
        }
    }

    /// Create a new task runner with custom shell path
    #[must_use]
    pub fn with_shell(
        project_root: PathBuf,
        capture_output: bool,
        shell_path: impl Into<String>,
    ) -> Self {
        Self {
            project_root,
            capture_output,
            shell_path: shell_path.into(),
        }
    }

    /// Execute a single IR task
    ///
    /// # Arguments
    /// * `task` - The IR task definition
    /// * `env` - Environment variables to inject (includes resolved secrets)
    ///
    /// # Errors
    /// Returns error if the task command is empty or execution fails
    #[tracing::instrument(
        name = "execute_task",
        fields(task_id = %task.id, shell = task.shell),
        skip(self, env)
    )]
    pub async fn execute(
        &self,
        task: &IRTask,
        env: HashMap<String, String>,
    ) -> Result<TaskOutput, RunnerError> {
        if task.command.is_empty() {
            return Err(RunnerError::EmptyCommand {
                task: task.id.clone(),
            });
        }

        let start = std::time::Instant::now();

        // Build command based on shell mode
        let mut cmd = if task.shell {
            // Shell mode: wrap command in shell -c
            let shell_cmd = task.command.join(" ");
            tracing::debug!(shell_cmd = %shell_cmd, shell = %self.shell_path, "Running in shell mode");

            let mut c = Command::new(&self.shell_path);
            c.arg("-c");
            c.arg(&shell_cmd);
            c
        } else {
            // Direct mode: execve
            tracing::debug!(cmd = ?task.command, "Running in direct mode");

            let mut c = Command::new(&task.command[0]);
            if task.command.len() > 1 {
                c.args(&task.command[1..]);
            }
            c
        };

        // Set working directory
        cmd.current_dir(&self.project_root);

        // Clear environment and inject our variables
        cmd.env_clear();
        for (k, v) in &env {
            cmd.env(k, v);
        }

        // Also inject essential env vars
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }

        // Configure output capture
        if self.capture_output {
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }

        // Execute
        tracing::info!(task = %task.id, "Starting task execution");

        let output = cmd
            .output()
            .await
            .map_err(|e| RunnerError::ExecutionFailed {
                task: task.id.clone(),
                source: e,
            })?;

        let duration = start.elapsed();
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            task = %task.id,
            exit_code = exit_code,
            success = success,
            duration_ms,
            "Task execution completed"
        );

        Ok(TaskOutput {
            task_id: task.id.clone(),
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success,
            from_cache: false,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::CachePolicy;
    use tempfile::TempDir;

    fn make_task(id: &str, command: Vec<&str>, shell: bool) -> IRTask {
        IRTask {
            id: id.to_string(),
            runtime: None,
            command: command.iter().map(|s| (*s).to_string()).collect(),
            shell,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        }
    }

    #[tokio::test]
    async fn test_simple_command() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        let task = make_task("test", vec!["echo", "hello"], false);

        let result = runner.execute(&task, HashMap::new()).await.unwrap();

        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(!result.from_cache);
    }

    #[tokio::test]
    async fn test_shell_mode() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        let task = make_task("test", vec!["echo", "hello", "&&", "echo", "world"], true);

        let result = runner.execute(&task, HashMap::new()).await.unwrap();

        assert!(result.success);
        assert!(result.stdout.contains("hello"));
        assert!(result.stdout.contains("world"));
    }

    #[tokio::test]
    async fn test_env_injection() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        let task = make_task("test", vec!["printenv", "MY_VAR"], false);

        let env = HashMap::from([("MY_VAR".to_string(), "test_value".to_string())]);
        let result = runner.execute(&task, env).await.unwrap();

        assert!(result.success);
        assert!(result.stdout.contains("test_value"));
    }

    #[tokio::test]
    async fn test_failing_command() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        let task = make_task("test", vec!["false"], false);

        let result = runner.execute(&task, HashMap::new()).await.unwrap();

        assert!(!result.success);
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_empty_command_error() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        let task = make_task("test", vec![], false);

        let result = runner.execute(&task, HashMap::new()).await;
        assert!(matches!(result, Err(RunnerError::EmptyCommand { .. })));
    }

    #[test]
    fn test_cached_output() {
        let output = TaskOutput::from_cache("test".to_string(), 100);
        assert!(output.success);
        assert!(output.from_cache);
        assert_eq!(output.duration_ms, 100);
    }

    #[test]
    fn test_dry_run_output() {
        let output = TaskOutput::dry_run("test".to_string());
        assert!(output.success);
        assert!(!output.from_cache);
        assert_eq!(output.duration_ms, 0);
    }

    #[tokio::test]
    #[ignore = "requires /bin/bash which may not exist in sandboxed builds"]
    async fn test_custom_shell() {
        let tmp = TempDir::new().unwrap();
        // Use /bin/bash (available on most Unix systems)
        let runner = IRTaskRunner::with_shell(tmp.path().to_path_buf(), true, "/bin/bash");
        let task = make_task("test", vec!["echo", "$BASH_VERSION"], true);

        let result = runner.execute(&task, HashMap::new()).await.unwrap();

        // On systems with bash, this should succeed and output something
        assert!(result.success);
    }

    #[test]
    fn test_runner_default_shell() {
        let tmp = TempDir::new().unwrap();
        let runner = IRTaskRunner::new(tmp.path().to_path_buf(), true);
        assert_eq!(runner.shell_path, "/bin/sh");
    }
}
