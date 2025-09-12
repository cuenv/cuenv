//! State management for hook execution tracking

use crate::hooks::types::{ExecutionStatus, HookResult};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, error, info, warn};

/// Manages persistent state for hook execution sessions
#[derive(Debug, Clone)]
pub struct StateManager {
    state_dir: PathBuf,
}

impl StateManager {
    /// Create a new state manager with the specified state directory
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the default state directory (~/.cuenv/state)
    pub fn default_state_dir() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        Ok(home.join(".cuenv").join("state"))
    }

    /// Create a state manager using the default state directory
    pub fn with_default_dir() -> Result<Self> {
        Ok(Self::new(Self::default_state_dir()?))
    }

    /// Ensure the state directory exists
    pub async fn ensure_state_dir(&self) -> Result<()> {
        if !self.state_dir.exists() {
            fs::create_dir_all(&self.state_dir)
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: Some(self.state_dir.clone().into_boxed_path()),
                    operation: "create_dir_all".to_string(),
                })?;
            debug!("Created state directory: {}", self.state_dir.display());
        }
        Ok(())
    }

    /// Generate a state file path for a given directory hash
    fn state_file_path(&self, directory_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", directory_hash))
    }

    /// Save execution state to disk
    pub async fn save_state(&self, state: &HookExecutionState) -> Result<()> {
        self.ensure_state_dir().await?;

        let state_file = self.state_file_path(&state.directory_hash);
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| Error::configuration(format!("Failed to serialize state: {e}")))?;

        fs::write(&state_file, json).await.map_err(|e| Error::Io {
            source: e,
            path: Some(state_file.into_boxed_path()),
            operation: "write".to_string(),
        })?;

        debug!(
            "Saved execution state for directory hash: {}",
            state.directory_hash
        );
        Ok(())
    }

    /// Load execution state from disk
    pub async fn load_state(&self, directory_hash: &str) -> Result<Option<HookExecutionState>> {
        let state_file = self.state_file_path(directory_hash);

        if !state_file.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&state_file)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(state_file.clone().into_boxed_path()),
                operation: "read_to_string".to_string(),
            })?;

        let state: HookExecutionState = serde_json::from_str(&json)
            .map_err(|e| Error::configuration(format!("Failed to deserialize state: {e}")))?;

        debug!(
            "Loaded execution state for directory hash: {}",
            directory_hash
        );
        Ok(Some(state))
    }

    /// Remove state file for a directory
    pub async fn remove_state(&self, directory_hash: &str) -> Result<()> {
        let state_file = self.state_file_path(directory_hash);

        if state_file.exists() {
            fs::remove_file(&state_file).await.map_err(|e| Error::Io {
                source: e,
                path: Some(state_file.into_boxed_path()),
                operation: "remove_file".to_string(),
            })?;
            debug!(
                "Removed execution state for directory hash: {}",
                directory_hash
            );
        }

        Ok(())
    }

    /// List all active execution states
    pub async fn list_active_states(&self) -> Result<Vec<HookExecutionState>> {
        if !self.state_dir.exists() {
            return Ok(Vec::new());
        }

        let mut states = Vec::new();
        let mut dir = fs::read_dir(&self.state_dir).await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "read_dir".to_string(),
        })?;

        while let Some(entry) = dir.next_entry().await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "next_entry".to_string(),
        })? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Ok(Some(state)) = self.load_state(stem).await
            {
                states.push(state);
            }
        }

        Ok(states)
    }

    /// Clean up the entire state directory
    pub async fn cleanup_state_directory(&self) -> Result<usize> {
        if !self.state_dir.exists() {
            return Ok(0);
        }

        let mut cleaned_count = 0;
        let mut dir = fs::read_dir(&self.state_dir).await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "read_dir".to_string(),
        })?;

        while let Some(entry) = dir.next_entry().await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "next_entry".to_string(),
        })? {
            let path = entry.path();

            // Only clean up JSON state files
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                // Try to load and check if it's a completed state
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    match self.load_state(stem).await {
                        Ok(Some(state)) if state.is_complete() => {
                            // Remove completed states
                            if let Err(e) = fs::remove_file(&path).await {
                                warn!("Failed to remove state file {}: {}", path.display(), e);
                            } else {
                                cleaned_count += 1;
                                debug!("Cleaned up state file: {}", path.display());
                            }
                        }
                        Ok(Some(_)) => {
                            // Keep running states
                            debug!("Keeping active state file: {}", path.display());
                        }
                        Ok(None) => {}
                        Err(e) => {
                            // If we can't parse it, it might be corrupted - remove it
                            warn!("Failed to parse state file {}: {}", path.display(), e);
                            if let Err(rm_err) = fs::remove_file(&path).await {
                                error!(
                                    "Failed to remove corrupted state file {}: {}",
                                    path.display(),
                                    rm_err
                                );
                            } else {
                                cleaned_count += 1;
                                info!("Removed corrupted state file: {}", path.display());
                            }
                        }
                    }
                }
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} state files from directory", cleaned_count);
        }

        Ok(cleaned_count)
    }

    /// Clean up orphaned state files (states without corresponding processes)
    pub async fn cleanup_orphaned_states(&self, max_age: chrono::Duration) -> Result<usize> {
        let cutoff = Utc::now() - max_age;
        let mut cleaned_count = 0;

        for state in self.list_active_states().await? {
            // Remove states that are stuck in running but are too old
            if state.status == ExecutionStatus::Running && state.started_at < cutoff {
                warn!(
                    "Found orphaned running state for {} (started {}), removing",
                    state.directory_path.display(),
                    state.started_at
                );
                self.remove_state(&state.directory_hash).await?;
                cleaned_count += 1;
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} orphaned state files", cleaned_count);
        }

        Ok(cleaned_count)
    }
}

/// Represents the state of hook execution for a specific directory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookExecutionState {
    /// Hash of the directory path
    pub directory_hash: String,
    /// Path to the directory being processed
    pub directory_path: PathBuf,
    /// Hash of the configuration that was approved
    pub config_hash: String,
    /// Current status of execution
    pub status: ExecutionStatus,
    /// Total number of hooks to execute
    pub total_hooks: usize,
    /// Number of hooks completed so far
    pub completed_hooks: usize,
    /// Index of currently executing hook (if any)
    pub current_hook_index: Option<usize>,
    /// Results of completed hooks
    pub hook_results: HashMap<usize, HookResult>,
    /// Timestamp when execution started
    pub started_at: DateTime<Utc>,
    /// Timestamp when execution finished (if completed)
    pub finished_at: Option<DateTime<Utc>>,
    /// Error message if execution failed
    pub error_message: Option<String>,
}

impl HookExecutionState {
    /// Create a new execution state
    pub fn new(
        directory_path: PathBuf,
        directory_hash: String,
        config_hash: String,
        total_hooks: usize,
    ) -> Self {
        Self {
            directory_hash,
            directory_path,
            config_hash,
            status: ExecutionStatus::Running,
            total_hooks,
            completed_hooks: 0,
            current_hook_index: None,
            hook_results: HashMap::new(),
            started_at: Utc::now(),
            finished_at: None,
            error_message: None,
        }
    }

    /// Mark a hook as currently executing
    pub fn mark_hook_running(&mut self, hook_index: usize) {
        self.current_hook_index = Some(hook_index);
        info!(
            "Started executing hook {} of {}",
            hook_index + 1,
            self.total_hooks
        );
    }

    /// Record the result of a hook execution
    pub fn record_hook_result(&mut self, hook_index: usize, result: HookResult) {
        self.hook_results.insert(hook_index, result.clone());
        self.completed_hooks += 1;
        self.current_hook_index = None;

        if result.success {
            info!(
                "Hook {} of {} completed successfully",
                hook_index + 1,
                self.total_hooks
            );
        } else {
            error!(
                "Hook {} of {} failed: {:?}",
                hook_index + 1,
                self.total_hooks,
                result.error
            );
            self.status = ExecutionStatus::Failed;
            self.error_message = result.error.clone();
            self.finished_at = Some(Utc::now());
            return;
        }

        // Check if all hooks are complete
        if self.completed_hooks == self.total_hooks {
            self.status = ExecutionStatus::Completed;
            self.finished_at = Some(Utc::now());
            info!("All {} hooks completed successfully", self.total_hooks);
        }
    }

    /// Mark execution as cancelled
    pub fn mark_cancelled(&mut self, reason: Option<String>) {
        self.status = ExecutionStatus::Cancelled;
        self.finished_at = Some(Utc::now());
        self.error_message = reason;
        self.current_hook_index = None;
    }

    /// Check if execution is complete (success, failure, or cancelled)
    pub fn is_complete(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        )
    }

    /// Get a human-readable progress display
    pub fn progress_display(&self) -> String {
        match &self.status {
            ExecutionStatus::Running => {
                if let Some(current) = self.current_hook_index {
                    format!(
                        "Executing hook {} of {} ({})",
                        current + 1,
                        self.total_hooks,
                        self.status
                    )
                } else {
                    format!(
                        "{} of {} hooks completed",
                        self.completed_hooks, self.total_hooks
                    )
                }
            }
            ExecutionStatus::Completed => "All hooks completed successfully".to_string(),
            ExecutionStatus::Failed => {
                if let Some(error) = &self.error_message {
                    format!("Hook execution failed: {}", error)
                } else {
                    "Hook execution failed".to_string()
                }
            }
            ExecutionStatus::Cancelled => {
                if let Some(reason) = &self.error_message {
                    format!("Hook execution cancelled: {}", reason)
                } else {
                    "Hook execution cancelled".to_string()
                }
            }
        }
    }

    /// Get execution duration
    pub fn duration(&self) -> chrono::Duration {
        let end = self.finished_at.unwrap_or_else(Utc::now);
        end - self.started_at
    }
}

/// Compute a hash for a directory path for state file naming
pub fn compute_directory_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::{Hook, HookResult};
    use std::collections::HashMap;
    use std::os::unix::process::ExitStatusExt;
    use tempfile::TempDir;

    #[test]
    fn test_compute_directory_hash() {
        let path = Path::new("/test/path");
        let hash = compute_directory_hash(path);
        assert_eq!(hash.len(), 16);

        // Same path should produce same hash
        let hash2 = compute_directory_hash(path);
        assert_eq!(hash, hash2);

        // Different path should produce different hash
        let different_path = Path::new("/other/path");
        let different_hash = compute_directory_hash(different_path);
        assert_ne!(hash, different_hash);
    }

    #[tokio::test]
    async fn test_state_manager_operations() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        let directory_path = PathBuf::from("/test/dir");
        let directory_hash = compute_directory_hash(&directory_path);
        let config_hash = "test_config_hash".to_string();

        let mut state =
            HookExecutionState::new(directory_path, directory_hash.clone(), config_hash, 2);

        // Save initial state
        state_manager.save_state(&state).await.unwrap();

        // Load state back
        let loaded_state = state_manager
            .load_state(&directory_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded_state.directory_hash, state.directory_hash);
        assert_eq!(loaded_state.total_hooks, 2);
        assert_eq!(loaded_state.status, ExecutionStatus::Running);

        // Update state with hook result
        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 300,
            continue_on_error: false,
        };

        let result = HookResult::success(
            hook,
            std::process::ExitStatus::from_raw(0),
            "test\n".to_string(),
            "".to_string(),
            100,
        );

        state.record_hook_result(0, result);
        state_manager.save_state(&state).await.unwrap();

        // Load updated state
        let updated_state = state_manager
            .load_state(&directory_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated_state.completed_hooks, 1);
        assert_eq!(updated_state.hook_results.len(), 1);

        // Remove state
        state_manager.remove_state(&directory_hash).await.unwrap();
        let removed_state = state_manager.load_state(&directory_hash).await.unwrap();
        assert!(removed_state.is_none());
    }

    #[test]
    fn test_hook_execution_state() {
        let directory_path = PathBuf::from("/test/dir");
        let directory_hash = "test_hash".to_string();
        let config_hash = "config_hash".to_string();
        let mut state = HookExecutionState::new(directory_path, directory_hash, config_hash, 3);

        // Initial state
        assert_eq!(state.status, ExecutionStatus::Running);
        assert_eq!(state.total_hooks, 3);
        assert_eq!(state.completed_hooks, 0);
        assert!(!state.is_complete());

        // Mark hook as running
        state.mark_hook_running(0);
        assert_eq!(state.current_hook_index, Some(0));

        // Record successful hook result
        let hook = Hook {
            command: "echo".to_string(),
            args: vec![],
            working_dir: None,
            env: HashMap::new(),
            timeout_seconds: 300,
            continue_on_error: false,
        };

        let result = HookResult::success(
            hook.clone(),
            std::process::ExitStatus::from_raw(0),
            "".to_string(),
            "".to_string(),
            100,
        );

        state.record_hook_result(0, result);
        assert_eq!(state.completed_hooks, 1);
        assert_eq!(state.current_hook_index, None);
        assert_eq!(state.status, ExecutionStatus::Running);
        assert!(!state.is_complete());

        // Record failed hook result
        let failed_result = HookResult::failure(
            hook,
            Some(std::process::ExitStatus::from_raw(256)),
            "".to_string(),
            "error".to_string(),
            50,
            "Command failed".to_string(),
        );

        state.record_hook_result(1, failed_result);
        assert_eq!(state.completed_hooks, 2);
        assert_eq!(state.status, ExecutionStatus::Failed);
        assert!(state.is_complete());
        assert!(state.error_message.is_some());

        // Test cancellation
        let mut cancelled_state = HookExecutionState::new(
            PathBuf::from("/test"),
            "hash".to_string(),
            "config".to_string(),
            1,
        );
        cancelled_state.mark_cancelled(Some("User cancelled".to_string()));
        assert_eq!(cancelled_state.status, ExecutionStatus::Cancelled);
        assert!(cancelled_state.is_complete());
    }

    #[test]
    fn test_progress_display() {
        let directory_path = PathBuf::from("/test/dir");
        let directory_hash = "test_hash".to_string();
        let config_hash = "config_hash".to_string();
        let mut state = HookExecutionState::new(directory_path, directory_hash, config_hash, 2);

        // Running state
        let display = state.progress_display();
        assert!(display.contains("0 of 2"));

        // Running with current hook
        state.mark_hook_running(0);
        let display = state.progress_display();
        assert!(display.contains("Executing hook 1 of 2"));

        // Completed state
        state.status = ExecutionStatus::Completed;
        state.current_hook_index = None;
        let display = state.progress_display();
        assert_eq!(display, "All hooks completed successfully");

        // Failed state
        state.status = ExecutionStatus::Failed;
        state.error_message = Some("Test error".to_string());
        let display = state.progress_display();
        assert!(display.contains("Hook execution failed: Test error"));
    }

    #[tokio::test]
    async fn test_state_directory_cleanup() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        // Create multiple states with different statuses
        let completed_state = HookExecutionState {
            directory_hash: "completed_hash".to_string(),
            directory_path: PathBuf::from("/completed"),
            config_hash: "config1".to_string(),
            status: ExecutionStatus::Completed,
            total_hooks: 1,
            completed_hooks: 1,
            current_hook_index: None,
            hook_results: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(1),
            finished_at: Some(Utc::now() - chrono::Duration::minutes(30)),
            error_message: None,
        };

        let running_state = HookExecutionState {
            directory_hash: "running_hash".to_string(),
            directory_path: PathBuf::from("/running"),
            config_hash: "config2".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 2,
            completed_hooks: 1,
            current_hook_index: Some(1),
            hook_results: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::minutes(5),
            finished_at: None,
            error_message: None,
        };

        let failed_state = HookExecutionState {
            directory_hash: "failed_hash".to_string(),
            directory_path: PathBuf::from("/failed"),
            config_hash: "config3".to_string(),
            status: ExecutionStatus::Failed,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: None,
            hook_results: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(2),
            finished_at: Some(Utc::now() - chrono::Duration::hours(1)),
            error_message: Some("Test failure".to_string()),
        };

        // Save all states
        state_manager.save_state(&completed_state).await.unwrap();
        state_manager.save_state(&running_state).await.unwrap();
        state_manager.save_state(&failed_state).await.unwrap();

        // Verify all states exist
        let states = state_manager.list_active_states().await.unwrap();
        assert_eq!(states.len(), 3);

        // Clean up completed states
        let cleaned = state_manager.cleanup_state_directory().await.unwrap();
        assert_eq!(cleaned, 2); // Should clean up completed and failed states

        // Verify only running state remains
        let remaining_states = state_manager.list_active_states().await.unwrap();
        assert_eq!(remaining_states.len(), 1);
        assert_eq!(remaining_states[0].directory_hash, "running_hash");
    }

    #[tokio::test]
    async fn test_cleanup_orphaned_states() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        // Create an old running state (orphaned)
        let orphaned_state = HookExecutionState {
            directory_hash: "orphaned_hash".to_string(),
            directory_path: PathBuf::from("/orphaned"),
            config_hash: "config".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: Some(0),
            hook_results: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(3),
            finished_at: None,
            error_message: None,
        };

        // Create a recent running state (not orphaned)
        let recent_state = HookExecutionState {
            directory_hash: "recent_hash".to_string(),
            directory_path: PathBuf::from("/recent"),
            config_hash: "config".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: Some(0),
            hook_results: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::minutes(5),
            finished_at: None,
            error_message: None,
        };

        // Save both states
        state_manager.save_state(&orphaned_state).await.unwrap();
        state_manager.save_state(&recent_state).await.unwrap();

        // Clean up orphaned states older than 1 hour
        let cleaned = state_manager
            .cleanup_orphaned_states(chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(cleaned, 1); // Should clean up only the orphaned state

        // Verify only recent state remains
        let remaining_states = state_manager.list_active_states().await.unwrap();
        assert_eq!(remaining_states.len(), 1);
        assert_eq!(remaining_states[0].directory_hash, "recent_hash");
    }

    #[tokio::test]
    async fn test_corrupted_state_file_handling() {
        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().join("state");
        let state_manager = StateManager::new(state_dir.clone());

        // Ensure state directory exists
        state_manager.ensure_state_dir().await.unwrap();

        // Write corrupted JSON to a state file
        let corrupted_file = state_dir.join("corrupted.json");
        tokio::fs::write(&corrupted_file, "{invalid json}")
            .await
            .unwrap();

        // List active states should handle the corrupted file gracefully
        let states = state_manager.list_active_states().await.unwrap();
        assert_eq!(states.len(), 0); // Corrupted file should be skipped

        // Cleanup should remove the corrupted file
        let cleaned = state_manager.cleanup_state_directory().await.unwrap();
        assert_eq!(cleaned, 1);

        // Verify the corrupted file is gone
        assert!(!corrupted_file.exists());
    }
}
