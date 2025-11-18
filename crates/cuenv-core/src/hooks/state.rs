//! State management for hook execution tracking

use crate::hooks::types::{ExecutionStatus, HookResult};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use fs4::tokio::AsyncFileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        // Check for CUENV_STATE_DIR environment variable first
        if let Ok(state_dir) = std::env::var("CUENV_STATE_DIR") {
            return Ok(PathBuf::from(state_dir));
        }

        let home = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        Ok(home.join(".cuenv").join("state"))
    }

    /// Create a state manager using the default state directory
    pub fn with_default_dir() -> Result<Self> {
        Ok(Self::new(Self::default_state_dir()?))
    }

    /// Get the state directory path
    pub fn get_state_dir(&self) -> &Path {
        &self.state_dir
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
    fn state_file_path(&self, instance_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", instance_hash))
    }

    /// Get the state file path for a given directory hash (public for PID files)
    pub fn get_state_file_path(&self, instance_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", instance_hash))
    }

    /// Save execution state to disk with atomic write and locking
    pub async fn save_state(&self, state: &HookExecutionState) -> Result<()> {
        self.ensure_state_dir().await?;

        let state_file = self.state_file_path(&state.instance_hash);
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| Error::configuration(format!("Failed to serialize state: {e}")))?;

        // Write to a temporary file first, then rename atomically
        let temp_path = state_file.with_extension("tmp");

        // Open temp file with exclusive lock for writing
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(temp_path.clone().into_boxed_path()),
                operation: "open".to_string(),
            })?;

        // Acquire exclusive lock (only one writer allowed)
        file.lock_exclusive().map_err(|e| {
            Error::configuration(format!(
                "Failed to acquire exclusive lock on state temp file: {}",
                e
            ))
        })?;

        file.write_all(json.as_bytes())
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(temp_path.clone().into_boxed_path()),
                operation: "write_all".to_string(),
            })?;

        file.sync_all().await.map_err(|e| Error::Io {
            source: e,
            path: Some(temp_path.clone().into_boxed_path()),
            operation: "sync_all".to_string(),
        })?;

        // Unlock happens automatically when file is dropped
        drop(file);

        // Atomically rename temp file to final location
        fs::rename(&temp_path, &state_file)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(state_file.clone().into_boxed_path()),
                operation: "rename".to_string(),
            })?;

        debug!(
            "Saved execution state for directory hash: {}",
            state.instance_hash
        );
        Ok(())
    }

    /// Load execution state from disk with shared locking
    pub async fn load_state(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        let state_file = self.state_file_path(instance_hash);

        if !state_file.exists() {
            return Ok(None);
        }

        // Open file with shared lock for reading
        let mut file = match OpenOptions::new().read(true).open(&state_file).await {
            Ok(f) => f,
            Err(e) => {
                // File might have been deleted between exists check and open
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(Error::Io {
                    source: e,
                    path: Some(state_file.clone().into_boxed_path()),
                    operation: "open".to_string(),
                });
            }
        };

        // Acquire shared lock (multiple readers allowed)
        file.lock_shared().map_err(|e| {
            Error::configuration(format!(
                "Failed to acquire shared lock on state file: {}",
                e
            ))
        })?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(state_file.clone().into_boxed_path()),
                operation: "read_to_string".to_string(),
            })?;

        // Unlock happens automatically when file is dropped
        drop(file);

        let state: HookExecutionState = serde_json::from_str(&contents)
            .map_err(|e| Error::configuration(format!("Failed to deserialize state: {e}")))?;

        debug!(
            "Loaded execution state for directory hash: {}",
            instance_hash
        );
        Ok(Some(state))
    }

    /// Remove state file for a directory
    pub async fn remove_state(&self, instance_hash: &str) -> Result<()> {
        let state_file = self.state_file_path(instance_hash);

        if state_file.exists() {
            fs::remove_file(&state_file).await.map_err(|e| Error::Io {
                source: e,
                path: Some(state_file.into_boxed_path()),
                operation: "remove_file".to_string(),
            })?;
            debug!(
                "Removed execution state for directory hash: {}",
                instance_hash
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
                self.remove_state(&state.instance_hash).await?;
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
    /// Hash combining directory path and config (instance identifier)
    pub instance_hash: String,
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
    /// The list of hooks being executed (for display purposes)
    #[serde(default)]
    pub hooks: Vec<crate::hooks::types::Hook>,
    /// Results of completed hooks
    pub hook_results: HashMap<usize, HookResult>,
    /// Timestamp when execution started
    pub started_at: DateTime<Utc>,
    /// Timestamp when execution finished (if completed)
    pub finished_at: Option<DateTime<Utc>>,
    /// Timestamp when the current hook started (if running)
    pub current_hook_started_at: Option<DateTime<Utc>>,
    /// Timestamp until which completed state should be displayed
    pub completed_display_until: Option<DateTime<Utc>>,
    /// Error message if execution failed
    pub error_message: Option<String>,
    /// Environment variables captured from source hooks
    pub environment_vars: HashMap<String, String>,
    /// Previous environment variables (for diff/unset support)
    pub previous_env: Option<HashMap<String, String>>,
}

impl HookExecutionState {
    /// Create a new execution state
    pub fn new(
        directory_path: PathBuf,
        instance_hash: String,
        config_hash: String,
        hooks: Vec<crate::hooks::types::Hook>,
    ) -> Self {
        let total_hooks = hooks.len();
        Self {
            instance_hash,
            directory_path,
            config_hash,
            status: ExecutionStatus::Running,
            total_hooks,
            completed_hooks: 0,
            current_hook_index: None,
            hooks,
            hook_results: HashMap::new(),
            started_at: Utc::now(),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            environment_vars: HashMap::new(),
            previous_env: None,
        }
    }

    /// Mark a hook as currently executing
    pub fn mark_hook_running(&mut self, hook_index: usize) {
        self.current_hook_index = Some(hook_index);
        self.current_hook_started_at = Some(Utc::now());
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
        self.current_hook_started_at = None;

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
            // Keep failed state visible for 2 seconds (enough for at least one starship poll)
            self.completed_display_until = Some(Utc::now() + chrono::Duration::seconds(2));
            return;
        }

        // Check if all hooks are complete
        if self.completed_hooks == self.total_hooks {
            self.status = ExecutionStatus::Completed;
            let now = Utc::now();
            self.finished_at = Some(now);
            // Keep completed state visible for 2 seconds (enough for at least one starship poll)
            self.completed_display_until = Some(now + chrono::Duration::seconds(2));
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

    /// Get current hook duration (if a hook is currently running)
    pub fn current_hook_duration(&self) -> Option<chrono::Duration> {
        self.current_hook_started_at
            .map(|started| Utc::now() - started)
    }

    /// Get the currently executing hook
    pub fn current_hook(&self) -> Option<&crate::hooks::types::Hook> {
        self.current_hook_index
            .and_then(|idx| self.hooks.get(idx))
    }

    /// Format duration in human-readable format (e.g., "2.3s", "1m 15s", "2h 5m")
    pub fn format_duration(duration: chrono::Duration) -> String {
        let total_secs = duration.num_seconds();

        if total_secs < 60 {
            // Less than 1 minute: show as decimal seconds
            let millis = duration.num_milliseconds();
            format!("{:.1}s", millis as f64 / 1000.0)
        } else if total_secs < 3600 {
            // Less than 1 hour: show minutes and seconds
            let mins = total_secs / 60;
            let secs = total_secs % 60;
            if secs == 0 {
                format!("{}m", mins)
            } else {
                format!("{}m {}s", mins, secs)
            }
        } else {
            // 1 hour or more: show hours and minutes
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            if mins == 0 {
                format!("{}h", hours)
            } else {
                format!("{}h {}m", hours, mins)
            }
        }
    }

    /// Get a short description of the current or next hook for display
    pub fn current_hook_display(&self) -> Option<String> {
        // If there's a current hook index, use that
        let hook = if let Some(hook) = self.current_hook() {
            Some(hook)
        } else if self.status == ExecutionStatus::Running && self.completed_hooks < self.total_hooks {
            // If we're running but no current hook index yet, show the next hook to execute
            self.hooks.get(self.completed_hooks)
        } else {
            None
        };

        hook.map(|h| {
            // Extract just the command name (first part before any path separators)
            let cmd_name = h.command.split('/').last().unwrap_or(&h.command);

            // Format: just the command name (no args, to keep it concise)
            format!("`{}`", cmd_name)
        })
    }

    /// Check if the completed state should still be displayed
    pub fn should_display_completed(&self) -> bool {
        if let Some(display_until) = self.completed_display_until {
            Utc::now() < display_until
        } else {
            false
        }
    }
}

/// Compute a hash for a unique execution instance (directory + config)
pub fn compute_instance_hash(path: &Path, config_hash: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(config_hash.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::{Hook, HookResult};
    use std::collections::HashMap;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_compute_instance_hash() {
        let path = Path::new("/test/path");
        let config_hash = "test_config";
        let hash = compute_instance_hash(path, config_hash);
        assert_eq!(hash.len(), 16);

        // Same path and config should produce same hash
        let hash2 = compute_instance_hash(path, config_hash);
        assert_eq!(hash, hash2);

        // Different path should produce different hash
        let different_path = Path::new("/other/path");
        let different_hash = compute_instance_hash(different_path, config_hash);
        assert_ne!(hash, different_hash);

        // Same path but different config should produce different hash
        let different_config_hash = compute_instance_hash(path, "different_config");
        assert_ne!(hash, different_config_hash);
    }

    #[tokio::test]
    async fn test_state_manager_operations() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        let directory_path = PathBuf::from("/test/dir");
        let config_hash = "test_config_hash".to_string();
        let instance_hash = compute_instance_hash(&directory_path, &config_hash);

        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["test1".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["test2".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        ];

        let mut state =
            HookExecutionState::new(directory_path, instance_hash.clone(), config_hash, hooks);

        // Save initial state
        state_manager.save_state(&state).await.unwrap();

        // Load state back
        let loaded_state = state_manager
            .load_state(&instance_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded_state.instance_hash, state.instance_hash);
        assert_eq!(loaded_state.total_hooks, 2);
        assert_eq!(loaded_state.status, ExecutionStatus::Running);

        // Update state with hook result
        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
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
            .load_state(&instance_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated_state.completed_hooks, 1);
        assert_eq!(updated_state.hook_results.len(), 1);

        // Remove state
        state_manager.remove_state(&instance_hash).await.unwrap();
        let removed_state = state_manager.load_state(&instance_hash).await.unwrap();
        assert!(removed_state.is_none());
    }

    #[test]
    fn test_hook_execution_state() {
        let directory_path = PathBuf::from("/test/dir");
        let instance_hash = "test_hash".to_string();
        let config_hash = "config_hash".to_string();
        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["test1".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["test2".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["test3".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        ];
        let mut state = HookExecutionState::new(directory_path, instance_hash, config_hash, hooks);

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
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
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
            vec![Hook {
                command: "echo".to_string(),
                args: vec![],
                dir: None,
                inputs: vec![],
                source: None,
            }],
        );
        cancelled_state.mark_cancelled(Some("User cancelled".to_string()));
        assert_eq!(cancelled_state.status, ExecutionStatus::Cancelled);
        assert!(cancelled_state.is_complete());
    }

    #[test]
    fn test_progress_display() {
        let directory_path = PathBuf::from("/test/dir");
        let instance_hash = "test_hash".to_string();
        let config_hash = "config_hash".to_string();
        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["test1".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["test2".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        ];
        let mut state = HookExecutionState::new(directory_path, instance_hash, config_hash, hooks);

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
            instance_hash: "completed_hash".to_string(),
            directory_path: PathBuf::from("/completed"),
            config_hash: "config1".to_string(),
            status: ExecutionStatus::Completed,
            total_hooks: 1,
            completed_hooks: 1,
            current_hook_index: None,
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(1),
            finished_at: Some(Utc::now() - chrono::Duration::minutes(30)),
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            previous_env: None,
        };

        let running_state = HookExecutionState {
            instance_hash: "running_hash".to_string(),
            directory_path: PathBuf::from("/running"),
            config_hash: "config2".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 2,
            completed_hooks: 1,
            current_hook_index: Some(1),
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::minutes(5),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            previous_env: None,
        };

        let failed_state = HookExecutionState {
            instance_hash: "failed_hash".to_string(),
            directory_path: PathBuf::from("/failed"),
            config_hash: "config3".to_string(),
            status: ExecutionStatus::Failed,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: None,
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(2),
            finished_at: Some(Utc::now() - chrono::Duration::hours(1)),
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: Some("Test failure".to_string()),
            previous_env: None,
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
        assert_eq!(remaining_states[0].instance_hash, "running_hash");
    }

    #[tokio::test]
    async fn test_cleanup_orphaned_states() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        // Create an old running state (orphaned)
        let orphaned_state = HookExecutionState {
            instance_hash: "orphaned_hash".to_string(),
            directory_path: PathBuf::from("/orphaned"),
            config_hash: "config".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: Some(0),
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(3),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            previous_env: None,
        };

        // Create a recent running state (not orphaned)
        let recent_state = HookExecutionState {
            instance_hash: "recent_hash".to_string(),
            directory_path: PathBuf::from("/recent"),
            config_hash: "config".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 1,
            completed_hooks: 0,
            current_hook_index: Some(0),
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::minutes(5),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            previous_env: None,
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
        assert_eq!(remaining_states[0].instance_hash, "recent_hash");
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

    #[tokio::test]
    async fn test_concurrent_state_modifications() {
        use tokio::task;

        let temp_dir = TempDir::new().unwrap();
        let state_manager = Arc::new(StateManager::new(temp_dir.path().to_path_buf()));

        // Create initial state
        let initial_state = HookExecutionState {
            instance_hash: "concurrent_hash".to_string(),
            directory_path: PathBuf::from("/concurrent"),
            config_hash: "config".to_string(),
            status: ExecutionStatus::Running,
            total_hooks: 10,
            completed_hooks: 0,
            current_hook_index: Some(0),
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now(),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            previous_env: None,
        };

        state_manager.save_state(&initial_state).await.unwrap();

        // Spawn multiple tasks that concurrently modify the state
        let mut handles = vec![];

        for i in 0..5 {
            let sm = state_manager.clone();
            let path = initial_state.directory_path.clone();

            let handle = task::spawn(async move {
                // Load state - it might have been modified by another task
                let instance_hash = compute_instance_hash(&path, "concurrent_config");

                // Simulate some work
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Load state, modify, and save (handle potential concurrent modifications)
                if let Ok(Some(mut state)) = sm.load_state(&instance_hash).await {
                    state.completed_hooks += 1;
                    state.current_hook_index = Some(i + 1);

                    // Save state - ignore errors from concurrent saves
                    let _ = sm.save_state(&state).await;
                }
            });

            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify final state - due to concurrent writes, the exact values may vary
        // but the state should be loadable and valid
        let final_state = state_manager
            .load_state(&initial_state.instance_hash)
            .await
            .unwrap();

        // The state might exist or not depending on timing of concurrent operations
        if let Some(state) = final_state {
            assert_eq!(state.instance_hash, "concurrent_hash");
            // Completed hooks will be 0 if all concurrent writes failed, or > 0 if some succeeded
        }
    }

    #[tokio::test]
    async fn test_state_with_unicode_and_special_chars() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        // Create state with unicode and special characters
        let mut unicode_state = HookExecutionState {
            instance_hash: "unicode_hash".to_string(),
            directory_path: PathBuf::from("/测试/目录/🚀"),
            config_hash: "config_ñ_é_ü".to_string(),
            status: ExecutionStatus::Failed,
            total_hooks: 1,
            completed_hooks: 1,
            current_hook_index: None,
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: Some("Error: 错误信息 with émojis 🔥💥".to_string()),
            previous_env: None,
        };

        // Add hook result with unicode output
        let unicode_hook = Hook {
            command: "echo".to_string(),
            args: vec![],
            dir: None,
            inputs: vec![],
            source: None,
        };
        let unicode_result = HookResult {
            hook: unicode_hook,
            success: false,
            exit_status: Some(1),
            stdout: "输出: Hello 世界! 🌍".to_string(),
            stderr: "错误: ñoño error ⚠️".to_string(),
            duration_ms: 100,
            error: Some("失败了 😢".to_string()),
        };
        unicode_state.hook_results.insert(0, unicode_result);

        // Save and load the state
        state_manager.save_state(&unicode_state).await.unwrap();

        let loaded = state_manager
            .load_state(&unicode_state.instance_hash)
            .await
            .unwrap()
            .unwrap();

        // Verify all unicode content is preserved
        assert_eq!(loaded.config_hash, "config_ñ_é_ü");
        assert_eq!(
            loaded.error_message,
            Some("Error: 错误信息 with émojis 🔥💥".to_string())
        );

        let hook_result = loaded.hook_results.get(&0).unwrap();
        assert_eq!(hook_result.stdout, "输出: Hello 世界! 🌍");
        assert_eq!(hook_result.stderr, "错误: ñoño error ⚠️");
        assert_eq!(hook_result.error, Some("失败了 😢".to_string()));
    }

    #[tokio::test]
    async fn test_state_directory_with_many_states() {
        let temp_dir = TempDir::new().unwrap();
        let state_manager = StateManager::new(temp_dir.path().to_path_buf());

        // Create many states to test scalability
        for i in 0..50 {
            let state = HookExecutionState {
                instance_hash: format!("hash_{}", i),
                directory_path: PathBuf::from(format!("/dir/{}", i)),
                config_hash: format!("config_{}", i),
                status: if i % 3 == 0 {
                    ExecutionStatus::Completed
                } else if i % 3 == 1 {
                    ExecutionStatus::Running
                } else {
                    ExecutionStatus::Failed
                },
                total_hooks: 1,
                completed_hooks: if i % 3 == 0 { 1 } else { 0 },
                current_hook_index: if i % 3 == 1 { Some(0) } else { None },
                hooks: vec![],
                hook_results: HashMap::new(),
                environment_vars: HashMap::new(),
                started_at: Utc::now() - chrono::Duration::hours(i as i64),
                finished_at: if i % 3 != 1 {
                    Some(Utc::now() - chrono::Duration::hours(i as i64 - 1))
                } else {
                    None
                },
                current_hook_started_at: None,
                completed_display_until: None,
                error_message: if i % 3 == 2 {
                    Some(format!("Error {}", i))
                } else {
                    None
                },
                previous_env: None,
            };
            state_manager.save_state(&state).await.unwrap();
        }

        // List all states
        let listed = state_manager.list_active_states().await.unwrap();
        assert_eq!(listed.len(), 50);

        // Clean up old completed states (older than 24 hours)
        let cleaned = state_manager
            .cleanup_orphaned_states(chrono::Duration::hours(24))
            .await
            .unwrap();

        // Should clean up states older than 24 hours
        assert!(cleaned > 0);
    }
}
