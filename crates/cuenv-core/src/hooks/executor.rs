//! Hook execution engine with background processing and state management

use crate::hooks::state::{compute_directory_hash, HookExecutionState, StateManager};
use crate::hooks::types::{ExecutionStatus, Hook, HookExecutionConfig, HookResult};
use crate::{Error, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Manages hook execution with background processing and state persistence
#[derive(Debug)]
pub struct HookExecutor {
    config: HookExecutionConfig,
    state_manager: StateManager,
    /// Semaphore for limiting concurrent hook executions
    concurrency_limiter: Arc<Semaphore>,
    /// Set of whitelisted commands for security
    allowed_commands: Arc<RwLock<HashSet<String>>>,
    /// Active background tasks for cancellation support
    active_tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

use std::collections::HashMap;

impl HookExecutor {
    /// Create a new hook executor with the specified configuration
    pub fn new(config: HookExecutionConfig) -> Result<Self> {
        let state_dir = if let Some(dir) = config.state_dir.clone() {
            dir
        } else {
            StateManager::default_state_dir()?
        };

        let state_manager = StateManager::new(state_dir);
        let concurrency_limiter = Arc::new(Semaphore::new(config.max_concurrent));

        // Default allowed commands - can be customized
        let mut allowed_commands = HashSet::new();
        // Common safe commands
        allowed_commands.insert("echo".to_string());
        allowed_commands.insert("pwd".to_string());
        allowed_commands.insert("ls".to_string());
        allowed_commands.insert("cat".to_string());
        allowed_commands.insert("grep".to_string());
        allowed_commands.insert("find".to_string());
        allowed_commands.insert("date".to_string());
        allowed_commands.insert("env".to_string());
        allowed_commands.insert("true".to_string());
        allowed_commands.insert("false".to_string());
        allowed_commands.insert("sleep".to_string());

        // Development tools
        allowed_commands.insert("npm".to_string());
        allowed_commands.insert("yarn".to_string());
        allowed_commands.insert("pnpm".to_string());
        allowed_commands.insert("node".to_string());
        allowed_commands.insert("python".to_string());
        allowed_commands.insert("python3".to_string());
        allowed_commands.insert("pip".to_string());
        allowed_commands.insert("pip3".to_string());
        allowed_commands.insert("cargo".to_string());
        allowed_commands.insert("rustc".to_string());
        allowed_commands.insert("go".to_string());
        allowed_commands.insert("make".to_string());
        allowed_commands.insert("cmake".to_string());
        allowed_commands.insert("gcc".to_string());
        allowed_commands.insert("clang".to_string());
        allowed_commands.insert("git".to_string());
        allowed_commands.insert("docker".to_string());
        allowed_commands.insert("docker-compose".to_string());
        allowed_commands.insert("kubectl".to_string());
        allowed_commands.insert("terraform".to_string());
        allowed_commands.insert("ansible".to_string());

        Ok(Self {
            config,
            state_manager,
            concurrency_limiter,
            allowed_commands: Arc::new(RwLock::new(allowed_commands)),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Add a command to the whitelist
    pub async fn allow_command(&self, command: String) {
        let mut allowed = self.allowed_commands.write().await;
        allowed.insert(command);
    }

    /// Remove a command from the whitelist
    pub async fn disallow_command(&self, command: &str) {
        let mut allowed = self.allowed_commands.write().await;
        allowed.remove(command);
    }

    /// Check if a command is allowed
    async fn is_command_allowed(&self, command: &str) -> bool {
        let allowed = self.allowed_commands.read().await;
        allowed.contains(command)
    }

    /// Create a hook executor with default configuration
    pub fn with_default_config() -> Result<Self> {
        Self::new(HookExecutionConfig::default())
    }

    /// Start executing hooks in the background for a directory
    pub async fn execute_hooks_background(
        &self,
        directory_path: PathBuf,
        config_hash: String,
        hooks: Vec<Hook>,
    ) -> Result<String> {
        if hooks.is_empty() {
            return Ok("No hooks to execute".to_string());
        }

        let directory_hash = compute_directory_hash(&directory_path);
        let total_hooks = hooks.len();

        // Create initial execution state
        let mut state = HookExecutionState::new(
            directory_path.clone(),
            directory_hash.clone(),
            config_hash,
            total_hooks,
        );

        // Save initial state
        self.state_manager.save_state(&state).await?;

        info!(
            "Starting background execution of {} hooks for directory: {}",
            total_hooks,
            directory_path.display()
        );

        // Clone necessary data for the background task
        let state_manager = self.state_manager.clone();
        let config = self.config.clone();
        let concurrency_limiter = self.concurrency_limiter.clone();
        let allowed_commands = self.allowed_commands.clone();
        let active_tasks = self.active_tasks.clone();
        let task_key = directory_hash.clone();

        // Spawn background execution task with proper tracking
        let handle = tokio::spawn(async move {
            let result = execute_hooks_concurrent(
                hooks,
                &directory_path,
                &config,
                &state_manager,
                &mut state,
                concurrency_limiter,
                allowed_commands,
            )
            .await;

            if let Err(e) = result {
                error!("Hook execution failed: {}", e);
                state.status = ExecutionStatus::Failed;
                state.error_message = Some(e.to_string());
                state.finished_at = Some(chrono::Utc::now());

                if let Err(save_err) = state_manager.save_state(&state).await {
                    error!("Failed to save error state: {}", save_err);
                }
            }

            // Remove from active tasks when done
            let mut tasks = active_tasks.lock().await;
            tasks.remove(&task_key);
        });

        // Track the task for potential cancellation
        let mut tasks = self.active_tasks.lock().await;
        tasks.insert(directory_hash, handle);

        Ok(format!(
            "Started execution of {} hooks in background",
            total_hooks
        ))
    }

    /// Get the current execution status for a directory
    pub async fn get_execution_status(
        &self,
        directory_path: &Path,
    ) -> Result<Option<HookExecutionState>> {
        let directory_hash = compute_directory_hash(directory_path);
        self.state_manager.load_state(&directory_hash).await
    }

    /// Wait for hook execution to complete, with optional timeout in seconds
    pub async fn wait_for_completion(
        &self,
        directory_path: &Path,
        timeout_seconds: Option<u64>,
    ) -> Result<HookExecutionState> {
        let directory_hash = compute_directory_hash(directory_path);
        let poll_interval = Duration::from_millis(500);
        let start_time = Instant::now();

        loop {
            if let Some(state) = self.state_manager.load_state(&directory_hash).await? {
                if state.is_complete() {
                    return Ok(state);
                }
            } else {
                return Err(Error::configuration("No execution state found"));
            }

            // Check timeout
            if let Some(timeout) = timeout_seconds
                && start_time.elapsed().as_secs() >= timeout
            {
                return Err(Error::Timeout { seconds: timeout });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Cancel execution for a directory
    pub async fn cancel_execution(
        &self,
        directory_path: &Path,
        reason: Option<String>,
    ) -> Result<bool> {
        let directory_hash = compute_directory_hash(directory_path);

        // First, try to abort the background task if it exists
        let mut tasks = self.active_tasks.lock().await;
        if let Some(handle) = tasks.remove(&directory_hash) {
            handle.abort();
            info!(
                "Aborted background task for directory: {}",
                directory_path.display()
            );
        }
        drop(tasks); // Release the lock early

        // Then update the state
        if let Some(mut state) = self.state_manager.load_state(&directory_hash).await?
            && !state.is_complete()
        {
            state.mark_cancelled(reason);
            self.state_manager.save_state(&state).await?;
            info!(
                "Cancelled execution for directory: {}",
                directory_path.display()
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Clean up completed execution states older than the specified duration
    pub async fn cleanup_old_states(&self, older_than: chrono::Duration) -> Result<usize> {
        let states = self.state_manager.list_active_states().await?;
        let cutoff = chrono::Utc::now() - older_than;
        let mut cleaned_count = 0;

        for state in states {
            if state.is_complete()
                && let Some(finished_at) = state.finished_at
                && finished_at < cutoff
            {
                self.state_manager
                    .remove_state(&state.directory_hash)
                    .await?;
                cleaned_count += 1;
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} old execution states", cleaned_count);
        }

        Ok(cleaned_count)
    }

    /// Execute a single hook and return the result
    pub async fn execute_single_hook(&self, hook: Hook) -> Result<HookResult> {
        // Validate the command is allowed
        if !self.is_command_allowed(&hook.command).await {
            return Err(Error::configuration(format!(
                "Command '{}' is not in the allowed commands whitelist",
                hook.command
            )));
        }

        // Use the hook's timeout if specified, otherwise use the default
        let timeout = if hook.timeout_seconds > 0 {
            hook.timeout_seconds
        } else {
            self.config.default_timeout_seconds
        };

        // Validate command arguments for potential injection
        validate_command_args(&hook)?;

        execute_hook_with_timeout(hook, &timeout).await
    }
}

/// Execute hooks concurrently with proper limits and state management
async fn execute_hooks_concurrent(
    hooks: Vec<Hook>,
    _directory_path: &Path,
    config: &HookExecutionConfig,
    state_manager: &StateManager,
    state: &mut HookExecutionState,
    concurrency_limiter: Arc<Semaphore>,
    allowed_commands: Arc<RwLock<HashSet<String>>>,
) -> Result<()> {
    use futures::future::join_all;

    // Batch saves to reduce I/O operations
    let mut pending_results = Vec::new();
    let mut tasks = Vec::new();

    // Clone hooks for error handling
    let hooks_backup = hooks.clone();

    for (index, hook) in hooks.into_iter().enumerate() {
        // Check if execution was cancelled
        if let Ok(Some(current_state)) = state_manager.load_state(&state.directory_hash).await
            && current_state.status == ExecutionStatus::Cancelled
        {
            info!("Execution was cancelled, stopping");
            break;
        }

        // Validate command is allowed
        let allowed = allowed_commands.read().await;
        if !allowed.contains(&hook.command) {
            let error_msg = format!("Command '{}' is not allowed", hook.command);
            state.record_hook_result(
                index,
                HookResult::failure(
                    hook.clone(),
                    None,
                    String::new(),
                    error_msg.clone(),
                    0,
                    error_msg,
                ),
            );
            if config.fail_fast {
                warn!(
                    "Hook {} uses disallowed command, stopping execution",
                    index + 1
                );
                break;
            }
            continue;
        }
        drop(allowed);

        // Validate command arguments
        if let Err(e) = validate_command_args(&hook) {
            let error_msg = format!("Invalid command arguments: {}", e);
            state.record_hook_result(
                index,
                HookResult::failure(
                    hook.clone(),
                    None,
                    String::new(),
                    error_msg.clone(),
                    0,
                    error_msg,
                ),
            );
            if config.fail_fast {
                warn!(
                    "Hook {} has invalid arguments, stopping execution",
                    index + 1
                );
                break;
            }
            continue;
        }

        let timeout_seconds = if hook.timeout_seconds > 0 {
            hook.timeout_seconds
        } else {
            config.default_timeout_seconds
        };

        let permit = concurrency_limiter.clone().acquire_owned().await.unwrap();
        let hook_clone = hook.clone();
        let task_index = index;

        // Mark hook as running before spawning
        state.mark_hook_running(index);

        let task = tokio::spawn(async move {
            let result = execute_hook_with_timeout(hook_clone, &timeout_seconds).await;
            drop(permit); // Release the semaphore permit
            (task_index, result)
        });

        tasks.push(task);

        // For fail_fast mode, we need to execute sequentially
        if config.fail_fast {
            match tasks.pop().unwrap().await {
                Ok((idx, Ok(result))) => {
                    state.record_hook_result(idx, result.clone());
                    if !result.success && !result.hook.continue_on_error {
                        warn!("Hook {} failed and fail_fast is enabled, stopping", idx + 1);
                        break;
                    }
                }
                Ok((idx, Err(e))) => {
                    let error_msg = format!("Hook execution error: {}", e);
                    state.record_hook_result(
                        idx,
                        HookResult::failure(
                            hooks_backup.get(idx).cloned().unwrap_or_else(|| Hook {
                                command: "unknown".to_string(),
                                args: vec![],
                                working_dir: None,
                                env: HashMap::new(),
                                timeout_seconds: 0,
                                continue_on_error: false,
                            }),
                            None,
                            String::new(),
                            error_msg.clone(),
                            0,
                            error_msg,
                        ),
                    );
                    break;
                }
                Err(e) => {
                    error!("Task join error: {}", e);
                    break;
                }
            }
        }
    }

    // If not fail_fast, wait for all tasks to complete
    if !config.fail_fast && !tasks.is_empty() {
        let results = join_all(tasks).await;
        for result in results {
            match result {
                Ok((idx, Ok(hook_result))) => {
                    pending_results.push((idx, hook_result));
                }
                Ok((idx, Err(e))) => {
                    error!("Hook {} failed: {}", idx, e);
                }
                Err(e) => {
                    error!("Task join error: {}", e);
                }
            }
        }

        // Batch update results
        for (idx, result) in pending_results {
            state.record_hook_result(idx, result);
        }
    }

    // Save final state once
    state_manager.save_state(state).await?;

    Ok(())
}

/// Validate command arguments for potential security issues
fn validate_command_args(hook: &Hook) -> Result<()> {
    // Check for shell metacharacters that could lead to injection
    let dangerous_chars = ['|', '&', ';', '$', '`', '\n', '\r', '<', '>', '(', ')'];

    for arg in &hook.args {
        for ch in dangerous_chars {
            if arg.contains(ch) {
                return Err(Error::configuration(format!(
                    "Command argument contains potentially dangerous character '{}': {}",
                    ch, arg
                )));
            }
        }

        // Check for command substitution patterns
        if arg.contains("$(") || arg.contains("${") {
            return Err(Error::configuration(format!(
                "Command argument contains potential command substitution: {}",
                arg
            )));
        }
    }

    // Validate environment variables
    for (key, value) in &hook.env {
        // Check environment variable names
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(Error::configuration(format!(
                "Invalid environment variable name: {}",
                key
            )));
        }

        // Check for null bytes in values
        if value.contains('\0') {
            return Err(Error::configuration(
                "Environment variable value contains null byte".to_string(),
            ));
        }
    }

    Ok(())
}

/// Execute a single hook with timeout
async fn execute_hook_with_timeout(hook: Hook, timeout_seconds: &u64) -> Result<HookResult> {
    let start_time = Instant::now();

    debug!("Executing hook: {} {}", hook.command, hook.args.join(" "));

    // Prepare the command
    let mut cmd = Command::new(&hook.command);
    cmd.args(&hook.args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Set working directory
    if let Some(working_dir) = &hook.working_dir {
        cmd.current_dir(working_dir);
    }

    // Set environment variables
    for (key, value) in &hook.env {
        cmd.env(key, value);
    }

    // Execute with timeout
    let execution_result = timeout(Duration::from_secs(*timeout_seconds), cmd.output()).await;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    match execution_result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                debug!("Hook completed successfully in {}ms", duration_ms);
                Ok(HookResult::success(
                    hook,
                    output.status,
                    stdout,
                    stderr,
                    duration_ms,
                ))
            } else {
                warn!("Hook failed with exit code: {:?}", output.status.code());
                Ok(HookResult::failure(
                    hook,
                    Some(output.status),
                    stdout,
                    stderr,
                    duration_ms,
                    format!("Command exited with status: {}", output.status),
                ))
            }
        }
        Ok(Err(io_error)) => {
            error!("Failed to execute hook: {}", io_error);
            Ok(HookResult::failure(
                hook,
                None,
                String::new(),
                String::new(),
                duration_ms,
                format!("Failed to execute command: {}", io_error),
            ))
        }
        Err(_timeout_error) => {
            warn!("Hook timed out after {} seconds", timeout_seconds);
            Ok(HookResult::timeout(
                hook,
                String::new(),
                String::new(),
                *timeout_seconds,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::Hook;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_hook_executor_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 2,
            default_timeout_seconds: 60,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        assert_eq!(executor.config.max_concurrent, 2);
        assert_eq!(executor.config.default_timeout_seconds, 60);
    }

    #[tokio::test]
    async fn test_execute_single_hook_success() {
        let executor = HookExecutor::with_default_config().unwrap();

        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_single_hook_failure() {
        let executor = HookExecutor::with_default_config().unwrap();

        let hook = Hook {
            command: "false".to_string(), // Command that always fails
            args: vec![],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(!result.success);
        assert!(result.exit_status.is_some());
        assert_ne!(result.exit_status.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_execute_single_hook_timeout() {
        let executor = HookExecutor::with_default_config().unwrap();

        let hook = Hook {
            command: "sleep".to_string(),
            args: vec!["10".to_string()], // Sleep for 10 seconds
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 1, // But timeout after 1 second
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_background_execution() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 1,
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/directory");
        let config_hash = "test_hash".to_string();

        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["hook1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["hook2".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
        ];

        let result = executor
            .execute_hooks_background(directory_path.clone(), config_hash, hooks)
            .await
            .unwrap();

        assert!(result.contains("Started execution of 2 hooks"));

        // Wait a bit for background execution to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check execution status
        let status = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap();
        assert!(status.is_some());

        let state = status.unwrap();
        assert_eq!(state.total_hooks, 2);
        assert_eq!(state.directory_path, directory_path);
    }

    #[tokio::test]
    async fn test_concurrent_execution() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 3,
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/concurrent");
        let config_hash = "concurrent_test".to_string();

        // Create hooks that can run concurrently
        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["hook1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["hook2".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["hook3".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
        ];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash, hooks)
            .await
            .unwrap();

        // Wait for completion
        let state = executor
            .wait_for_completion(&directory_path, Some(5))
            .await
            .unwrap();

        // All hooks should complete successfully
        assert_eq!(state.completed_hooks, 3);
        assert!(state.is_complete());
        assert_eq!(state.status, ExecutionStatus::Completed);
    }

    #[tokio::test]
    async fn test_command_validation() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test disallowed command
        let hook = Hook {
            command: "rm".to_string(), // Not in whitelist
            args: vec!["-rf".to_string(), "/".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await;
        assert!(result.is_err(), "Expected error for disallowed command");

        // Test command with dangerous arguments
        let hook_with_injection = Hook {
            command: "echo".to_string(),
            args: vec!["test; rm -rf /".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook_with_injection).await;
        assert!(
            result.is_err(),
            "Expected error for command with dangerous arguments"
        );
    }

    #[tokio::test]
    async fn test_cancellation() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 1,
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/cancel");
        let config_hash = "cancel_test".to_string();

        // Create a long-running hook
        let hooks = vec![Hook {
            command: "sleep".to_string(),
            args: vec!["10".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 20,
            continue_on_error: false,
        }];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash, hooks)
            .await
            .unwrap();

        // Wait a bit for execution to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel the execution
        let cancelled = executor
            .cancel_execution(&directory_path, Some("User cancelled".to_string()))
            .await
            .unwrap();
        assert!(cancelled);

        // Check that state reflects cancellation
        let state = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_large_output_handling() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Generate a large output using printf repeating a pattern
        // Create a large string in the environment variable instead
        let large_content = "x".repeat(1000); // 1KB per line
        let mut args = Vec::new();
        // Generate 100 lines of 1KB each = 100KB total
        for i in 0..100 {
            args.push(format!("Line {}: {}", i, large_content));
        }

        // Use echo with multiple arguments
        let hook = Hook {
            command: "echo".to_string(),
            args,
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        // Output should be captured without causing memory issues
        assert!(result.stdout.len() > 50_000); // At least 50KB of output
    }

    #[tokio::test]
    async fn test_state_cleanup() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 1,
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/cleanup");
        let config_hash = "cleanup_test".to_string();

        // Execute some hooks
        let hooks = vec![Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        }];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash, hooks)
            .await
            .unwrap();

        // Wait for completion
        executor
            .wait_for_completion(&directory_path, Some(5))
            .await
            .unwrap();

        // Clean up old states (should clean up the completed state)
        let cleaned = executor
            .cleanup_old_states(chrono::Duration::seconds(0))
            .await
            .unwrap();
        assert_eq!(cleaned, 1);

        // State should be gone
        let state = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap();
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_execution_state_tracking() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 1,
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/directory");

        // Initially no state
        let status = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap();
        assert!(status.is_none());

        // Start execution
        let hooks = vec![Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 10,
            continue_on_error: false,
        }];

        executor
            .execute_hooks_background(directory_path.clone(), "hash".to_string(), hooks)
            .await
            .unwrap();

        // Should now have state
        let status = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap();
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn test_command_whitelist_management() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test adding a new command to whitelist
        let custom_command = "my-custom-tool".to_string();
        executor.allow_command(custom_command.clone()).await;

        // Test that the newly allowed command works
        let hook = Hook {
            command: custom_command.clone(),
            args: vec!["--version".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: true, // Don't fail if command doesn't exist
        };

        // Should not error due to whitelist check (may fail if command doesn't exist)
        let result = executor.execute_single_hook(hook).await;
        // If it errors, it should be because the command doesn't exist, not because it's not allowed
        if result.is_err() {
            let err_msg = result.unwrap_err().to_string();
            assert!(
                !err_msg.contains("not allowed"),
                "Command should be allowed after adding to whitelist"
            );
        }

        // Test removing a command from whitelist
        executor.disallow_command("echo").await;

        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await;
        assert!(result.is_err(), "Echo command should be disallowed");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not allowed")
                || err_msg.contains("not in whitelist")
                || err_msg.contains("Configuration"),
            "Error message should indicate command not allowed: {}",
            err_msg
        );

        // Test re-allowing a command
        executor.allow_command("echo".to_string()).await;

        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await;
        assert!(
            result.is_ok(),
            "Echo command should be allowed after re-adding"
        );
    }

    #[tokio::test]
    async fn test_fail_fast_mode_edge_cases() {
        let temp_dir = TempDir::new().unwrap();

        // Test fail_fast with multiple failing hooks
        let config = HookExecutionConfig {
            max_concurrent: 2,
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/fail-fast");

        let hooks = vec![
            Hook {
                command: "false".to_string(), // Will fail
                args: vec![],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(), // Should not execute due to fail_fast
                args: vec!["should not run".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(), // Should not execute due to fail_fast
                args: vec!["also should not run".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
        ];

        executor
            .execute_hooks_background(directory_path.clone(), "fail_fast_test".to_string(), hooks)
            .await
            .unwrap();

        // Wait for completion
        executor
            .wait_for_completion(&directory_path, Some(10))
            .await
            .unwrap();

        let state = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state.status, ExecutionStatus::Failed);
        // Only the first hook should have been executed
        // Only the first hook should have been executed
        assert_eq!(state.completed_hooks, 1);

        // Test fail_fast with continue_on_error interaction
        let directory_path2 = PathBuf::from("/test/fail-fast-continue");

        let hooks2 = vec![
            Hook {
                command: "false".to_string(),
                args: vec![],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: true, // Should continue despite failure
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["this should run".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
            Hook {
                command: "false".to_string(), // Will fail and stop execution
                args: vec![],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["this should not run".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
        ];

        executor
            .execute_hooks_background(
                directory_path2.clone(),
                "fail_fast_continue_test".to_string(),
                hooks2,
            )
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path2, Some(10))
            .await
            .unwrap();

        let state2 = executor
            .get_execution_status(&directory_path2)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state2.status, ExecutionStatus::Failed);
        // First three hooks should have been executed
        // First three hooks should have been executed
        assert_eq!(state2.completed_hooks, 3);
    }

    #[tokio::test]
    async fn test_security_validation_comprehensive() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test various command injection attempts
        let injection_attempts = vec![
            vec!["; rm -rf /".to_string()],
            vec!["&& rm -rf /".to_string()],
            vec!["| rm -rf /".to_string()],
            vec!["`rm -rf /`".to_string()],
            vec!["$(rm -rf /)".to_string()],
            vec!["'; DROP TABLE users; --".to_string()],
            vec!["\"; rm -rf / #".to_string()],
            vec!["../../../etc/passwd; cat /etc/passwd".to_string()],
            vec!["test\nrm -rf /".to_string()],
            vec!["test\r\nrm -rf /".to_string()],
        ];

        for args in injection_attempts {
            let hook = Hook {
                command: "echo".to_string(),
                args: args.clone(),
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            };

            let result = executor.execute_single_hook(hook).await;
            assert!(
                result.is_err(),
                "Should reject potentially dangerous arguments: {:?}",
                args
            );
        }

        // Test environment variable security
        // Note: The current implementation might not check for LD_PRELOAD specifically
        // This is a security recommendation for future implementation
        let mut dangerous_env = HashMap::new();
        dangerous_env.insert("LD_PRELOAD".to_string(), "/evil/library.so".to_string());

        let hook_with_ld_preload = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: dangerous_env,
            timeout_seconds: 5,
            continue_on_error: true, // Continue on error to handle different behaviors
        };

        // This test documents that LD_PRELOAD should ideally be checked
        let _ = executor.execute_single_hook(hook_with_ld_preload).await;

        // Test PATH manipulation
        // Note: PATH manipulation might be allowed in some cases
        let mut path_manipulation = HashMap::new();
        path_manipulation.insert("PATH".to_string(), "/evil/bin:$PATH".to_string());

        let hook_with_path = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: path_manipulation,
            timeout_seconds: 5,
            continue_on_error: true,
        };

        // This test documents that PATH manipulation could be a security concern
        let _ = executor.execute_single_hook(hook_with_path).await;
    }

    #[tokio::test]
    async fn test_working_directory_handling() {
        let executor = HookExecutor::with_default_config().unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Test with valid working directory
        let hook_with_valid_dir = Hook {
            command: "pwd".to_string(),
            args: vec![],
            working_dir: Some(temp_dir.path().to_path_buf()),
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: false,
        };

        let result = executor
            .execute_single_hook(hook_with_valid_dir)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.stdout.contains(temp_dir.path().to_str().unwrap()));

        // Test with non-existent working directory
        let hook_with_invalid_dir = Hook {
            command: "pwd".to_string(),
            args: vec![],
            working_dir: Some(PathBuf::from("/nonexistent/directory/that/does/not/exist")),
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: true, // Continue on error to handle different OS behaviors
        };

        let result = executor.execute_single_hook(hook_with_invalid_dir).await;
        // This might succeed or fail depending on the implementation
        // The important part is it doesn't panic
        if result.is_ok() {
            // If it succeeds, the command might have handled the missing directory
            assert!(!result
                .unwrap()
                .stdout
                .contains("/nonexistent/directory/that/does/not/exist"));
        }

        // Test with relative path working directory (should be validated)
        let hook_with_relative_dir = Hook {
            command: "pwd".to_string(),
            args: vec![],
            working_dir: Some(PathBuf::from("./relative/path")),
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: false,
        };

        // This might work or fail depending on the implementation
        let _ = executor.execute_single_hook(hook_with_relative_dir).await;
    }

    #[tokio::test]
    async fn test_concurrent_execution_limits() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 2, // Limit to 2 concurrent hooks
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/concurrent");

        // Create 5 hooks that sleep for different durations
        let hooks = vec![
            Hook {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
            Hook {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 10,
                continue_on_error: false,
            },
        ];

        let start = std::time::Instant::now();

        executor
            .execute_hooks_background(directory_path.clone(), "concurrent_test".to_string(), hooks)
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path, Some(30))
            .await
            .unwrap();

        let duration = start.elapsed();

        // With max_concurrent=2, 5 hooks of 0.1s each should take at least 0.3s
        // (3 batches: 2, 2, 1)
        assert!(
            duration.as_secs_f64() >= 0.25,
            "Concurrent execution limit not enforced properly"
        );

        let state = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state.status, ExecutionStatus::Completed);
        assert_eq!(state.completed_hooks, 5);
        assert_eq!(state.total_hooks, 5);
    }

    #[tokio::test]
    async fn test_hook_execution_with_complex_output() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test simple hooks without dangerous characters
        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["stdout output".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("stdout output"));

        // Test hook with non-zero exit code (using false command)
        let hook_with_exit_code = Hook {
            command: "false".to_string(),
            args: vec![],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 5,
            continue_on_error: true,
        };

        let result = executor
            .execute_single_hook(hook_with_exit_code)
            .await
            .unwrap();
        assert!(!result.success);
        // Exit code should be non-zero
        assert!(result.exit_status.is_some());
    }

    #[tokio::test]
    async fn test_multiple_directory_executions() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 2,
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();

        // Start executions for multiple directories
        let directories = vec![
            PathBuf::from("/test/dir1"),
            PathBuf::from("/test/dir2"),
            PathBuf::from("/test/dir3"),
        ];

        for (i, dir) in directories.iter().enumerate() {
            let hooks = vec![Hook {
                command: "echo".to_string(),
                args: vec![format!("directory {}", i)],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            }];

            executor
                .execute_hooks_background(dir.clone(), format!("hash_{}", i), hooks)
                .await
                .unwrap();
        }

        // Wait for all to complete
        for dir in &directories {
            executor.wait_for_completion(dir, Some(10)).await.unwrap();

            let state = executor.get_execution_status(dir).await.unwrap().unwrap();

            assert_eq!(state.status, ExecutionStatus::Completed);
            assert_eq!(state.completed_hooks, 1);
            assert_eq!(state.total_hooks, 1);
        }
    }

    #[tokio::test]
    async fn test_error_recovery_and_retry() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            max_concurrent: 1,
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/recovery");

        // Execute hooks with some failures
        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["success 1".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
            Hook {
                command: "false".to_string(),
                args: vec![],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: true, // Continue despite failure
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["success 2".to_string()],
                working_dir: None,
                env: HashMap::new(),
                timeout_seconds: 5,
                continue_on_error: false,
            },
        ];

        executor
            .execute_hooks_background(directory_path.clone(), "recovery_test".to_string(), hooks)
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path, Some(10))
            .await
            .unwrap();

        let state = executor
            .get_execution_status(&directory_path)
            .await
            .unwrap()
            .unwrap();

        // Should complete with partial failure
        assert_eq!(state.status, ExecutionStatus::Completed);
        assert_eq!(state.completed_hooks, 3);
        assert_eq!(state.total_hooks, 3);
    }
}
