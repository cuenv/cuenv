//! Content-addressed cache for OCI binaries.
//!
//! Binaries are stored by their SHA256 digest, ensuring:
//! - Hermetic builds (same digest = same binary)
//! - Deduplication across projects
//! - Fast cache hits without network requests

use std::path::{Path, PathBuf};
use tracing::{debug, trace};

use crate::Result;

/// Content-addressed cache for OCI binaries.
///
/// Default location: `~/.cache/cuenv/oci/`
///
/// Structure:
/// ```text
/// ~/.cache/cuenv/oci/
/// ├── blobs/
/// │   └── sha256/
/// │       └── abc123...  # Raw layer blobs
/// └── bin/
///     └── sha256/
///         └── def456...  # Extracted binaries
/// ```
#[derive(Debug, Clone)]
pub struct OciCache {
    root: PathBuf,
}

impl Default for OciCache {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("cuenv")
            .join("oci");
        Self::new(cache_dir)
    }
}

impl OciCache {
    /// Create a cache at the specified root directory.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Get the cache root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the path for a cached blob.
    #[must_use]
    pub fn blob_path(&self, digest: &str) -> PathBuf {
        let (algo, hash) = parse_digest(digest);
        self.root.join("blobs").join(algo).join(hash)
    }

    /// Get the path for a cached binary.
    #[must_use]
    pub fn binary_path(&self, digest: &str) -> PathBuf {
        let (algo, hash) = parse_digest(digest);
        self.root.join("bin").join(algo).join(hash)
    }

    /// Check if a blob is cached.
    #[must_use]
    pub fn has_blob(&self, digest: &str) -> bool {
        self.blob_path(digest).exists()
    }

    /// Check if a binary is cached.
    #[must_use]
    pub fn has_binary(&self, digest: &str) -> bool {
        self.binary_path(digest).exists()
    }

    /// Get a cached binary if it exists.
    #[must_use]
    pub fn get_binary(&self, digest: &str) -> Option<PathBuf> {
        let path = self.binary_path(digest);
        if path.exists() {
            trace!(digest, ?path, "Cache hit for binary");
            Some(path)
        } else {
            trace!(digest, "Cache miss for binary");
            None
        }
    }

    /// Store a blob in the cache.
    ///
    /// The blob is moved to the cache location.
    pub fn store_blob(&self, digest: &str, source: &Path) -> Result<PathBuf> {
        let dest = self.blob_path(digest);
        self.store_file(source, &dest)?;
        debug!(digest, ?dest, "Stored blob in cache");
        Ok(dest)
    }

    /// Store a binary in the cache.
    ///
    /// The binary is copied to the cache location.
    pub fn store_binary(&self, digest: &str, source: &Path) -> Result<PathBuf> {
        let dest = self.binary_path(digest);
        self.store_file(source, &dest)?;
        debug!(digest, ?dest, "Stored binary in cache");
        Ok(dest)
    }

    /// Store a file in the cache, creating parent directories.
    fn store_file(&self, source: &Path, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, dest)?;
        Ok(())
    }

    /// Ensure cache directories exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.root.join("blobs").join("sha256"))?;
        std::fs::create_dir_all(self.root.join("bin").join("sha256"))?;
        Ok(())
    }
}

/// Parse a digest string into (algorithm, hash).
///
/// Examples:
/// - "sha256:abc123" -> ("sha256", "abc123")
/// - "abc123" -> ("sha256", "abc123")
fn parse_digest(digest: &str) -> (&str, &str) {
    if let Some((algo, hash)) = digest.split_once(':') {
        (algo, hash)
    } else {
        ("sha256", digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cache_paths() {
        let cache = OciCache::new(PathBuf::from("/tmp/cache"));

        assert_eq!(
            cache.blob_path("sha256:abc123"),
            PathBuf::from("/tmp/cache/blobs/sha256/abc123")
        );
        assert_eq!(
            cache.binary_path("sha256:def456"),
            PathBuf::from("/tmp/cache/bin/sha256/def456")
        );
    }

    #[test]
    fn test_cache_store_and_get() -> Result<()> {
        let temp = TempDir::new()?;
        let cache = OciCache::new(temp.path().to_path_buf());
        cache.ensure_dirs()?;

        // Create a test file
        let test_file = temp.path().join("test_binary");
        std::fs::write(&test_file, b"test content")?;

        // Store as binary
        let digest = "sha256:abc123";
        let cached_path = cache.store_binary(digest, &test_file)?;

        // Verify cache hit
        assert!(cache.has_binary(digest));
        assert_eq!(cache.get_binary(digest), Some(cached_path));

        // Verify content
        let content = std::fs::read(cache.binary_path(digest))?;
        assert_eq!(content, b"test content");

        Ok(())
    }

    #[test]
    fn test_parse_digest() {
        assert_eq!(parse_digest("sha256:abc123"), ("sha256", "abc123"));
        assert_eq!(parse_digest("sha512:def456"), ("sha512", "def456"));
        assert_eq!(parse_digest("abc123"), ("sha256", "abc123"));
    }
}
