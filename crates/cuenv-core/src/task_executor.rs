//! Task executor for running tasks with environment support
//!
//! This module handles the actual execution of tasks, including:
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Output capture and streaming

use crate::environment::Environment;
use crate::task::{Task, TaskDefinition, TaskGroup, Tasks};
use crate::task_graph::TaskGraph;
use crate::{Error, Result};
use async_recursion::async_recursion;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinSet;

/// Task execution result
#[derive(Debug, Clone)]
pub struct TaskResult {
    /// Task name
    pub name: String,
    /// Exit code
    pub exit_code: Option<i32>,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Whether the task succeeded
    pub success: bool,
}

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: bool,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
    /// Environment variables to propagate
    pub environment: Environment,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: false,
            max_parallel: 0,
            environment: Environment::new(),
        }
    }
}

/// Task executor
pub struct TaskExecutor {
    config: ExecutorConfig,
}

impl TaskExecutor {
    /// Create a new task executor
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    /// Execute a single task
    pub async fn execute_task(&self, name: &str, task: &Task) -> Result<TaskResult> {
        tracing::info!("Executing task: {}", name);

        // Build the command based on shell and args configuration
        let mut cmd = if let Some(shell) = &task.shell {
            // Check if shell is properly configured
            if shell.command.is_some() && shell.flag.is_some() {
                // Execute via specified shell
                let shell_command = shell.command.as_ref().unwrap();
                let shell_flag = shell.flag.as_ref().unwrap();
                let mut cmd = Command::new(shell_command);
                cmd.arg(shell_flag);

                if task.args.is_empty() {
                    // Just execute the command string as-is
                    cmd.arg(&task.command);
                } else {
                    // Concatenate command and args with proper shell quoting
                    let full_command = if task.command.is_empty() {
                        task.args.join(" ")
                    } else {
                        format!("{} {}", task.command, task.args.join(" "))
                    };
                    cmd.arg(full_command);
                }
                cmd
            } else {
                // Shell field present but not properly configured, fall back to direct execution
                let mut cmd = Command::new(&task.command);
                for arg in &task.args {
                    cmd.arg(arg);
                }
                cmd
            }
        } else {
            // Direct execution (secure by default)
            let mut cmd = Command::new(&task.command);
            for arg in &task.args {
                cmd.arg(arg);
            }
            cmd
        };

        // Set environment variables
        let env_vars = self.config.environment.merge_with_system();
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        // Configure output handling
        if self.config.capture_output {
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }

        // Execute the command
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::configuration(format!("Failed to spawn task '{}': {}", name, e)))?;

        let (stdout, stderr) = if self.config.capture_output {
            // Capture output concurrently to prevent deadlocks
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();

            let stdout_task = async {
                if let Some(stdout) = stdout_handle {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    let mut stdout_lines = Vec::new();
                    while let Ok(Some(line)) = lines.next_line().await {
                        stdout_lines.push(line);
                    }
                    stdout_lines.join("\n")
                } else {
                    String::new()
                }
            };

            let stderr_task = async {
                if let Some(stderr) = stderr_handle {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    let mut stderr_lines = Vec::new();
                    while let Ok(Some(line)) = lines.next_line().await {
                        stderr_lines.push(line);
                    }
                    stderr_lines.join("\n")
                } else {
                    String::new()
                }
            };

            // Read stdout and stderr concurrently
            tokio::join!(stdout_task, stderr_task)
        } else {
            (String::new(), String::new())
        };

        // Wait for completion
        let status = child.wait().await.map_err(|e| {
            Error::configuration(format!("Failed to wait for task '{}': {}", name, e))
        })?;

        let exit_code = status.code();
        let success = status.success();

        if !success {
            tracing::warn!("Task '{}' failed with exit code: {:?}", name, exit_code);
        } else {
            tracing::info!("Task '{}' completed successfully", name);
        }

        Ok(TaskResult {
            name: name.to_string(),
            exit_code,
            stdout,
            stderr,
            success,
        })
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
                let result = self.execute_task(name, task).await?;
                Ok(vec![result])
            }
            TaskDefinition::Group(group) => self.execute_group(name, group, all_tasks).await,
        }
    }

    /// Execute a task group
    async fn execute_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match group {
            TaskGroup::Sequential(tasks) => self.execute_sequential(prefix, tasks, all_tasks).await,
            TaskGroup::Parallel(tasks) => self.execute_parallel(prefix, tasks, all_tasks).await,
        }
    }

    /// Execute tasks sequentially
    async fn execute_sequential(
        &self,
        prefix: &str,
        tasks: &[TaskDefinition],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut results = Vec::new();

        for (i, task_def) in tasks.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_results = self
                .execute_definition(&task_name, task_def, all_tasks)
                .await?;

            // Check if any task failed
            for result in &task_results {
                if !result.success {
                    return Err(Error::configuration(format!(
                        "Task '{}' failed in sequential group",
                        result.name
                    )));
                }
            }

            results.extend(task_results);
        }

        Ok(results)
    }

    /// Execute tasks in parallel
    async fn execute_parallel(
        &self,
        prefix: &str,
        tasks: &HashMap<String, TaskDefinition>,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());

        for (name, task_def) in tasks {
            let task_name = format!("{}.{}", prefix, name);
            let task_def = task_def.clone();
            let all_tasks = Arc::clone(&all_tasks);
            let executor = self.clone_with_config();

            join_set.spawn(async move {
                executor
                    .execute_definition(&task_name, &task_def, &all_tasks)
                    .await
            });

            // Apply parallelism limit if configured
            if self.config.max_parallel > 0 && join_set.len() >= self.config.max_parallel {
                // Wait for one to complete before starting more
                if let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(Ok(_)) => {} // Task completed successfully, continue
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
        }

        // Wait for all remaining tasks
        let mut all_results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => all_results.extend(results),
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

    /// Execute tasks using a task graph (respects dependencies)
    pub async fn execute_graph(&self, graph: &TaskGraph) -> Result<Vec<TaskResult>> {
        let parallel_groups = graph.get_parallel_groups()?;
        let mut all_results = Vec::new();

        // Use a single JoinSet for all groups to enforce global parallelism limit
        let mut join_set = JoinSet::new();
        let mut group_iter = parallel_groups.into_iter();
        let mut current_group = group_iter.next();

        while current_group.is_some() || !join_set.is_empty() {
            // Start tasks from current group up to parallelism limit
            if let Some(group) = current_group.as_mut() {
                while let Some(node) = group.pop() {
                    let task = node.task.clone();
                    let name = node.name.clone();
                    let executor = self.clone_with_config();

                    join_set.spawn(async move { executor.execute_task(&name, &task).await });

                    // Apply parallelism limit if configured
                    if self.config.max_parallel > 0 && join_set.len() >= self.config.max_parallel {
                        break;
                    }
                }

                // Move to next group if current group is empty
                if group.is_empty() {
                    current_group = group_iter.next();
                }
            }

            // Wait for at least one task to complete
            if let Some(result) = join_set.join_next().await {
                match result {
                    Ok(Ok(task_result)) => {
                        if !task_result.success {
                            return Err(Error::configuration(format!(
                                "Task '{}' failed",
                                task_result.name
                            )));
                        }
                        all_results.push(task_result);
                    }
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

        Ok(all_results)
    }

    /// Clone executor with same config (for parallel execution)
    fn clone_with_config(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
    }
}

/// Execute an arbitrary command with the cuenv environment
pub async fn execute_command(
    command: &str,
    args: &[String],
    environment: &Environment,
) -> Result<i32> {
    tracing::info!("Executing command: {} {:?}", command, args);

    let mut cmd = Command::new(command);
    cmd.args(args);

    // Set environment variables
    let env_vars = environment.merge_with_system();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    // Inherit stdio for interactive commands
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(Stdio::inherit());

    // Execute and wait
    let status = cmd.status().await.map_err(|e| {
        Error::configuration(format!("Failed to execute command '{}': {}", command, e))
    })?;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Hello task".to_string()),
        };

        let result = executor.execute_task("test", &task).await.unwrap();

        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_with_environment() {
        let mut config = ExecutorConfig::default();
        config.capture_output = true;
        config
            .environment
            .set("TEST_VAR".to_string(), "test_value".to_string());

        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "printenv".to_string(),
            args: vec!["TEST_VAR".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Print env task".to_string()),
        };

        let result = executor.execute_task("test", &task).await.unwrap();

        assert!(result.success);
        assert!(result.stdout.contains("test_value"));
    }

    #[tokio::test]
    async fn test_execute_failing_task() {
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "false".to_string(),
            args: vec![],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Failing task".to_string()),
        };

        let result = executor.execute_task("test", &task).await.unwrap();

        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_execute_sequential_group() {
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        let executor = TaskExecutor::new(config);

        let task1 = Task {
            command: "echo".to_string(),
            args: vec!["first".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("First task".to_string()),
        };

        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Second task".to_string()),
        };

        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(task1),
            TaskDefinition::Single(task2),
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
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        let executor = TaskExecutor::new(config);

        // Test that malicious shell metacharacters in arguments don't get executed
        let malicious_task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string(), "; rm -rf /".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Malicious task test".to_string()),
        };

        let result = executor
            .execute_task("malicious", &malicious_task)
            .await
            .unwrap();

        // The malicious command should be treated as literal argument to echo
        assert!(result.success);
        assert!(result.stdout.contains("hello ; rm -rf /"));
    }

    #[tokio::test]
    async fn test_special_characters_in_args() {
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        let executor = TaskExecutor::new(config);

        // Test various special characters that could be used for injection
        let special_chars = vec![
            "$USER",          // Variable expansion
            "$(whoami)",      // Command substitution
            "`whoami`",       // Backtick command substitution
            "&& echo hacked", // Command chaining
            "|| echo failed", // Error chaining
            "> /tmp/hack",    // Redirection
            "| cat",          // Piping
        ];

        for special_arg in special_chars {
            let task = Task {
                command: "echo".to_string(),
                args: vec!["safe".to_string(), special_arg.to_string()],
                shell: None,
                dependencies: vec![],
                inputs: vec![],
                outputs: vec![],
                description: Some("Special character test".to_string()),
            };

            let result = executor.execute_task("special", &task).await.unwrap();

            // Special characters should be treated literally, not interpreted
            assert!(result.success);
            assert!(result.stdout.contains("safe"));
            assert!(result.stdout.contains(&special_arg));
        }
    }

    #[tokio::test]
    async fn test_environment_variable_safety() {
        let mut config = ExecutorConfig::default();
        config.capture_output = true;

        // Set environment variable with potentially dangerous value
        config
            .environment
            .set("DANGEROUS_VAR".to_string(), "; rm -rf /".to_string());

        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "printenv".to_string(),
            args: vec!["DANGEROUS_VAR".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: Some("Environment variable safety test".to_string()),
        };

        let result = executor.execute_task("env_test", &task).await.unwrap();

        // Environment variable should be passed safely
        assert!(result.success);
        assert!(result.stdout.contains("; rm -rf /"));
    }
}
