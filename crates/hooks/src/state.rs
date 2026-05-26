//! State management for hook execution tracking

use crate::types::{ExecutionStatus, Hook, HookResult};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
#[allow(unused_imports)] // Used by load_state_sync for file locking
use fs4::fs_std::FileExt as SyncFileExt;
use fs4::tokio::AsyncFileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
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
    fn state_file_path(&self, instance_hash: &str) -> PathBuf {
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

    // ========================================================================
    // Directory Marker System - Fast status lookups without config hash
    // ========================================================================

    /// Compute a key for directory-only lookups (used for fast status checks).
    /// This hashes just the canonicalized directory path, without config hash.
    #[must_use]
    pub fn compute_directory_key(path: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        hasher.update(canonical.to_string_lossy().as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    /// Get the path for a directory marker file
    fn directory_marker_path(&self, directory_key: &str) -> PathBuf {
        self.state_dir.join(format!("{}.marker", directory_key))
    }

    /// Create a marker file linking directory to instance hash.
    /// Called when hooks start to enable fast status lookups.
    pub async fn create_directory_marker(
        &self,
        directory_path: &Path,
        instance_hash: &str,
    ) -> Result<()> {
        self.ensure_state_dir().await?;
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);

        fs::write(&marker_path, instance_hash)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(marker_path.into_boxed_path()),
                operation: "write_marker".to_string(),
            })?;

        debug!(
            "Created directory marker for {} -> {}",
            directory_path.display(),
            instance_hash
        );
        Ok(())
    }

    /// Remove marker file for a directory.
    /// Called when hooks complete/fail and display timeout expires.
    pub async fn remove_directory_marker(&self, directory_path: &Path) -> Result<()> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);

        if marker_path.exists() {
            fs::remove_file(&marker_path).await.ok(); // Ignore errors
            debug!("Removed directory marker for {}", directory_path.display());
        }
        Ok(())
    }

    /// Fast synchronous check: does a marker exist for this directory?
    /// This is the hot path for Starship - just a single stat() syscall.
    #[must_use]
    pub fn has_active_marker(&self, directory_path: &Path) -> bool {
        let dir_key = Self::compute_directory_key(directory_path);
        self.directory_marker_path(&dir_key).exists()
    }

    /// Read the instance hash from a marker file (if it exists).
    pub async fn get_marker_instance_hash(&self, directory_path: &Path) -> Option<String> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);
        fs::read_to_string(&marker_path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
    }

    // ========================================================================
    // Synchronous Methods (for fast path - no tokio runtime required)
    // ========================================================================

    /// Read the instance hash from a marker file synchronously.
    /// This is the sync equivalent of `get_marker_instance_hash` for the fast path.
    #[must_use]
    pub fn get_marker_instance_hash_sync(&self, directory_path: &Path) -> Option<String> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);
        std::fs::read_to_string(&marker_path)
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Load execution state from disk synchronously with shared locking.
    /// This is the sync equivalent of `load_state` for the fast path.
    pub fn load_state_sync(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        let state_file = self.state_file_path(instance_hash);

        if !state_file.exists() {
            return Ok(None);
        }

        // Open file with shared lock for reading
        let mut file = match std::fs::File::open(&state_file) {
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
        file.read_to_string(&mut contents).map_err(|e| Error::Io {
            source: e,
            path: Some(state_file.clone().into_boxed_path()),
            operation: "read_to_string".to_string(),
        })?;

        // Unlock happens automatically when file is dropped
        drop(file);

        let state: HookExecutionState = serde_json::from_str(&contents)
            .map_err(|e| Error::serialization(format!("Failed to deserialize state: {e}")))?;

        Ok(Some(state))
    }

    // ========================================================================
    // Cleanup Methods
    // ========================================================================

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

            let extension = path.extension().and_then(|s| s.to_str());

            // Clean up JSON state files
            if extension == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    match self.load_state(stem).await {
                        Ok(Some(state)) if state.is_complete() => {
                            // Remove completed states and their markers
                            if let Err(e) = fs::remove_file(&path).await {
                                warn!("Failed to remove state file {}: {}", path.display(), e);
                            } else {
                                cleaned_count += 1;
                                debug!("Cleaned up state file: {}", path.display());
                                // Also remove the directory marker
                                self.remove_directory_marker(&state.directory_path)
                                    .await
                                    .ok();
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
            // Clean up orphaned marker files (markers without corresponding state)
            else if extension == Some("marker")
                && let Ok(instance_hash) = fs::read_to_string(&path).await
            {
                let instance_hash = instance_hash.trim();
                // Check if corresponding state exists
                match self.load_state(instance_hash).await {
                    Ok(None) => {
                        // State doesn't exist, remove orphaned marker
                        if fs::remove_file(&path).await.is_ok() {
                            cleaned_count += 1;
                            debug!("Cleaned up orphaned marker: {}", path.display());
                        }
                    }
                    Ok(Some(state)) if state.is_complete() && !state.should_display_completed() => {
                        // State is complete and expired, remove marker
                        if fs::remove_file(&path).await.is_ok() {
                            cleaned_count += 1;
                            debug!("Cleaned up expired marker: {}", path.display());
                        }
                    }
                    _ => {} // Keep marker
                }
            }
        }

        if cleaned_count > 0 {
            info!(
                "Cleaned up {} state/marker files from directory",
                cleaned_count
            );
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
                // Also remove the directory marker
                self.remove_directory_marker(&state.directory_path)
                    .await
                    .ok();
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
    pub hooks: Vec<Hook>,
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
    #[must_use]
    pub fn new(
        directory_path: PathBuf,
        instance_hash: String,
        config_hash: String,
        hooks: Vec<Hook>,
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Takes ownership for API clarity, cloning is intentional"
    )]
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
            self.error_message.clone_from(&result.error);
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
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        )
    }

    /// Get a human-readable progress display
    #[must_use]
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
    #[must_use]
    pub fn current_hook_duration(&self) -> Option<chrono::Duration> {
        self.current_hook_started_at
            .map(|started| Utc::now() - started)
    }

    /// Get the currently executing hook
    #[must_use]
    pub fn current_hook(&self) -> Option<&Hook> {
        self.current_hook_index.and_then(|idx| self.hooks.get(idx))
    }

    /// Format duration in human-readable format (e.g., "2.3s", "1m 15s", "2h 5m")
    #[must_use]
    pub fn format_duration(duration: chrono::Duration) -> String {
        let total_secs = duration.num_seconds();

        if total_secs < 60 {
            // Less than 1 minute: show as decimal seconds
            let millis = duration.num_milliseconds();
            // Precision loss is acceptable for display purposes
            #[expect(
                clippy::cast_precision_loss,
                reason = "Display formatting, precision loss is acceptable"
            )]
            let secs = millis as f64 / 1000.0;
            format!("{secs:.1}s")
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
    #[must_use]
    pub fn current_hook_display(&self) -> Option<String> {
        // If there's a current hook index, use that
        let hook = if let Some(hook) = self.current_hook() {
            Some(hook)
        } else if self.status == ExecutionStatus::Running && self.completed_hooks < self.total_hooks
        {
            // If we're running but no current hook index yet, show the next hook to execute
            self.hooks.get(self.completed_hooks)
        } else {
            None
        };

        hook.map(|h| {
            // Extract just the command name (first part before any path separators)
            let cmd_name = h.command.split('/').next_back().unwrap_or(&h.command);

            // Format: just the command name (no args, to keep it concise)
            format!("`{}`", cmd_name)
        })
    }

    /// Check if the completed state should still be displayed
    #[must_use]
    pub fn should_display_completed(&self) -> bool {
        if let Some(display_until) = self.completed_display_until {
            Utc::now() < display_until
        } else {
            false
        }
    }
}

/// Compute a hash for a unique execution instance (directory + config)
#[must_use]
pub fn compute_instance_hash(path: &Path, config_hash: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(config_hash.as_bytes());
    // Include cuenv version in hash to invalidate cache on upgrades
    // This is important when internal logic (like environment capturing) changes
    hasher.update(b":");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Compute a hash for hook execution that includes input file contents.
///
/// This is separate from the approval hash - approval only cares about the hook
/// definition, but execution cache needs to invalidate when input files change.
pub fn compute_execution_hash(hooks: &[Hook], base_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();

    // Hash the hook definitions
    if let Ok(hooks_json) = serde_json::to_string(hooks) {
        hasher.update(hooks_json.as_bytes());
    }

    // Hash the contents of input files from each hook
    for hook in hooks {
        // Determine the working directory for this hook
        let hook_dir = hook
            .dir
            .as_ref()
            .map_or_else(|| base_dir.to_path_buf(), PathBuf::from);

        for input in &hook.inputs {
            let input_path = hook_dir.join(input);
            if let Ok(content) = std::fs::read(&input_path) {
                hasher.update(b"file:");
                hasher.update(input.as_bytes());
                hasher.update(b":");
                hasher.update(&content);
            }
        }
    }

    // Include cuenv version
    hasher.update(b":version:");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());

    format!("{:x}", hasher.finalize())[..16].to_string()
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
