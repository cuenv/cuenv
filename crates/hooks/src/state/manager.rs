use super::HookExecutionState;
use crate::{Error, Result};
use fs4::tokio::AsyncFileExt;
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

/// Manages persistent state for hook execution sessions
#[derive(Debug, Clone)]
pub struct StateManager {
    pub(super) state_dir: PathBuf,
}

impl StateManager {
    /// Create a new state manager with the specified state directory
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the default state directory.
    ///
    /// Uses platform-appropriate paths:
    /// - Linux: `~/.local/state/cuenv/hooks`
    /// - macOS: `~/Library/Application Support/cuenv/hooks`
    /// - Windows: `%APPDATA%\cuenv\hooks`
    pub fn default_state_dir() -> Result<PathBuf> {
        let base = dirs::state_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| Error::configuration("Could not determine state directory"))?;
        Ok(base.join("cuenv").join("hooks"))
    }

    /// Create a state manager using the default state directory
    pub fn with_default_dir() -> Result<Self> {
        Ok(Self::new(Self::default_state_dir()?))
    }

    /// Get the state directory path
    #[must_use]
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
    pub(super) fn state_file_path(&self, instance_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", instance_hash))
    }

    /// Get the state file path for a given directory hash (public for PID files)
    #[must_use]
    pub fn get_state_file_path(&self, instance_hash: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", instance_hash))
    }

    /// Save execution state to disk with atomic write and locking
    pub async fn save_state(&self, state: &HookExecutionState) -> Result<()> {
        self.ensure_state_dir().await?;

        let state_file = self.state_file_path(&state.instance_hash);
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| Error::serialization(format!("Failed to serialize state: {e}")))?;

        let temp_path = state_file.with_extension("tmp");

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

        drop(file);

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

        let mut file = match OpenOptions::new().read(true).open(&state_file).await {
            Ok(f) => f,
            Err(e) => {
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

        drop(file);

        let state: HookExecutionState = serde_json::from_str(&contents)
            .map_err(|e| Error::serialization(format!("Failed to deserialize state: {e}")))?;

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

    /// Load execution state from disk synchronously with shared locking.
    /// This is the sync equivalent of `load_state` for the fast path.
    pub fn load_state_sync(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        let state_file = self.state_file_path(instance_hash);

        if !state_file.exists() {
            return Ok(None);
        }

        let mut file = match std::fs::File::open(&state_file) {
            Ok(f) => f,
            Err(e) => {
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

        file.lock_shared().map_err(|e| {
            Error::configuration(format!(
                "Failed to acquire shared lock on state file: {}",
                e
            ))
        })?;

        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|e| Error::Io {
            source: e,
            path: Some(state_file.clone().into_boxed_path()),
            operation: "read_to_string".to_string(),
        })?;

        drop(file);

        let state: HookExecutionState = serde_json::from_str(&contents)
            .map_err(|e| Error::serialization(format!("Failed to deserialize state: {e}")))?;

        Ok(Some(state))
    }
}
