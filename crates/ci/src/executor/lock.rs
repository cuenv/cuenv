//! Concurrency Control
//!
//! Provides distributed locking for task concurrency groups to ensure
//! serialized execution of deployment tasks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Default lock timeout (5 minutes)
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(300);

/// Default stale lock threshold (10 minutes)
pub const STALE_LOCK_THRESHOLD: Duration = Duration::from_secs(600);

/// Lock acquisition poll interval
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Errors for lock operations
#[derive(Debug, Error)]
pub enum LockError {
    /// Lock acquisition timed out
    #[error("Lock acquisition timed out for group '{group}' after {timeout_secs}s")]
    Timeout { group: String, timeout_secs: u64 },

    /// Lock file IO error
    #[error("Lock file error for group '{group}': {source}")]
    Io {
        group: String,
        #[source]
        source: io::Error,
    },

    /// Lock directory creation failed
    #[error("Failed to create lock directory: {0}")]
    DirectoryCreation(io::Error),

    /// Lock is held by another process
    #[error("Lock held by process {pid} (acquired {age_secs}s ago)")]
    HeldByOther { pid: u32, age_secs: u64 },
}

/// Lock metadata stored in lock file
#[derive(Debug, Clone)]
pub struct LockMetadata {
    /// Process ID that holds the lock
    pub pid: u32,
    /// Timestamp when lock was acquired
    pub acquired_at: u64,
    /// Task ID that holds the lock
    pub task_id: String,
}

impl LockMetadata {
    fn serialize(&self) -> String {
        format!("{}:{}:{}", self.pid, self.acquired_at, self.task_id)
    }

    fn deserialize(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Self {
            pid: parts[0].parse().ok()?,
            acquired_at: parts[1].parse().ok()?,
            task_id: parts[2].to_string(),
        })
    }
}

/// Configuration for concurrency lock
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// Directory to store lock files
    pub lock_dir: PathBuf,
    /// Maximum time to wait for lock acquisition
    pub timeout: Duration,
    /// Threshold for considering a lock stale
    pub stale_threshold: Duration,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            lock_dir: PathBuf::from(".cuenv/locks"),
            timeout: DEFAULT_LOCK_TIMEOUT,
            stale_threshold: STALE_LOCK_THRESHOLD,
        }
    }
}

/// Concurrency lock manager
///
/// Provides file-based locking for concurrency groups. Locks are automatically
/// released when the guard is dropped.
///
/// # Stale Lock Detection
///
/// Locks are considered stale if their age (based on the `acquired_at` timestamp
/// stored in the lock file) exceeds the configured `stale_threshold`. This approach
/// has a limitation: if the lock holder process crashes without releasing the lock,
/// the staleness is determined by the original acquisition time, not by process
/// liveness. This means a lock may be held longer than expected if the holder
/// process hangs but doesn't crash.
///
/// For truly distributed scenarios, consider integrating with a proper distributed
/// lock service (e.g., etcd, Redis, or cloud-native locking APIs).
#[derive(Debug)]
pub struct ConcurrencyLock {
    config: LockConfig,
}

impl ConcurrencyLock {
    /// Create a new lock manager with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: LockConfig::default(),
        }
    }

    /// Create a lock manager with custom configuration
    #[must_use]
    pub const fn with_config(config: LockConfig) -> Self {
        Self { config }
    }

    /// Set the lock directory
    #[must_use]
    pub fn with_lock_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.lock_dir = dir.into();
        self
    }

    /// Set the lock acquisition timeout
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = timeout;
        self
    }

    /// Acquire a lock for the given concurrency group
    ///
    /// Blocks until the lock is acquired or timeout is reached.
    ///
    /// # Arguments
    /// * `group` - Concurrency group name
    /// * `task_id` - Task ID acquiring the lock (for diagnostics)
    ///
    /// # Errors
    /// Returns error if lock cannot be acquired within timeout
    pub async fn acquire(&self, group: &str, task_id: &str) -> Result<LockGuard, LockError> {
        // Ensure lock directory exists
        fs::create_dir_all(&self.config.lock_dir).map_err(LockError::DirectoryCreation)?;

        let lock_path = self.lock_path(group);
        let start = Instant::now();
        let pid = std::process::id();
        let metadata = LockMetadata {
            pid,
            acquired_at: current_timestamp(),
            task_id: task_id.to_string(),
        };

        loop {
            // Try to acquire the lock
            match Self::try_acquire(&lock_path, &metadata) {
                Ok(()) => {
                    tracing::info!(
                        group = %group,
                        task = %task_id,
                        "Acquired concurrency lock"
                    );
                    return Ok(LockGuard {
                        lock_path,
                        group: group.to_string(),
                    });
                }
                Err(LockError::HeldByOther { pid, age_secs }) => {
                    // Check if lock is stale
                    if Duration::from_secs(age_secs) > self.config.stale_threshold {
                        tracing::warn!(
                            group = %group,
                            holder_pid = pid,
                            age_secs = age_secs,
                            "Breaking stale lock"
                        );
                        // Remove stale lock and retry
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }

                    // Check timeout
                    if start.elapsed() >= self.config.timeout {
                        return Err(LockError::Timeout {
                            group: group.to_string(),
                            timeout_secs: self.config.timeout.as_secs(),
                        });
                    }

                    tracing::debug!(
                        group = %group,
                        holder_pid = pid,
                        "Lock held by another process, waiting..."
                    );
                }
                Err(e) => return Err(e),
            }

            // Wait before retrying
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Try to acquire lock without blocking
    ///
    /// # Errors
    ///
    /// Returns `LockError` if lock cannot be acquired or lock directory cannot be created.
    pub fn try_acquire_sync(&self, group: &str, task_id: &str) -> Result<LockGuard, LockError> {
        fs::create_dir_all(&self.config.lock_dir).map_err(LockError::DirectoryCreation)?;

        let lock_path = self.lock_path(group);
        let metadata = LockMetadata {
            pid: std::process::id(),
            acquired_at: current_timestamp(),
            task_id: task_id.to_string(),
        };

        Self::try_acquire(&lock_path, &metadata)?;

        Ok(LockGuard {
            lock_path,
            group: group.to_string(),
        })
    }

    /// Check if a lock is currently held
    #[must_use]
    pub fn is_locked(&self, group: &str) -> bool {
        let lock_path = self.lock_path(group);
        lock_path.exists()
    }

    /// Get information about current lock holder
    #[must_use]
    pub fn lock_info(&self, group: &str) -> Option<LockMetadata> {
        let lock_path = self.lock_path(group);
        read_lock_metadata(&lock_path)
    }

    fn lock_path(&self, group: &str) -> PathBuf {
        // Sanitize group name for filesystem
        let safe_name: String = group
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.config.lock_dir.join(format!("{safe_name}.lock"))
    }

    fn try_acquire(lock_path: &Path, metadata: &LockMetadata) -> Result<(), LockError> {
        // Try to create lock file exclusively
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                // Write metadata
                file.write_all(metadata.serialize().as_bytes())
                    .map_err(|e| LockError::Io {
                        group: metadata.task_id.clone(),
                        source: e,
                    })?;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Lock exists, check who holds it
                if let Some(existing) = read_lock_metadata(lock_path) {
                    let now = current_timestamp();
                    let age_secs = now.saturating_sub(existing.acquired_at);
                    Err(LockError::HeldByOther {
                        pid: existing.pid,
                        age_secs,
                    })
                } else {
                    // Can't read lock file, assume it's invalid and remove
                    let _ = fs::remove_file(lock_path);
                    Err(LockError::HeldByOther {
                        pid: 0,
                        age_secs: 0,
                    })
                }
            }
            Err(e) => Err(LockError::Io {
                group: metadata.task_id.clone(),
                source: e,
            }),
        }
    }
}

impl Default for ConcurrencyLock {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that releases lock when dropped
#[derive(Debug)]
pub struct LockGuard {
    lock_path: PathBuf,
    group: String,
}

impl LockGuard {
    /// Get the concurrency group name
    #[must_use]
    pub fn group(&self) -> &str {
        &self.group
    }

    /// Explicitly release the lock
    pub fn release(self) {
        // Drop will handle it
        drop(self);
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.lock_path) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(
                    group = %self.group,
                    error = %e,
                    "Failed to release lock"
                );
            }
        } else {
            tracing::debug!(group = %self.group, "Released concurrency lock");
        }
    }
}

fn read_lock_metadata(path: &Path) -> Option<LockMetadata> {
    let mut file = File::open(path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    LockMetadata::deserialize(&contents)
}

fn current_timestamp() -> u64 {
    // System time before UNIX epoch is practically impossible on modern systems,
    // but we handle it gracefully by returning 0 in that case
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lock_metadata_serialization() {
        let metadata = LockMetadata {
            pid: 12345,
            acquired_at: 1_234_567_890,
            task_id: "test-task".to_string(),
        };

        let serialized = metadata.serialize();
        let deserialized = LockMetadata::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.pid, 12345);
        assert_eq!(deserialized.acquired_at, 1_234_567_890);
        assert_eq!(deserialized.task_id, "test-task");
    }

    #[test]
    fn test_lock_acquisition_sync() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new().with_lock_dir(tmp.path());

        // First acquisition should succeed
        let guard1 = lock.try_acquire_sync("test-group", "task1").unwrap();
        assert!(lock.is_locked("test-group"));

        // Second acquisition should fail
        let result = lock.try_acquire_sync("test-group", "task2");
        assert!(matches!(result, Err(LockError::HeldByOther { .. })));

        // Release first lock
        drop(guard1);
        assert!(!lock.is_locked("test-group"));

        // Now we can acquire again
        let _guard2 = lock.try_acquire_sync("test-group", "task2").unwrap();
        assert!(lock.is_locked("test-group"));
    }

    #[test]
    fn test_different_groups() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new().with_lock_dir(tmp.path());

        let _guard1 = lock.try_acquire_sync("group-a", "task1").unwrap();
        let _guard2 = lock.try_acquire_sync("group-b", "task2").unwrap();

        assert!(lock.is_locked("group-a"));
        assert!(lock.is_locked("group-b"));
    }

    #[test]
    fn test_lock_info() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new().with_lock_dir(tmp.path());

        let _guard = lock.try_acquire_sync("test-group", "my-task").unwrap();

        let info = lock.lock_info("test-group").unwrap();
        assert_eq!(info.task_id, "my-task");
        assert_eq!(info.pid, std::process::id());
    }

    #[test]
    fn test_group_name_sanitization() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new().with_lock_dir(tmp.path());

        // Group with special characters
        let _guard = lock
            .try_acquire_sync("production/deploy:v1", "task1")
            .unwrap();

        // Lock file should exist with sanitized name
        let lock_path = tmp.path().join("production_deploy_v1.lock");
        assert!(lock_path.exists());
    }

    #[tokio::test]
    async fn test_async_acquisition() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new()
            .with_lock_dir(tmp.path())
            .with_timeout(Duration::from_secs(1));

        let guard = lock.acquire("async-group", "task1").await.unwrap();
        assert!(lock.is_locked("async-group"));
        drop(guard);
    }

    #[tokio::test]
    async fn test_timeout() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new()
            .with_lock_dir(tmp.path())
            .with_timeout(Duration::from_millis(100));

        // Hold the lock
        let _guard = lock.try_acquire_sync("timeout-group", "holder").unwrap();

        // Try to acquire with short timeout
        let result = lock.acquire("timeout-group", "waiter").await;
        assert!(matches!(result, Err(LockError::Timeout { .. })));
    }

    #[test]
    fn test_lock_release_on_drop() {
        let tmp = TempDir::new().unwrap();
        let lock = ConcurrencyLock::new().with_lock_dir(tmp.path());

        {
            let _guard = lock.try_acquire_sync("drop-test", "task1").unwrap();
            assert!(lock.is_locked("drop-test"));
        }

        // Lock should be released after guard is dropped
        assert!(!lock.is_locked("drop-test"));
    }
}
