//! State management and persistence for hook execution

use super::{HookResult, HookStatus};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// State of hook execution for a directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionState {
    /// The directory this state is for
    pub directory: PathBuf,
    /// SHA256 hash of the configuration
    pub config_hash: String,
    /// Total number of hooks to execute
    pub total_hooks: usize,
    /// Number of hooks that have completed
    pub completed_hooks: usize,
    /// Overall status of the execution
    pub status: HookStatus,
    /// Index of the currently executing hook (if any)
    pub current_hook_index: Option<usize>,
    /// Results of all hook executions
    pub results: Vec<HookResult>,
    /// Timestamp when execution started
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp when execution finished (if applicable)
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl HookExecutionState {
    /// Create a new execution state
    pub fn new(directory: PathBuf, config_hash: String, total_hooks: usize) -> Self {
        Self {
            directory,
            config_hash,
            total_hooks,
            completed_hooks: 0,
            status: HookStatus::Pending,
            current_hook_index: None,
            results: Vec::with_capacity(total_hooks),
            started_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    /// Update the state with a hook result
    pub fn update_result(&mut self, index: usize, result: HookResult) {
        // Ensure we have space for this result
        while self.results.len() <= index {
            self.results.push(HookResult::pending(result.hook.clone()));
        }

        self.results[index] = result.clone();

        // Update counters
        if matches!(result.status, HookStatus::Completed | HookStatus::Failed(_)) {
            self.completed_hooks += 1;

            // Clear current hook if it was this one
            if self.current_hook_index == Some(index) {
                self.current_hook_index = None;
            }
        }

        // Update overall status
        self.update_overall_status();
    }

    /// Mark a hook as currently executing
    pub fn mark_running(&mut self, index: usize) {
        self.current_hook_index = Some(index);
        self.status = HookStatus::Running;
    }

    /// Update the overall status based on individual results
    fn update_overall_status(&mut self) {
        let has_failure = self
            .results
            .iter()
            .any(|r| matches!(r.status, HookStatus::Failed(_)));

        if has_failure {
            self.status = HookStatus::Failed("One or more hooks failed".to_string());
            self.finished_at = Some(chrono::Utc::now());
        } else if self.completed_hooks == self.total_hooks {
            self.status = HookStatus::Completed;
            self.finished_at = Some(chrono::Utc::now());
        } else if self.current_hook_index.is_some() {
            self.status = HookStatus::Running;
        } else {
            self.status = HookStatus::Pending;
        }
    }

    /// Get a status string for display
    pub fn status_string(&self) -> String {
        match &self.status {
            HookStatus::Completed => "cuenv Activated".to_string(),
            HookStatus::Failed(_) => "cuenv Failed".to_string(),
            _ => format!("{}/{} Completed", self.completed_hooks, self.total_hooks),
        }
    }

    /// Check if execution is complete
    pub fn is_complete(&self) -> bool {
        matches!(self.status, HookStatus::Completed | HookStatus::Failed(_))
    }
}

/// Manages persistent state storage
pub struct StateManager {
    state_dir: PathBuf,
}

impl StateManager {
    /// Create a new state manager
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the default state directory (~/.cuenv/state)
    pub fn default_state_dir() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        Ok(home.join(".cuenv").join("state"))
    }

    /// Ensure the state directory exists
    pub async fn ensure_state_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.state_dir)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.state_dir.clone().into_boxed_path()),
                operation: "create state directory".to_string(),
            })?;
        Ok(())
    }

    /// Get the path for a state file
    fn state_file_path(&self, dir_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", dir_hash))
    }

    /// Save execution state to disk
    pub async fn save_state(&self, state: &HookExecutionState) -> Result<()> {
        self.ensure_state_dir().await?;

        let path = self.state_file_path(&state.config_hash);
        debug!("Saving state to: {}", path.display());

        let json = serde_json::to_string_pretty(state)
            .map_err(|e| Error::configuration(format!("Failed to serialize state: {}", e)))?;

        let mut file = fs::File::create(&path).await.map_err(|e| Error::Io {
            source: e,
            path: Some(path.clone().into_boxed_path()),
            operation: "create state file".to_string(),
        })?;

        file.write_all(json.as_bytes())
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(path.into_boxed_path()),
                operation: "write state file".to_string(),
            })?;

        info!("State saved successfully");
        Ok(())
    }

    /// Load execution state from disk
    pub async fn load_state(&self, dir_hash: &str) -> Result<Option<HookExecutionState>> {
        let path = self.state_file_path(dir_hash);

        if !path.exists() {
            debug!("No state file found at: {}", path.display());
            return Ok(None);
        }

        debug!("Loading state from: {}", path.display());

        let contents = fs::read_to_string(&path).await.map_err(|e| Error::Io {
            source: e,
            path: Some(path.clone().into_boxed_path()),
            operation: "read state file".to_string(),
        })?;

        let state = serde_json::from_str(&contents)
            .map_err(|e| Error::configuration(format!("Failed to parse state file: {}", e)))?;

        info!("State loaded successfully");
        Ok(Some(state))
    }

    /// Delete a state file
    pub async fn delete_state(&self, dir_hash: &str) -> Result<()> {
        let path = self.state_file_path(dir_hash);

        if path.exists() {
            fs::remove_file(&path).await.map_err(|e| Error::Io {
                source: e,
                path: Some(path.clone().into_boxed_path()),
                operation: "delete state file".to_string(),
            })?;
            debug!("Deleted state file: {}", path.display());
        }

        Ok(())
    }

    /// List all current state files
    pub async fn list_states(&self) -> Result<Vec<String>> {
        self.ensure_state_dir().await?;

        let mut entries = fs::read_dir(&self.state_dir).await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "read state directory".to_string(),
        })?;

        let mut hashes = Vec::new();

        while let Some(entry) = entries.next_entry().await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "read directory entry".to_string(),
        })? {
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".json")
            {
                hashes.push(name.trim_end_matches(".json").to_string());
            }
        }

        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hook_execution_state_new() {
        let state = HookExecutionState::new(PathBuf::from("/test"), "hash123".to_string(), 5);

        assert_eq!(state.directory, PathBuf::from("/test"));
        assert_eq!(state.config_hash, "hash123");
        assert_eq!(state.total_hooks, 5);
        assert_eq!(state.completed_hooks, 0);
        assert_eq!(state.status, HookStatus::Pending);
        assert!(state.current_hook_index.is_none());
        assert!(state.results.is_empty());
        assert!(state.finished_at.is_none());
    }

    #[test]
    fn test_state_status_string() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 3);

        assert_eq!(state.status_string(), "0/3 Completed");

        state.completed_hooks = 2;
        assert_eq!(state.status_string(), "2/3 Completed");

        state.status = HookStatus::Completed;
        assert_eq!(state.status_string(), "cuenv Activated");

        state.status = HookStatus::Failed("error".to_string());
        assert_eq!(state.status_string(), "cuenv Failed");
    }

    #[test]
    fn test_state_is_complete() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 2);

        assert!(!state.is_complete());

        state.status = HookStatus::Running;
        assert!(!state.is_complete());

        state.status = HookStatus::Completed;
        assert!(state.is_complete());

        state.status = HookStatus::Failed("error".to_string());
        assert!(state.is_complete());
    }

    #[tokio::test]
    async fn test_state_manager_paths() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::new(temp_dir.path().to_path_buf());

        let path = manager.state_file_path("test_hash");
        assert_eq!(path, temp_dir.path().join("test_hash.json"));
    }

    #[tokio::test]
    async fn test_state_manager_save_load() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::new(temp_dir.path().to_path_buf());

        let state = HookExecutionState::new(PathBuf::from("/test"), "test_hash".to_string(), 2);

        // Save state
        manager.save_state(&state).await.unwrap();

        // Load state
        let loaded = manager.load_state("test_hash").await.unwrap();
        assert!(loaded.is_some());

        let loaded_state = loaded.unwrap();
        assert_eq!(loaded_state.directory, PathBuf::from("/test"));
        assert_eq!(loaded_state.config_hash, "test_hash");
        assert_eq!(loaded_state.total_hooks, 2);
    }

    #[tokio::test]
    async fn test_state_manager_delete() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::new(temp_dir.path().to_path_buf());

        let state = HookExecutionState::new(PathBuf::from("/test"), "delete_test".to_string(), 1);

        // Save state
        manager.save_state(&state).await.unwrap();

        // Verify it exists
        let loaded = manager.load_state("delete_test").await.unwrap();
        assert!(loaded.is_some());

        // Delete it
        manager.delete_state("delete_test").await.unwrap();

        // Verify it's gone
        let loaded = manager.load_state("delete_test").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_state_manager_list() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::new(temp_dir.path().to_path_buf());

        // Save multiple states
        for i in 1..=3 {
            let state = HookExecutionState::new(
                PathBuf::from(format!("/test{}", i)),
                format!("hash{}", i),
                1,
            );
            manager.save_state(&state).await.unwrap();
        }

        // List states
        let mut states = manager.list_states().await.unwrap();
        states.sort();

        assert_eq!(states.len(), 3);
        assert_eq!(states, vec!["hash1", "hash2", "hash3"]);
    }
}
