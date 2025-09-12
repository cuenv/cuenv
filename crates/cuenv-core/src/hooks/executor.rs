//! Hook execution engine with background processing and state management

use crate::hooks::state::{HookExecutionState, StateManager, compute_directory_hash};
use crate::hooks::types::{ExecutionStatus, Hook, HookExecutionConfig, HookResult};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Manages hook execution with background processing and state persistence
#[derive(Debug)]
pub struct HookExecutor {
    config: HookExecutionConfig,
    state_manager: StateManager,
}

impl HookExecutor {
    /// Create a new hook executor with the specified configuration
    pub fn new(config: HookExecutionConfig) -> Result<Self> {
        let state_dir = if let Some(dir) = config.state_dir.clone() {
            dir
        } else {
            StateManager::default_state_dir()?
        };

        let state_manager = StateManager::new(state_dir);

        Ok(Self {
            config,
            state_manager,
        })
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

        // Spawn background execution task
        tokio::spawn(async move {
            let result = execute_hooks_sequential(
                hooks,
                &directory_path,
                &config,
                &state_manager,
                &mut state,
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
        });

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
        // Use the hook's timeout if specified, otherwise use the default
        let timeout = if hook.timeout_seconds > 0 {
            hook.timeout_seconds
        } else {
            self.config.default_timeout_seconds
        };
        execute_hook_with_timeout(hook, &timeout).await
    }
}

/// Execute hooks sequentially, updating state as we go
async fn execute_hooks_sequential(
    hooks: Vec<Hook>,
    _directory_path: &Path,
    config: &HookExecutionConfig,
    state_manager: &StateManager,
    state: &mut HookExecutionState,
) -> Result<()> {
    for (index, hook) in hooks.into_iter().enumerate() {
        // Check if execution was cancelled
        if let Ok(Some(current_state)) = state_manager.load_state(&state.directory_hash).await
            && current_state.status == ExecutionStatus::Cancelled
        {
            info!("Execution was cancelled, stopping");
            return Ok(());
        }

        // Mark hook as currently executing
        state.mark_hook_running(index);
        state_manager.save_state(state).await?;

        // Execute the hook
        let timeout_seconds = if hook.timeout_seconds > 0 {
            hook.timeout_seconds
        } else {
            config.default_timeout_seconds
        };

        let result = execute_hook_with_timeout(hook, &timeout_seconds).await?;

        // Record the result
        state.record_hook_result(index, result.clone());
        state_manager.save_state(state).await?;

        // Check if we should stop on failure
        if !result.success && config.fail_fast && !result.hook.continue_on_error {
            warn!(
                "Hook {} failed and fail_fast is enabled, stopping execution",
                index + 1
            );
            break;
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
}
