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

    /// Get the directory for a cached binary (by digest).
    ///
    /// Binaries are stored as `bin/<algo>/<hash>/<binary_name>` so that
    /// the directory can be added to PATH and the binary called by name.
    #[must_use]
    pub fn binary_dir(&self, digest: &str) -> PathBuf {
        let (algo, hash) = parse_digest(digest);
        self.root.join("bin").join(algo).join(hash)
    }

    /// Get the full path for a cached binary with its name.
    #[must_use]
    pub fn binary_path(&self, digest: &str, binary_name: &str) -> PathBuf {
        self.binary_dir(digest).join(binary_name)
    }

    /// Check if a blob is cached.
    #[must_use]
    pub fn has_blob(&self, digest: &str) -> bool {
        self.blob_path(digest).exists()
    }

    /// Check if a binary is cached.
    #[must_use]
    pub fn has_binary(&self, digest: &str, binary_name: &str) -> bool {
        self.binary_path(digest, binary_name).exists()
    }

    /// Get a cached binary if it exists.
    #[must_use]
    pub fn get_binary(&self, digest: &str, binary_name: &str) -> Option<PathBuf> {
        let path = self.binary_path(digest, binary_name);
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
    /// The binary is copied to the cache location with its proper name.
    pub fn store_binary(&self, digest: &str, binary_name: &str, source: &Path) -> Result<PathBuf> {
        let dest = self.binary_path(digest, binary_name);
        self.store_file(source, &dest)?;
        debug!(digest, binary_name, ?dest, "Stored binary in cache");
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
            cache.binary_dir("sha256:def456"),
            PathBuf::from("/tmp/cache/bin/sha256/def456")
        );
        assert_eq!(
            cache.binary_path("sha256:def456", "jq"),
            PathBuf::from("/tmp/cache/bin/sha256/def456/jq")
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

        // Store as binary with name
        let digest = "sha256:abc123";
        let binary_name = "jq";
        let cached_path = cache.store_binary(digest, binary_name, &test_file)?;

        // Verify cache hit
        assert!(cache.has_binary(digest, binary_name));
        assert_eq!(cache.get_binary(digest, binary_name), Some(cached_path));

        // Verify content
        let content = std::fs::read(cache.binary_path(digest, binary_name))?;
        assert_eq!(content, b"test content");

        Ok(())
    }

    #[test]
    fn test_parse_digest() {
        assert_eq!(parse_digest("sha256:abc123"), ("sha256", "abc123"));
        assert_eq!(parse_digest("sha512:def456"), ("sha512", "def456"));
        assert_eq!(parse_digest("abc123"), ("sha256", "abc123"));
    }

    #[test]
    fn test_cache_default() {
        let cache = OciCache::default();
        // Default cache should be in user's cache directory
        let root = cache.root();
        assert!(root.to_string_lossy().contains("oci"));
    }

    #[test]
    fn test_cache_root() {
        let cache = OciCache::new(PathBuf::from("/custom/cache"));
        assert_eq!(cache.root(), Path::new("/custom/cache"));
    }

    #[test]
    fn test_cache_clone() {
        let cache = OciCache::new(PathBuf::from("/tmp/test"));
        let cloned = cache.clone();
        assert_eq!(cache.root(), cloned.root());
    }

    #[test]
    fn test_cache_debug() {
        let cache = OciCache::new(PathBuf::from("/tmp/test"));
        let debug = format!("{cache:?}");
        assert!(debug.contains("OciCache"));
        assert!(debug.contains("/tmp/test"));
    }

    #[test]
    fn test_has_blob_missing() {
        let temp = TempDir::new().unwrap();
        let cache = OciCache::new(temp.path().to_path_buf());
        assert!(!cache.has_blob("sha256:nonexistent"));
    }

    #[test]
    fn test_has_binary_missing() {
        let temp = TempDir::new().unwrap();
        let cache = OciCache::new(temp.path().to_path_buf());
        assert!(!cache.has_binary("sha256:nonexistent", "missing"));
    }

    #[test]
    fn test_get_binary_missing() {
        let temp = TempDir::new().unwrap();
        let cache = OciCache::new(temp.path().to_path_buf());
        assert!(cache.get_binary("sha256:nonexistent", "missing").is_none());
    }

    #[test]
    fn test_store_blob() -> Result<()> {
        let temp = TempDir::new()?;
        let cache = OciCache::new(temp.path().to_path_buf());

        // Create a test file
        let source = temp.path().join("source_blob");
        std::fs::write(&source, b"blob data")?;

        let digest = "sha256:blobhash123";
        let stored = cache.store_blob(digest, &source)?;

        assert!(cache.has_blob(digest));
        assert_eq!(stored, cache.blob_path(digest));

        let content = std::fs::read(&stored)?;
        assert_eq!(content, b"blob data");

        Ok(())
    }

    #[test]
    fn test_ensure_dirs() -> Result<()> {
        let temp = TempDir::new()?;
        let cache = OciCache::new(temp.path().to_path_buf());
        cache.ensure_dirs()?;

        assert!(temp.path().join("blobs").join("sha256").exists());
        assert!(temp.path().join("bin").join("sha256").exists());

        Ok(())
    }

    #[test]
    fn test_blob_path_without_prefix() {
        let cache = OciCache::new(PathBuf::from("/tmp/cache"));
        // When no prefix, defaults to sha256
        assert_eq!(
            cache.blob_path("abc123"),
            PathBuf::from("/tmp/cache/blobs/sha256/abc123")
        );
    }

    #[test]
    fn test_binary_dir_without_prefix() {
        let cache = OciCache::new(PathBuf::from("/tmp/cache"));
        assert_eq!(
            cache.binary_dir("xyz789"),
            PathBuf::from("/tmp/cache/bin/sha256/xyz789")
        );
    }

    #[test]
    fn test_parse_digest_sha512() {
        let (algo, hash) = parse_digest("sha512:longhashvalue");
        assert_eq!(algo, "sha512");
        assert_eq!(hash, "longhashvalue");
    }
}
