//! CI Cache Operations
//!
//! Handles cache lookups and storage for CI task execution based on
//! content-addressable digests and cache policies.
//!
//! This module provides:
//! - `LocalCacheBackend`: File-based cache for local development
//! - Legacy helper functions for backward compatibility

use crate::executor::backend::{
    BackendError, BackendResult, CacheBackend, CacheEntry, CacheLookupResult, CacheOutput,
    policy_allows_read, policy_allows_write,
};
use crate::ir::{CachePolicy, Task as IRTask};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Error types for cache operations (legacy, for backward compatibility)
#[derive(Debug, Error)]
pub enum CacheError {
    /// IO error during cache operations (generic)
    #[error("Cache IO error: {0}")]
    Io(#[from] io::Error),

    /// IO error with path context for better diagnostics
    #[error("Failed to {operation} '{path}': {source}")]
    IoWithContext {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    /// JSON serialization error
    #[error("Cache serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Backend error
    #[error("Backend error: {0}")]
    Backend(#[from] BackendError),
}

impl CacheError {
    /// Create an IO error with path context
    pub fn io_with_context(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::IoWithContext {
            operation,
            path: path.into(),
            source,
        }
    }
}

/// Result of a cache lookup
#[derive(Debug, Clone)]
pub struct CacheResult {
    /// Whether the cache entry was found
    pub hit: bool,
    /// The digest used for lookup
    pub key: String,
    /// Path to cache entry (if hit)
    pub path: Option<PathBuf>,
}

/// Metadata stored with cached task results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    /// Task ID
    pub task_id: String,
    /// Digest (cache key)
    pub digest: String,
    /// Command that was executed
    pub command: Vec<String>,
    /// When the cache entry was created
    pub created_at: DateTime<Utc>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Exit code
    pub exit_code: i32,
}

/// Task execution logs
#[derive(Debug, Clone, Default)]
pub struct TaskLogs {
    /// Standard output
    pub stdout: Option<String>,
    /// Standard error
    pub stderr: Option<String>,
}

/// Compute the cache path for a given digest
///
/// Uses a two-level directory structure to avoid too many entries in a
/// single directory: `{root}/{digest[0:2]}/{digest[2:4]}/{digest}/`
fn cache_path_for_digest(cache_root: &Path, digest: &str) -> PathBuf {
    // Strip the "sha256:" prefix if present
    let hash = digest.strip_prefix("sha256:").unwrap_or(digest);

    if hash.len() < 4 {
        // Fallback for very short hashes (shouldn't happen)
        return cache_root.join(hash);
    }

    cache_root.join(&hash[..2]).join(&hash[2..4]).join(hash)
}

/// Check cache before execution based on policy
///
/// # Arguments
/// * `task` - The IR task definition
/// * `digest` - Pre-computed digest for this task
/// * `cache_root` - Root directory for cache storage
/// * `policy_override` - Optional global policy override
///
/// # Returns
/// `CacheResult` indicating whether a cache hit was found
pub fn check_cache(
    task: &IRTask,
    digest: &str,
    cache_root: &Path,
    policy_override: Option<CachePolicy>,
) -> CacheResult {
    let effective_policy = policy_override.unwrap_or(task.cache_policy);

    match effective_policy {
        CachePolicy::Normal | CachePolicy::Readonly => {
            // Check if cache entry exists
            let cache_path = cache_path_for_digest(cache_root, digest);
            let metadata_path = cache_path.join("metadata.json");

            if metadata_path.exists() {
                tracing::debug!(
                    task = %task.id,
                    digest = %digest,
                    path = %cache_path.display(),
                    "Cache hit"
                );
                return CacheResult {
                    hit: true,
                    key: digest.to_string(),
                    path: Some(cache_path),
                };
            }

            tracing::debug!(
                task = %task.id,
                digest = %digest,
                "Cache miss"
            );
        }
        CachePolicy::Writeonly | CachePolicy::Disabled => {
            // Skip cache lookup
            tracing::debug!(
                task = %task.id,
                policy = ?effective_policy,
                "Cache lookup skipped due to policy"
            );
        }
    }

    CacheResult {
        hit: false,
        key: digest.to_string(),
        path: None,
    }
}

/// Store result after successful execution
///
/// # Arguments
/// * `task` - The IR task definition
/// * `digest` - Pre-computed digest for this task
/// * `cache_root` - Root directory for cache storage
/// * `logs` - Captured stdout/stderr
/// * `duration_ms` - Execution duration
/// * `exit_code` - Exit code
/// * `policy_override` - Optional global policy override
///
/// # Errors
/// Returns error if IO operations fail
pub fn store_result(
    task: &IRTask,
    digest: &str,
    cache_root: &Path,
    logs: &TaskLogs,
    duration_ms: u64,
    exit_code: i32,
    policy_override: Option<CachePolicy>,
) -> Result<(), CacheError> {
    let effective_policy = policy_override.unwrap_or(task.cache_policy);

    match effective_policy {
        CachePolicy::Normal | CachePolicy::Writeonly => {
            let cache_path = cache_path_for_digest(cache_root, digest);
            fs::create_dir_all(&cache_path)
                .map_err(|e| CacheError::io_with_context("create directory", &cache_path, e))?;

            // Write metadata
            let meta = CacheMetadata {
                task_id: task.id.clone(),
                digest: digest.to_string(),
                command: task.command.clone(),
                created_at: Utc::now(),
                duration_ms,
                exit_code,
            };
            let meta_path = cache_path.join("metadata.json");
            let meta_json = serde_json::to_string_pretty(&meta)?;
            fs::write(&meta_path, &meta_json)
                .map_err(|e| CacheError::io_with_context("write", &meta_path, e))?;

            // Write logs
            let logs_dir = cache_path.join("logs");
            fs::create_dir_all(&logs_dir)
                .map_err(|e| CacheError::io_with_context("create directory", &logs_dir, e))?;

            if let Some(stdout) = &logs.stdout {
                let stdout_path = logs_dir.join("stdout.log");
                fs::write(&stdout_path, stdout)
                    .map_err(|e| CacheError::io_with_context("write", &stdout_path, e))?;
            }
            if let Some(stderr) = &logs.stderr {
                let stderr_path = logs_dir.join("stderr.log");
                fs::write(&stderr_path, stderr)
                    .map_err(|e| CacheError::io_with_context("write", &stderr_path, e))?;
            }

            tracing::debug!(
                task = %task.id,
                digest = %digest,
                path = %cache_path.display(),
                "Cache entry stored"
            );
        }
        CachePolicy::Readonly | CachePolicy::Disabled => {
            // Skip cache write
            tracing::debug!(
                task = %task.id,
                policy = ?effective_policy,
                "Cache write skipped due to policy"
            );
        }
    }

    Ok(())
}

/// Load cached task metadata
///
/// # Errors
/// Returns error if the cache entry doesn't exist or can't be read
pub fn load_metadata(cache_path: &Path) -> Result<CacheMetadata, CacheError> {
    let meta_path = cache_path.join("metadata.json");
    let content = fs::read_to_string(&meta_path)
        .map_err(|e| CacheError::io_with_context("read", &meta_path, e))?;
    let meta: CacheMetadata = serde_json::from_str(&content)?;
    Ok(meta)
}

/// Load cached logs
#[must_use]
pub fn load_logs(cache_path: &Path) -> TaskLogs {
    let logs_dir = cache_path.join("logs");

    let stdout = fs::read_to_string(logs_dir.join("stdout.log")).ok();
    let stderr = fs::read_to_string(logs_dir.join("stderr.log")).ok();

    TaskLogs { stdout, stderr }
}

// ============================================================================
// LocalCacheBackend - File-based cache implementing CacheBackend trait
// ============================================================================

/// Local file-based cache backend
///
/// Stores cached task results on the local filesystem using a content-addressable
/// directory structure. Suitable for local development and single-machine CI.
#[derive(Debug, Clone)]
pub struct LocalCacheBackend {
    /// Root directory for cache storage
    cache_root: PathBuf,
}

impl LocalCacheBackend {
    /// Create a new local cache backend
    #[must_use]
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
        }
    }

    /// Get the cache path for a digest
    fn cache_path(&self, digest: &str) -> PathBuf {
        cache_path_for_digest(&self.cache_root, digest)
    }

    /// Store outputs directory path
    fn outputs_path(&self, digest: &str) -> PathBuf {
        self.cache_path(digest).join("outputs")
    }
}

#[async_trait]
impl CacheBackend for LocalCacheBackend {
    async fn check(
        &self,
        task: &IRTask,
        digest: &str,
        policy: CachePolicy,
    ) -> BackendResult<CacheLookupResult> {
        if !policy_allows_read(policy) {
            tracing::debug!(
                task = %task.id,
                policy = ?policy,
                "Cache lookup skipped due to policy"
            );
            return Ok(CacheLookupResult::miss(digest));
        }

        let cache_path = self.cache_path(digest);
        let metadata_path = cache_path.join("metadata.json");

        if metadata_path.exists() {
            // Load metadata to get duration
            match load_metadata(&cache_path) {
                Ok(meta) => {
                    tracing::debug!(
                        task = %task.id,
                        digest = %digest,
                        path = %cache_path.display(),
                        "Cache hit"
                    );
                    return Ok(CacheLookupResult::hit(digest, meta.duration_ms));
                }
                Err(e) => {
                    tracing::warn!(
                        task = %task.id,
                        error = %e,
                        "Failed to load cache metadata, treating as miss"
                    );
                }
            }
        }

        tracing::debug!(
            task = %task.id,
            digest = %digest,
            "Cache miss"
        );
        Ok(CacheLookupResult::miss(digest))
    }

    async fn store(
        &self,
        task: &IRTask,
        digest: &str,
        entry: &CacheEntry,
        policy: CachePolicy,
    ) -> BackendResult<()> {
        if !policy_allows_write(policy) {
            tracing::debug!(
                task = %task.id,
                policy = ?policy,
                "Cache write skipped due to policy"
            );
            return Ok(());
        }

        let cache_path = self.cache_path(digest);
        fs::create_dir_all(&cache_path)
            .map_err(|e| BackendError::io_with_context("create directory", &cache_path, e))?;

        // Write metadata
        let meta = CacheMetadata {
            task_id: task.id.clone(),
            digest: digest.to_string(),
            command: task.command.clone(),
            created_at: Utc::now(),
            duration_ms: entry.duration_ms,
            exit_code: entry.exit_code,
        };
        let meta_path = cache_path.join("metadata.json");
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| BackendError::Serialization(e.to_string()))?;
        fs::write(&meta_path, &meta_json)
            .map_err(|e| BackendError::io_with_context("write", &meta_path, e))?;

        // Write logs
        let logs_dir = cache_path.join("logs");
        fs::create_dir_all(&logs_dir)
            .map_err(|e| BackendError::io_with_context("create directory", &logs_dir, e))?;

        if let Some(stdout) = &entry.stdout {
            let stdout_path = logs_dir.join("stdout.log");
            fs::write(&stdout_path, stdout)
                .map_err(|e| BackendError::io_with_context("write", &stdout_path, e))?;
        }
        if let Some(stderr) = &entry.stderr {
            let stderr_path = logs_dir.join("stderr.log");
            fs::write(&stderr_path, stderr)
                .map_err(|e| BackendError::io_with_context("write", &stderr_path, e))?;
        }

        // Write outputs
        if !entry.outputs.is_empty() {
            let outputs_dir = self.outputs_path(digest);
            fs::create_dir_all(&outputs_dir)
                .map_err(|e| BackendError::io_with_context("create directory", &outputs_dir, e))?;

            for output in &entry.outputs {
                let output_path = outputs_dir.join(&output.path);
                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        BackendError::io_with_context("create directory", parent, e)
                    })?;
                }
                fs::write(&output_path, &output.data)
                    .map_err(|e| BackendError::io_with_context("write", &output_path, e))?;

                #[cfg(unix)]
                if output.is_executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&output_path)
                        .map_err(|e| {
                            BackendError::io_with_context("read metadata", &output_path, e)
                        })?
                        .permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    fs::set_permissions(&output_path, perms).map_err(|e| {
                        BackendError::io_with_context("set permissions", &output_path, e)
                    })?;
                }
            }
        }

        tracing::debug!(
            task = %task.id,
            digest = %digest,
            path = %cache_path.display(),
            outputs = entry.outputs.len(),
            "Cache entry stored"
        );

        Ok(())
    }

    async fn restore_outputs(
        &self,
        task: &IRTask,
        digest: &str,
        workspace: &Path,
    ) -> BackendResult<Vec<CacheOutput>> {
        let outputs_dir = self.outputs_path(digest);
        let mut restored = Vec::new();

        if !outputs_dir.exists() {
            return Ok(restored);
        }

        // Walk the outputs directory and restore files
        for entry in walkdir(&outputs_dir)
            .map_err(|e| BackendError::io_with_context("read directory", &outputs_dir, e))?
        {
            let rel_path = entry
                .strip_prefix(&outputs_dir)
                .map_err(|e| BackendError::Io(io::Error::other(e.to_string())))?;

            let dest_path = workspace.join(rel_path);
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| BackendError::io_with_context("create directory", parent, e))?;
            }

            let data =
                fs::read(&entry).map_err(|e| BackendError::io_with_context("read", &entry, e))?;
            let is_executable = is_file_executable(&entry);

            fs::write(&dest_path, &data)
                .map_err(|e| BackendError::io_with_context("write", &dest_path, e))?;

            #[cfg(unix)]
            if is_executable {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&dest_path)
                    .map_err(|e| BackendError::io_with_context("read metadata", &dest_path, e))?
                    .permissions();
                perms.set_mode(perms.mode() | 0o111);
                fs::set_permissions(&dest_path, perms)
                    .map_err(|e| BackendError::io_with_context("set permissions", &dest_path, e))?;
            }

            restored.push(CacheOutput {
                path: rel_path.to_string_lossy().to_string(),
                data,
                is_executable,
            });
        }

        tracing::debug!(
            task = %task.id,
            digest = %digest,
            restored = restored.len(),
            "Restored outputs from cache"
        );

        Ok(restored)
    }

    async fn get_logs(
        &self,
        _task: &IRTask,
        digest: &str,
    ) -> BackendResult<(Option<String>, Option<String>)> {
        let cache_path = self.cache_path(digest);
        let logs = load_logs(&cache_path);
        Ok((logs.stdout, logs.stderr))
    }

    fn name(&self) -> &'static str {
        "local"
    }

    async fn health_check(&self) -> BackendResult<()> {
        // Local cache is always available if we can create the directory
        fs::create_dir_all(&self.cache_root)
            .map_err(|e| BackendError::io_with_context("create directory", &self.cache_root, e))?;
        Ok(())
    }
}

/// Walk a directory recursively and return all file paths
fn walkdir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path)?);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Check if a file is executable
#[cfg(unix)]
fn is_file_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_file_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_task(id: &str, policy: CachePolicy) -> IRTask {
        IRTask {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string(), "hello".to_string()],
            shell: false,
            env: std::collections::HashMap::new(),
            secrets: std::collections::HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: policy,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_cache_path_structure() {
        let root = Path::new("/cache");
        let path = cache_path_for_digest(root, "sha256:abcdef123456");
        assert_eq!(path, PathBuf::from("/cache/ab/cd/abcdef123456"));
    }

    #[test]
    fn test_cache_miss() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Normal);

        let result = check_cache(&task, "sha256:nonexistent", tmp.path(), None);
        assert!(!result.hit);
        assert!(result.path.is_none());
    }

    #[test]
    fn test_cache_hit_after_store() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Normal);
        let digest = "sha256:testdigest123456";

        // Store result
        let logs = TaskLogs {
            stdout: Some("output".to_string()),
            stderr: None,
        };
        store_result(&task, digest, tmp.path(), &logs, 100, 0, None).unwrap();

        // Check cache - should hit
        let result = check_cache(&task, digest, tmp.path(), None);
        assert!(result.hit);
        assert!(result.path.is_some());

        // Load and verify metadata
        let meta = load_metadata(result.path.as_ref().unwrap()).unwrap();
        assert_eq!(meta.task_id, "test");
        assert_eq!(meta.exit_code, 0);
        assert_eq!(meta.duration_ms, 100);
    }

    #[test]
    fn test_readonly_policy_no_write() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Readonly);
        let digest = "sha256:readonly123";

        // Store with readonly policy - should skip
        let logs = TaskLogs::default();
        store_result(&task, digest, tmp.path(), &logs, 100, 0, None).unwrap();

        // Cache should not exist
        let result = check_cache(&task, digest, tmp.path(), None);
        assert!(!result.hit);
    }

    #[test]
    fn test_writeonly_policy_no_read() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Normal);
        let digest = "sha256:writeonly123";

        // Store with normal policy
        let logs = TaskLogs::default();
        store_result(&task, digest, tmp.path(), &logs, 100, 0, None).unwrap();

        // Check with writeonly override - should not read
        let result = check_cache(&task, digest, tmp.path(), Some(CachePolicy::Writeonly));
        assert!(!result.hit);
    }

    #[test]
    fn test_disabled_policy_no_cache() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Disabled);
        let digest = "sha256:disabled123";

        // Store should skip
        let logs = TaskLogs::default();
        store_result(&task, digest, tmp.path(), &logs, 100, 0, None).unwrap();

        // Check should also skip
        let result = check_cache(&task, digest, tmp.path(), None);
        assert!(!result.hit);
    }

    #[test]
    fn test_policy_override() {
        let tmp = TempDir::new().unwrap();
        let task = make_task("test", CachePolicy::Normal);
        let digest = "sha256:override123";

        // Store with disabled override - should skip
        let logs = TaskLogs::default();
        store_result(
            &task,
            digest,
            tmp.path(),
            &logs,
            100,
            0,
            Some(CachePolicy::Disabled),
        )
        .unwrap();

        // Cache should not exist
        let result = check_cache(&task, digest, tmp.path(), None);
        assert!(!result.hit);
    }
}
