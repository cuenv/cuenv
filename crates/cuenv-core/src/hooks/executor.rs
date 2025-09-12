//! Hook execution engine for cuenv
//!
//! This module handles the sequential execution of hooks with fail-fast behavior,
//! background processing, and integration with the CUE evaluator.

use super::{ExecHook, HookExecutionState, HookResult, HookStatus};
use super::state::StateManager;
use crate::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info};

/// Hook execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    /// Maximum time to wait for a hook to complete (in seconds)
    pub hook_timeout_seconds: u64,
    /// Maximum number of concurrent hook executions
    pub max_concurrent_executions: usize,
    /// Whether to capture stdout/stderr from hooks
    pub capture_output: bool,
    /// Working directory for hook execution
    pub working_directory: Option<PathBuf>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            hook_timeout_seconds: 300, // 5 minutes
            max_concurrent_executions: 1, // Sequential execution
            capture_output: true,
            working_directory: None,
        }
    }
}

/// Hook execution engine
#[derive(Debug)]
pub struct HookExecutor {
    /// Configuration for the executor
    config: ExecutorConfig,
    /// State manager for persistence
    state_manager: StateManager,
}

impl HookExecutor {
    /// Create a new hook executor
    /// 
    /// # Errors
    /// Returns an error if the state manager cannot be initialized
    pub fn new(config: ExecutorConfig) -> Result<Self> {
        let state_manager = StateManager::new()?;
        Ok(Self {
            config,
            state_manager,
        })
    }

    /// Create a new hook executor with default configuration
    /// 
    /// # Errors
    /// Returns an error if the state manager cannot be initialized
    pub fn with_default_config() -> Result<Self> {
        Self::new(ExecutorConfig::default())
    }

    /// Execute hooks for a directory in the background
    /// 
    /// This starts hook execution and returns immediately. The execution state
    /// can be monitored using the state manager.
    /// 
    /// # Errors
    /// Returns an error if the hooks cannot be started
    pub async fn execute_hooks_background(
        &self,
        directory: PathBuf,
        hooks: Vec<ExecHook>,
        config_hash: String,
    ) -> Result<()> {
        if hooks.is_empty() {
            debug!("No hooks to execute for directory: {}", directory.display());
            return Ok(());
        }

        info!(
            "Starting background hook execution for {} hooks in directory: {}",
            hooks.len(),
            directory.display()
        );

        // Create initial execution state
        let mut state = HookExecutionState::new(directory.clone(), config_hash.clone(), hooks.len());
        state.mark_started();

        // Save initial state
        self.state_manager.save_state(&state).await?;

        // Spawn background task for hook execution
        let executor = self.clone_for_background();
        tokio::spawn(async move {
            info!("Background task started for hook execution");
            if let Err(e) = executor.execute_hooks_sequential(state, hooks).await {
                error!("Hook execution failed: {}", e);
            } else {
                info!("Hook execution completed successfully");
            }
        });

        Ok(())
    }

    /// Clone executor for background execution
    fn clone_for_background(&self) -> HookExecutor {
        HookExecutor {
            config: self.config.clone(),
            state_manager: StateManager::new().unwrap_or_else(|e| {
                error!("Failed to create state manager for background task: {}", e);
                panic!("Critical error: cannot create state manager");
            }),
        }
    }

    /// Execute hooks sequentially with fail-fast behavior
    async fn execute_hooks_sequential(
        &self,
        mut state: HookExecutionState,
        hooks: Vec<ExecHook>,
    ) -> Result<()> {
        for (index, hook) in hooks.iter().enumerate() {
            // Check if execution should be aborted
            if state.is_finished() {
                break;
            }

            state.mark_hook_started(index);
            self.state_manager.save_state(&state).await?;

            debug!("Executing hook {}: {}", index, hook.command);

            let start_time = Instant::now();
            let result = self.execute_single_hook(hook, &state.directory).await;
            let duration = start_time.elapsed();

            let hook_result = HookResult {
                index,
                exit_code: result.as_ref().map_or(1, |r| *r),
                stdout: String::new(), // TODO: Capture actual output
                stderr: String::new(), // TODO: Capture actual output  
                duration_ms: duration.as_millis() as u64,
                completed_at: Utc::now(),
            };

            state.add_result(hook_result.clone());

            // Save updated state
            self.state_manager.save_state(&state).await?;

            // Fail-fast: stop on first failure
            if !hook_result.is_success() {
                error!(
                    "Hook {} failed with exit code {}, stopping execution",
                    index, hook_result.exit_code
                );
                break;
            }

            info!("Hook {} completed successfully", index);
        }

        info!(
            "Hook execution finished for directory: {} with status: {}",
            state.directory.display(),
            state.status
        );

        // Clean up finished state after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            // TODO: Implement state cleanup
        });

        Ok(())
    }

    /// Execute a single hook
    async fn execute_single_hook(
        &self,
        hook: &ExecHook,
        base_directory: &Path,
    ) -> Result<i32> {
        let working_dir = if let Some(ref dir) = hook.dir {
            base_directory.join(dir)
        } else {
            base_directory.to_path_buf()
        };

        debug!(
            "Executing command '{}' in directory '{}'",
            hook.command,
            working_dir.display()
        );

        // Build command
        let mut cmd = Command::new(&hook.command);
        
        if let Some(ref args) = hook.args {
            cmd.args(args);
        }

        cmd.current_dir(&working_dir);

        // Configure stdio based on configuration
        if self.config.capture_output {
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }

        // Execute with timeout
        let timeout_duration = Duration::from_secs(self.config.hook_timeout_seconds);
        
        // Spawn the process first
        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn hook process: {}", e);
            Error::configuration(format!(
                "Failed to start hook command '{}': {}",
                hook.command, e
            ))
        })?;

        debug!("Hook process spawned successfully, waiting for completion");

        // Then apply timeout to waiting for completion
        match tokio::time::timeout(timeout_duration, child.wait()).await {
            Ok(Ok(status)) => {
                let exit_code = status.code().unwrap_or(-1);
                debug!("Hook command completed with exit code: {}", exit_code);
                Ok(exit_code)
            }
            Ok(Err(e)) => {
                error!("Failed to wait for hook process: {}", e);
                Err(Error::configuration(format!(
                    "Hook execution failed: {}"
                    , e
                )))
            }
            Err(_) => {
                error!("Hook execution timed out after {} seconds", self.config.hook_timeout_seconds);
                // Try to kill the process
                let _ = child.kill().await;
                Err(Error::Timeout {
                    seconds: self.config.hook_timeout_seconds,
                })
            }
        }
    }

    /// Get current execution status for a directory
    /// 
    /// # Errors
    /// Returns an error if the state cannot be loaded
    pub async fn get_execution_status(&self, directory: &Path) -> Result<Option<HookExecutionState>> {
        self.state_manager.load_state(directory).await
    }

    /// Wait for hook execution to complete
    /// 
    /// # Errors
    /// Returns an error if the state cannot be monitored
    pub async fn wait_for_completion(&self, directory: &Path, timeout_seconds: Option<u64>) -> Result<HookExecutionState> {
        let timeout_duration = timeout_seconds
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300)); // Default 5 minute timeout

        let start_time = Instant::now();

        loop {
            if start_time.elapsed() > timeout_duration {
                return Err(Error::Timeout {
                    seconds: timeout_duration.as_secs(),
                });
            }

            if let Some(state) = self.state_manager.load_state(directory).await? {
                if state.is_finished() {
                    return Ok(state);
                }
            }

            // Wait before checking again
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Cancel hook execution for a directory
    /// 
    /// # Errors
    /// Returns an error if the state cannot be updated or removed
    pub async fn cancel_execution(&self, directory: &Path) -> Result<()> {
        if let Some(mut state) = self.state_manager.load_state(directory).await? {
            if !state.is_finished() {
                state.status = HookStatus::Failed("Cancelled by user".to_string());
                state.finished_at = Some(Utc::now());
                self.state_manager.save_state(&state).await?;
            }
        }

        // Remove state to clean up
        self.state_manager.remove_state(directory).await?;
        
        Ok(())
    }

    /// List all active hook executions
    /// 
    /// # Errors
    /// Returns an error if active states cannot be retrieved
    pub async fn list_active_executions(&self) -> Result<Vec<HookExecutionState>> {
        self.state_manager.list_active_states().await
    }
}

/// Event emitted during hook execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookExecutionEvent {
    /// Hook execution started
    Started {
        directory: PathBuf,
        session_id: uuid::Uuid,
        total_hooks: usize,
    },
    /// Individual hook started
    HookStarted {
        directory: PathBuf,
        session_id: uuid::Uuid,
        hook_index: usize,
        command: String,
    },
    /// Individual hook completed
    HookCompleted {
        directory: PathBuf,
        session_id: uuid::Uuid,
        hook_index: usize,
        result: HookResult,
    },
    /// All hooks completed or execution failed
    Finished {
        directory: PathBuf,
        session_id: uuid::Uuid,
        status: HookStatus,
    },
}

/// Event sender type for hook execution events
pub type EventSender = mpsc::UnboundedSender<HookExecutionEvent>;

/// Event receiver type for hook execution events
pub type EventReceiver = mpsc::UnboundedReceiver<HookExecutionEvent>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::env;

    #[tokio::test]
    async fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert_eq!(config.hook_timeout_seconds, 300);
        assert_eq!(config.max_concurrent_executions, 1);
        assert!(config.capture_output);
        assert_eq!(config.working_directory, None);
    }

    #[tokio::test]
    async fn test_executor_creation() {
        // Set up temporary directory for state
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        assert!(executor.config.capture_output);
    }

    #[tokio::test]
    async fn test_execute_hooks_background_empty() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let directory = temp_dir.path().to_path_buf();
        let hooks = vec![];
        let config_hash = "test-hash".to_string();

        // Should succeed with empty hooks
        executor
            .execute_hooks_background(directory, hooks, config_hash)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_get_execution_status_none() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let directory = temp_dir.path();

        let status = executor.get_execution_status(directory).await.unwrap();
        assert!(status.is_none());
    }

    #[tokio::test] 
    async fn test_execute_single_hook_success() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let hook = ExecHook {
            command: "echo".to_string(),
            args: Some(vec!["hello".to_string()]),
            dir: None,
            inputs: None,
            source: None,
            preload: None,
        };

        let result = executor
            .execute_single_hook(&hook, temp_dir.path())
            .await
            .unwrap();
        
        assert_eq!(result, 0); // echo should succeed
    }

    #[tokio::test]
    async fn test_execute_single_hook_failure() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let hook = ExecHook {
            command: "false".to_string(), // Command that always fails
            args: None,
            dir: None,
            inputs: None,
            source: None,
            preload: None,
        };

        let result = executor
            .execute_single_hook(&hook, temp_dir.path())
            .await
            .unwrap();
        
        assert_ne!(result, 0); // false should fail
    }

    #[tokio::test]
    async fn test_cancel_execution() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let directory = temp_dir.path();

        // Should succeed even if no execution is running
        executor.cancel_execution(directory).await.unwrap();
    }

    #[tokio::test]
    async fn test_list_active_executions() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();

        let executions = executor.list_active_executions().await.unwrap();
        assert!(executions.is_empty());
    }

    #[tokio::test]
    async fn test_executor_config_serialization() {
        let config = ExecutorConfig {
            hook_timeout_seconds: 120,
            max_concurrent_executions: 2,
            capture_output: false,
            working_directory: Some(PathBuf::from("/test")),
        };

        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: ExecutorConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(config.hook_timeout_seconds, deserialized.hook_timeout_seconds);
        assert_eq!(config.max_concurrent_executions, deserialized.max_concurrent_executions);
        assert_eq!(config.capture_output, deserialized.capture_output);
        assert_eq!(config.working_directory, deserialized.working_directory);
    }

    #[test]
    fn test_hook_execution_event_serialization() {
        let event = HookExecutionEvent::Started {
            directory: PathBuf::from("/test"),
            session_id: uuid::Uuid::new_v4(),
            total_hooks: 3,
        };

        let serialized = serde_json::to_string(&event).unwrap();
        let _deserialized: HookExecutionEvent = serde_json::from_str(&serialized).unwrap();
        // If we get here without panic, serialization works
    }

    #[tokio::test]
    async fn test_wait_for_completion_no_state() {
        let temp_dir = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let executor = HookExecutor::with_default_config().unwrap();
        let directory = temp_dir.path();

        // Should timeout since there's no state
        let result = executor.wait_for_completion(directory, Some(1)).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Timeout { .. }));
    }
}