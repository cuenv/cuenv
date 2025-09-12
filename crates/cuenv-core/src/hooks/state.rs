//! State persistence for hook execution
//!
//! This module handles persisting hook execution state to disk and managing
//! the lifecycle of background hook execution sessions.

use super::HookExecutionState;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use tokio::sync::RwLock;
use std::sync::Arc;

/// Manager for hook execution state persistence
#[derive(Debug)]
pub struct StateManager {
    /// Base directory for state files (~/.cuenv/state/)
    state_dir: PathBuf,
    /// In-memory cache of active states
    active_states: Arc<RwLock<HashMap<String, HookExecutionState>>>,
}

impl StateManager {
    /// Create a new state manager
    /// 
    /// # Errors
    /// Returns an error if the state directory cannot be created or accessed
    pub fn new() -> Result<Self> {
        let state_dir = Self::get_state_dir()?;
        
        // Ensure state directory exists
        if !state_dir.exists() {
            fs::create_dir_all(&state_dir).map_err(|e| Error::Io {
                source: e,
                path: Some(state_dir.clone().into()),
                operation: "create state directory".to_string(),
            })?;
        }

        Ok(Self {
            state_dir,
            active_states: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get the state directory path (~/.cuenv/state/)
    fn get_state_dir() -> Result<PathBuf> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        
        Ok(home_dir.join(".cuenv").join("state"))
    }

    /// Generate a state key for a directory (hash of canonical path)
    fn directory_key(directory: &Path) -> Result<String> {
        let canonical = directory.canonicalize().map_err(|e| Error::Io {
            source: e,
            path: Some(directory.into()),
            operation: "canonicalize directory path".to_string(),
        })?;
        
        // Use a simple hash of the path
        Ok(format!("{:x}", md5::compute(canonical.to_string_lossy().as_bytes())))
    }

    /// Get the file path for a directory's state
    fn state_file_path(&self, directory_key: &str) -> PathBuf {
        self.state_dir.join(format!("{directory_key}.json"))
    }

    /// Save hook execution state to disk
    /// 
    /// # Errors
    /// Returns an error if the state cannot be serialized or written to disk
    pub async fn save_state(&self, state: &HookExecutionState) -> Result<()> {
        let directory_key = Self::directory_key(&state.directory)?;
        let file_path = self.state_file_path(&directory_key);
        
        // Serialize state to JSON
        let json_content = serde_json::to_string_pretty(state).map_err(|e| {
            Error::configuration(format!("Failed to serialize hook state: {e}"))
        })?;
        
        // Write to file atomically
        let temp_path = format!("{}.tmp", file_path.display());
        fs::write(&temp_path, json_content).map_err(|e| Error::Io {
            source: e,
            path: Some(PathBuf::from(temp_path.clone()).into()),
            operation: "write state file".to_string(),
        })?;
        
        fs::rename(&temp_path, &file_path).map_err(|e| Error::Io {
            source: e,
            path: Some(file_path.clone().into()),
            operation: "atomic rename of state file".to_string(),
        })?;
        
        // Update in-memory cache
        let mut active_states = self.active_states.write().await;
        active_states.insert(directory_key, state.clone());
        
        Ok(())
    }

    /// Load hook execution state from disk
    /// 
    /// # Errors
    /// Returns an error if the state file cannot be read or parsed
    pub async fn load_state(&self, directory: &Path) -> Result<Option<HookExecutionState>> {
        let directory_key = Self::directory_key(directory)?;
        
        // Check in-memory cache first
        {
            let active_states = self.active_states.read().await;
            if let Some(state) = active_states.get(&directory_key) {
                return Ok(Some(state.clone()));
            }
        }
        
        let file_path = self.state_file_path(&directory_key);
        
        if !file_path.exists() {
            return Ok(None);
        }
        
        let content = fs::read_to_string(&file_path).map_err(|e| Error::Io {
            source: e,
            path: Some(file_path.clone().into()),
            operation: "read state file".to_string(),
        })?;
        
        let state: HookExecutionState = serde_json::from_str(&content).map_err(|e| {
            Error::configuration(format!("Failed to parse hook state file: {e}"))
        })?;
        
        // Update in-memory cache
        let mut active_states = self.active_states.write().await;
        active_states.insert(directory_key, state.clone());
        
        Ok(Some(state))
    }

    /// Remove hook execution state (when finished)
    /// 
    /// # Errors
    /// Returns an error if the state file cannot be deleted
    pub async fn remove_state(&self, directory: &Path) -> Result<()> {
        let directory_key = Self::directory_key(directory)?;
        let file_path = self.state_file_path(&directory_key);
        
        // Remove from in-memory cache
        {
            let mut active_states = self.active_states.write().await;
            active_states.remove(&directory_key);
        }
        
        // Remove file if it exists
        if file_path.exists() {
            fs::remove_file(&file_path).map_err(|e| Error::Io {
                source: e,
                path: Some(file_path.into()),
                operation: "remove state file".to_string(),
            })?;
        }
        
        Ok(())
    }

    /// List all active hook execution states
    /// 
    /// # Errors
    /// Returns an error if the state directory cannot be read
    pub async fn list_active_states(&self) -> Result<Vec<HookExecutionState>> {
        let mut states = Vec::new();
        
        // First, load any states from disk that aren't in memory
        if self.state_dir.exists() {
            let entries = fs::read_dir(&self.state_dir).map_err(|e| Error::Io {
                source: e,
                path: Some(self.state_dir.clone().into()),
                operation: "read state directory".to_string(),
            })?;
            
            for entry in entries {
                let entry = entry.map_err(|e| Error::Io {
                    source: e,
                    path: Some(self.state_dir.clone().into()),
                    operation: "iterate state directory".to_string(),
                })?;
                
                let path = entry.path();
                if path.extension() == Some(std::ffi::OsStr::new("json")) {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Ok(state) = serde_json::from_str::<HookExecutionState>(&content) {
                            // Only include non-finished states
                            if !state.is_finished() {
                                states.push(state);
                            }
                        }
                    }
                }
            }
        }
        
        Ok(states)
    }

    /// Clean up finished states (remove old completed/failed executions)
    /// 
    /// # Errors
    /// Returns an error if state files cannot be accessed or removed
    pub async fn cleanup_finished_states(&self) -> Result<()> {
        if !self.state_dir.exists() {
            return Ok(());
        }
        
        let entries = fs::read_dir(&self.state_dir).map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into()),
            operation: "read state directory for cleanup".to_string(),
        })?;
        
        for entry in entries {
            let entry = entry.map_err(|e| Error::Io {
                source: e,
                path: Some(self.state_dir.clone().into()),
                operation: "iterate state directory for cleanup".to_string(),
            })?;
            
            let path = entry.path();
            if path.extension() == Some(std::ffi::OsStr::new("json")) {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(state) = serde_json::from_str::<HookExecutionState>(&content) {
                        // Remove finished states
                        if state.is_finished() {
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}

/// Configuration for the state manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfig {
    /// Maximum number of states to keep in memory
    pub max_memory_states: usize,
    /// Whether to automatically clean up finished states
    pub auto_cleanup: bool,
    /// Maximum age of finished states before cleanup (in seconds)
    pub cleanup_age_seconds: u64,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            max_memory_states: 100,
            auto_cleanup: true,
            cleanup_age_seconds: 24 * 60 * 60, // 24 hours
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookStatus;
    use tempfile::TempDir;
    use std::env;

    async fn create_test_state_manager() -> (StateManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let _state_dir = temp_dir.path().join(".cuenv").join("state");
        
        // Override home directory for testing
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        
        let manager = StateManager::new().unwrap();
        (manager, temp_dir)
    }

    #[tokio::test]
    async fn test_state_manager_new() {
        let (_manager, _temp) = create_test_state_manager().await;
        // If we get here, StateManager::new() succeeded
    }

    #[tokio::test]
    async fn test_directory_key() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path();
        
        let key1 = StateManager::directory_key(test_path).unwrap();
        let key2 = StateManager::directory_key(test_path).unwrap();
        
        // Same path should generate same key
        assert_eq!(key1, key2);
        assert!(!key1.is_empty());
        // Key should be a hex string
        assert!(key1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test] 
    async fn test_save_and_load_state() {
        let (manager, _temp) = create_test_state_manager().await;
        
        let temp_dir = TempDir::new().unwrap();
        let directory = temp_dir.path().to_path_buf();
        let state = HookExecutionState::new(directory.clone(), "test-hash".to_string(), 2);
        
        // Save state
        manager.save_state(&state).await.unwrap();
        
        // Load state
        let loaded_state = manager.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_some());
        
        let loaded_state = loaded_state.unwrap();
        assert_eq!(loaded_state.directory, state.directory);
        assert_eq!(loaded_state.config_hash, state.config_hash);
        assert_eq!(loaded_state.total_hooks, state.total_hooks);
        assert_eq!(loaded_state.session_id, state.session_id);
    }

    #[tokio::test]
    async fn test_load_nonexistent_state() {
        let (manager, _temp) = create_test_state_manager().await;
        
        let temp_dir = TempDir::new().unwrap(); 
        let directory = temp_dir.path().to_path_buf();
        
        let loaded_state = manager.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_none());
    }

    #[tokio::test]
    async fn test_remove_state() {
        let (manager, _temp) = create_test_state_manager().await;
        
        let temp_dir = TempDir::new().unwrap();
        let directory = temp_dir.path().to_path_buf();
        let state = HookExecutionState::new(directory.clone(), "test-hash".to_string(), 1);
        
        // Save state
        manager.save_state(&state).await.unwrap();
        
        // Verify it exists
        let loaded_state = manager.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_some());
        
        // Remove state
        manager.remove_state(&directory).await.unwrap();
        
        // Verify it's gone
        let loaded_state = manager.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_none());
    }

    #[tokio::test]
    async fn test_list_active_states() {
        let (manager, _temp) = create_test_state_manager().await;
        
        // Initially should be empty
        let states = manager.list_active_states().await.unwrap();
        assert!(states.is_empty());
        
        // Add some states
        let temp_dir1 = TempDir::new().unwrap();
        let directory1 = temp_dir1.path().to_path_buf();
        let state1 = HookExecutionState::new(directory1.clone(), "hash1".to_string(), 1);
        
        let temp_dir2 = TempDir::new().unwrap();
        let directory2 = temp_dir2.path().to_path_buf();
        let mut state2 = HookExecutionState::new(directory2.clone(), "hash2".to_string(), 1);
        state2.status = HookStatus::Completed; // This one is finished
        
        manager.save_state(&state1).await.unwrap();
        manager.save_state(&state2).await.unwrap();
        
        // Should only return non-finished states
        let states = manager.list_active_states().await.unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].config_hash, "hash1");
    }

    #[tokio::test]
    async fn test_cleanup_finished_states() {
        let (manager, _temp) = create_test_state_manager().await;
        
        let temp_dir = TempDir::new().unwrap();
        let directory = temp_dir.path().to_path_buf();
        let mut state = HookExecutionState::new(directory.clone(), "test-hash".to_string(), 1);
        state.status = HookStatus::Completed;
        
        // Save finished state
        manager.save_state(&state).await.unwrap();
        
        // Verify it exists
        let loaded_state = manager.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_some());
        
        // Clean up
        manager.cleanup_finished_states().await.unwrap();
        
        // Should be gone after cleanup
        // (Note: cleanup removes from disk, but load_state checks memory cache first)
        // So we need to create a new manager to bypass the cache
        let manager2 = StateManager::new().unwrap();
        let loaded_state = manager2.load_state(&directory).await.unwrap();
        assert!(loaded_state.is_none());
    }

    #[tokio::test]
    async fn test_state_config_default() {
        let config = StateConfig::default();
        assert_eq!(config.max_memory_states, 100);
        assert!(config.auto_cleanup);
        assert_eq!(config.cleanup_age_seconds, 24 * 60 * 60);
    }

    #[test]
    fn test_state_config_serialization() {
        let config = StateConfig::default();
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: StateConfig = serde_json::from_str(&serialized).unwrap();
        
        assert_eq!(config.max_memory_states, deserialized.max_memory_states);
        assert_eq!(config.auto_cleanup, deserialized.auto_cleanup);
        assert_eq!(config.cleanup_age_seconds, deserialized.cleanup_age_seconds);
    }
}