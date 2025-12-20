//! Cache Backend Abstraction
//!
//! Defines the `CacheBackend` trait for pluggable cache implementations.
//! Supports both local file-based caching and remote Bazel RE v2 caching.

use crate::ir::{CachePolicy, Task as IRTask};
use async_trait::async_trait;
use std::path::Path;
use thiserror::Error;

/// Error types for cache backend operations
#[derive(Debug, Error)]
pub enum BackendError {
    /// IO error during cache operations (generic, for #[from] compatibility)
    #[error("Cache IO error: {0}")]
    Io(#[from] std::io::Error),

    /// IO error with path context for better diagnostics
    #[error("Failed to {operation} '{path}': {source}")]
    IoWithContext {
        operation: &'static str,
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Remote connection error
    #[error("Remote cache connection error: {0}")]
    Connection(String),

    /// Remote cache unavailable (gracefully degradable)
    ///
    /// This error indicates the cache is temporarily unavailable but execution
    /// should continue without caching. Callers should handle this gracefully.
    #[error("Remote cache unavailable: {0}")]
    Unavailable(String),

    /// Digest mismatch during download
    #[error("Digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    /// Blob not found in CAS
    #[error("Blob not found: {digest}")]
    BlobNotFound { digest: String },

    /// Action result not found
    #[error("Action result not found for digest: {digest}")]
    ActionNotFound { digest: String },
}

impl BackendError {
    /// Returns true if this error indicates the cache is unavailable but
    /// execution should continue without caching (graceful degradation).
    #[must_use]
    pub fn is_gracefully_degradable(&self) -> bool {
        matches!(
            self,
            BackendError::Unavailable(_)
                | BackendError::Connection(_)
                | BackendError::ActionNotFound { .. }
        )
    }

    /// Create an IO error with path context
    pub fn io_with_context(
        operation: &'static str,
        path: impl Into<std::path::PathBuf>,
        source: std::io::Error,
    ) -> Self {
        BackendError::IoWithContext {
            operation,
            path: path.into(),
            source,
        }
    }
}

/// Result type for cache backend operations
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// Result of a cache lookup
#[derive(Debug, Clone)]
pub struct CacheLookupResult {
    /// Whether the cache entry was found
    pub hit: bool,
    /// The digest used for lookup
    pub key: String,
    /// Execution duration from cached result (if hit)
    pub cached_duration_ms: Option<u64>,
}

impl CacheLookupResult {
    /// Create a cache miss result
    #[must_use]
    pub fn miss(key: impl Into<String>) -> Self {
        Self {
            hit: false,
            key: key.into(),
            cached_duration_ms: None,
        }
    }

    /// Create a cache hit result
    #[must_use]
    pub fn hit(key: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            hit: true,
            key: key.into(),
            cached_duration_ms: Some(duration_ms),
        }
    }
}

/// Output artifact to store in cache
#[derive(Debug, Clone)]
pub struct CacheOutput {
    /// Relative path within workspace
    pub path: String,
    /// File contents
    pub data: Vec<u8>,
    /// Whether this is executable
    pub is_executable: bool,
}

/// Stored task execution result
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// Standard output
    pub stdout: Option<String>,
    /// Standard error
    pub stderr: Option<String>,
    /// Exit code
    pub exit_code: i32,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Output artifacts
    pub outputs: Vec<CacheOutput>,
}

/// Cache backend trait for pluggable cache implementations
///
/// Implementations must be thread-safe (`Send + Sync`) for concurrent task execution.
#[async_trait]
pub trait CacheBackend: Send + Sync {
    /// Check if a cached result exists for the given task and digest
    ///
    /// # Arguments
    /// * `task` - The IR task definition
    /// * `digest` - Pre-computed digest (cache key)
    /// * `policy` - Effective cache policy (may be overridden globally)
    ///
    /// # Returns
    /// `CacheLookupResult` indicating whether a cache hit was found
    async fn check(
        &self,
        task: &IRTask,
        digest: &str,
        policy: CachePolicy,
    ) -> BackendResult<CacheLookupResult>;

    /// Store a task execution result in the cache
    ///
    /// # Arguments
    /// * `task` - The IR task definition
    /// * `digest` - Pre-computed digest (cache key)
    /// * `entry` - The execution result to store
    /// * `policy` - Effective cache policy
    ///
    /// # Errors
    /// Returns error if storage fails (but callers should handle gracefully)
    async fn store(
        &self,
        task: &IRTask,
        digest: &str,
        entry: &CacheEntry,
        policy: CachePolicy,
    ) -> BackendResult<()>;

    /// Restore output artifacts from cache to the workspace
    ///
    /// # Arguments
    /// * `task` - The IR task definition
    /// * `digest` - Pre-computed digest (cache key)
    /// * `workspace` - Directory to restore outputs to
    ///
    /// # Errors
    /// Returns error if restoration fails
    async fn restore_outputs(
        &self,
        task: &IRTask,
        digest: &str,
        workspace: &Path,
    ) -> BackendResult<Vec<CacheOutput>>;

    /// Get cached stdout/stderr logs
    ///
    /// # Arguments
    /// * `task` - The IR task definition
    /// * `digest` - Pre-computed digest (cache key)
    ///
    /// # Returns
    /// Tuple of (stdout, stderr) if available
    async fn get_logs(
        &self,
        task: &IRTask,
        digest: &str,
    ) -> BackendResult<(Option<String>, Option<String>)>;

    /// Get the backend name for logging/metrics
    fn name(&self) -> &'static str;

    /// Check if the backend is available/connected
    async fn health_check(&self) -> BackendResult<()>;
}

/// Determine if cache read is allowed for a policy
#[must_use]
pub fn policy_allows_read(policy: CachePolicy) -> bool {
    matches!(policy, CachePolicy::Normal | CachePolicy::Readonly)
}

/// Determine if cache write is allowed for a policy
#[must_use]
pub fn policy_allows_write(policy: CachePolicy) -> bool {
    matches!(policy, CachePolicy::Normal | CachePolicy::Writeonly)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_lookup_result() {
        let miss = CacheLookupResult::miss("sha256:abc123");
        assert!(!miss.hit);
        assert_eq!(miss.key, "sha256:abc123");
        assert!(miss.cached_duration_ms.is_none());

        let hit = CacheLookupResult::hit("sha256:def456", 1234);
        assert!(hit.hit);
        assert_eq!(hit.key, "sha256:def456");
        assert_eq!(hit.cached_duration_ms, Some(1234));
    }

    #[test]
    fn test_policy_allows_read() {
        assert!(policy_allows_read(CachePolicy::Normal));
        assert!(policy_allows_read(CachePolicy::Readonly));
        assert!(!policy_allows_read(CachePolicy::Writeonly));
        assert!(!policy_allows_read(CachePolicy::Disabled));
    }

    #[test]
    fn test_policy_allows_write() {
        assert!(policy_allows_write(CachePolicy::Normal));
        assert!(!policy_allows_write(CachePolicy::Readonly));
        assert!(policy_allows_write(CachePolicy::Writeonly));
        assert!(!policy_allows_write(CachePolicy::Disabled));
    }

    #[test]
    fn test_is_gracefully_degradable() {
        // Transient failures should allow graceful degradation
        assert!(BackendError::Unavailable("test".to_string()).is_gracefully_degradable());
        assert!(BackendError::Connection("test".to_string()).is_gracefully_degradable());
        assert!(BackendError::ActionNotFound {
            digest: "test".to_string()
        }
        .is_gracefully_degradable());

        // Hard failures should not allow graceful degradation
        assert!(!BackendError::Io(std::io::Error::other("test")).is_gracefully_degradable());
        assert!(
            !BackendError::IoWithContext {
                operation: "write",
                path: std::path::PathBuf::from("/test"),
                source: std::io::Error::other("test"),
            }
            .is_gracefully_degradable()
        );
        assert!(!BackendError::Serialization("test".to_string()).is_gracefully_degradable());
        assert!(!BackendError::DigestMismatch {
            expected: "a".to_string(),
            actual: "b".to_string()
        }
        .is_gracefully_degradable());
        assert!(
            !BackendError::BlobNotFound {
                digest: "test".to_string()
            }
            .is_gracefully_degradable()
        );
    }

    #[test]
    fn test_io_with_context() {
        let error = BackendError::io_with_context(
            "write",
            "/test/path",
            std::io::Error::other("disk full"),
        );
        let msg = error.to_string();
        assert!(msg.contains("write"));
        assert!(msg.contains("/test/path"));
        assert!(msg.contains("disk full"));
    }
}
