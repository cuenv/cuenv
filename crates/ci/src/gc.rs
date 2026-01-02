//! Garbage Collection
//!
//! LRU-based cleanup for local cache and optionally Nix store closures.

// GC involves complex file system traversal with LRU and size calculations
#![allow(clippy::cognitive_complexity, clippy::too_many_lines)]

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// Default max cache size (10 GB)
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Default max age for cache entries (30 days)
pub const DEFAULT_MAX_AGE_DAYS: u32 = 30;

/// Errors for garbage collection
#[derive(Debug, Error)]
pub enum GCError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Cache directory not found
    #[error("Cache directory not found: {0}")]
    CacheDirNotFound(PathBuf),

    /// Nix garbage collection failed
    #[error("Nix garbage collection failed: {0}")]
    NixGCFailed(String),
}

/// Statistics from garbage collection run
#[derive(Debug, Clone, Default)]
pub struct GCStats {
    /// Number of entries scanned
    pub entries_scanned: usize,
    /// Number of entries removed
    pub entries_removed: usize,
    /// Bytes freed
    pub bytes_freed: u64,
    /// Current cache size after GC
    pub current_size: u64,
    /// Duration of GC run
    pub duration_ms: u64,
}

/// Cache entry with metadata for LRU sorting
#[derive(Debug)]
struct CacheEntry {
    path: PathBuf,
    size: u64,
    last_accessed: SystemTime,
}

/// Garbage collector configuration
#[derive(Debug, Clone)]
pub struct GCConfig {
    /// Cache directory to clean
    pub cache_dir: PathBuf,
    /// Maximum total cache size in bytes
    pub max_size_bytes: u64,
    /// Maximum age for cache entries in days
    pub max_age_days: u32,
    /// Whether to run Nix garbage collection
    pub run_nix_gc: bool,
    /// Dry run (don't actually delete)
    pub dry_run: bool,
}

impl Default for GCConfig {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from(".cuenv/cache"),
            max_size_bytes: DEFAULT_MAX_SIZE_BYTES,
            max_age_days: DEFAULT_MAX_AGE_DAYS,
            run_nix_gc: false,
            dry_run: false,
        }
    }
}

/// Garbage collector for CI cache
pub struct GarbageCollector {
    config: GCConfig,
}

impl GarbageCollector {
    /// Create a new garbage collector with default config
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: GCConfig::default(),
        }
    }

    /// Create with custom configuration
    #[must_use]
    pub const fn with_config(config: GCConfig) -> Self {
        Self { config }
    }

    /// Set the cache directory
    #[must_use]
    pub fn cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.cache_dir = dir.into();
        self
    }

    /// Set max cache size
    #[must_use]
    pub const fn max_size(mut self, bytes: u64) -> Self {
        self.config.max_size_bytes = bytes;
        self
    }

    /// Set max age in days
    #[must_use]
    pub const fn max_age_days(mut self, days: u32) -> Self {
        self.config.max_age_days = days;
        self
    }

    /// Enable Nix garbage collection
    #[must_use]
    pub const fn with_nix_gc(mut self) -> Self {
        self.config.run_nix_gc = true;
        self
    }

    /// Enable dry run mode
    #[must_use]
    pub const fn dry_run(mut self) -> Self {
        self.config.dry_run = true;
        self
    }

    /// Run garbage collection
    ///
    /// # Errors
    ///
    /// Returns `GCError` if garbage collection fails.
    pub fn run(&self) -> Result<GCStats, GCError> {
        let start = std::time::Instant::now();
        let mut stats = GCStats::default();

        if !self.config.cache_dir.exists() {
            tracing::debug!(
                dir = %self.config.cache_dir.display(),
                "Cache directory does not exist, nothing to clean"
            );
            return Ok(stats);
        }

        // Collect all cache entries
        let mut entries = Self::scan_cache(&self.config.cache_dir)?;
        stats.entries_scanned = entries.len();

        // Calculate current size
        let total_size: u64 = entries.iter().map(|e| e.size).sum();
        tracing::info!(
            entries = entries.len(),
            size_mb = total_size / (1024 * 1024),
            "Scanned cache"
        );

        // Sort by last accessed (oldest first)
        entries.sort_by(|a, b| a.last_accessed.cmp(&b.last_accessed));

        let now = SystemTime::now();
        let max_age = Duration::from_secs(u64::from(self.config.max_age_days) * 24 * 60 * 60);
        let mut current_size = total_size;

        // Remove entries that are too old or exceed size limit
        for entry in entries {
            let age = now
                .duration_since(entry.last_accessed)
                .unwrap_or(Duration::ZERO);

            let should_remove = age > max_age || current_size > self.config.max_size_bytes;

            if should_remove {
                if self.config.dry_run {
                    tracing::info!(
                        path = %entry.path.display(),
                        size = entry.size,
                        age_days = age.as_secs() / (24 * 60 * 60),
                        "[dry-run] Would remove"
                    );
                } else {
                    match Self::remove_entry(&entry.path) {
                        Ok(()) => {
                            tracing::debug!(
                                path = %entry.path.display(),
                                size = entry.size,
                                "Removed cache entry"
                            );
                            stats.entries_removed += 1;
                            stats.bytes_freed += entry.size;
                            current_size = current_size.saturating_sub(entry.size);
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %entry.path.display(),
                                error = %e,
                                "Failed to remove cache entry"
                            );
                        }
                    }
                }
            }

            // Stop if we're under the size limit and past max age check
            if current_size <= self.config.max_size_bytes && age <= max_age {
                break;
            }
        }

        stats.current_size = current_size;

        // Run Nix GC if configured
        if self.config.run_nix_gc
            && !self.config.dry_run
            && let Err(e) = Self::run_nix_gc()
        {
            tracing::warn!(error = %e, "Nix garbage collection failed");
        }

        stats.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        tracing::info!(
            removed = stats.entries_removed,
            freed_mb = stats.bytes_freed / (1024 * 1024),
            current_mb = stats.current_size / (1024 * 1024),
            duration_ms = stats.duration_ms,
            "Garbage collection complete"
        );

        Ok(stats)
    }

    /// Scan the cache directory and collect all cache entries with their metadata.
    fn scan_cache(dir: &Path) -> Result<Vec<CacheEntry>, GCError> {
        let mut entries = Vec::new();
        Self::scan_dir_recursive(dir, &mut entries)?;
        Ok(entries)
    }

    /// Recursively traverse a directory tree, collecting file entries.
    fn scan_dir_recursive(dir: &Path, entries: &mut Vec<CacheEntry>) -> Result<(), GCError> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                Self::scan_dir_recursive(&path, entries)?;
            } else if metadata.is_file()
                && let Some(cache_entry) = Self::create_entry(&path, &metadata)
            {
                entries.push(cache_entry);
            }
        }
        Ok(())
    }

    /// Create a cache entry from file path and metadata.
    ///
    /// Returns `None` if the access/modification time cannot be determined.
    fn create_entry(path: &Path, metadata: &Metadata) -> Option<CacheEntry> {
        let size = metadata.len();
        let last_accessed = metadata.accessed().or_else(|_| metadata.modified()).ok()?;

        Some(CacheEntry {
            path: path.to_path_buf(),
            size,
            last_accessed,
        })
    }

    /// Remove a cache entry (file or directory).
    fn remove_entry(path: &Path) -> Result<(), GCError> {
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn run_nix_gc() -> Result<(), GCError> {
        tracing::info!("Running Nix garbage collection...");

        let output = std::process::Command::new("nix-collect-garbage")
            .arg("-d") // Delete old generations
            .output()
            .map_err(|e| GCError::NixGCFailed(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GCError::NixGCFailed(stderr.to_string()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::info!(output = %stdout, "Nix garbage collection complete");

        Ok(())
    }
}

impl Default for GarbageCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function to run GC with default settings
///
/// # Errors
///
/// Returns `GCError` if garbage collection fails.
pub fn run_gc(cache_dir: &Path) -> Result<GCStats, GCError> {
    GarbageCollector::new().cache_dir(cache_dir).run()
}

/// Run GC in dry-run mode to see what would be deleted
///
/// # Errors
///
/// Returns `GCError` if garbage collection preview fails.
pub fn preview_gc(cache_dir: &Path) -> Result<GCStats, GCError> {
    GarbageCollector::new().cache_dir(cache_dir).dry_run().run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        file.write_all(&vec![0u8; size]).unwrap();
        path
    }

    #[test]
    fn test_empty_cache() {
        let tmp = TempDir::new().unwrap();
        let gc = GarbageCollector::new().cache_dir(tmp.path());
        let stats = gc.run().unwrap();
        assert_eq!(stats.entries_scanned, 0);
        assert_eq!(stats.entries_removed, 0);
    }

    #[test]
    fn test_cache_under_limit() {
        let tmp = TempDir::new().unwrap();
        create_test_file(tmp.path(), "file1.cache", 1000);
        create_test_file(tmp.path(), "file2.cache", 2000);

        let gc = GarbageCollector::new()
            .cache_dir(tmp.path())
            .max_size(1024 * 1024); // 1MB limit

        let stats = gc.run().unwrap();
        assert_eq!(stats.entries_scanned, 2);
        assert_eq!(stats.entries_removed, 0); // Nothing removed, under limit
    }

    #[test]
    fn test_cache_over_limit() {
        let tmp = TempDir::new().unwrap();
        create_test_file(tmp.path(), "file1.cache", 500);
        create_test_file(tmp.path(), "file2.cache", 500);
        create_test_file(tmp.path(), "file3.cache", 500);

        let gc = GarbageCollector::new().cache_dir(tmp.path()).max_size(1000); // Limit to 1000 bytes

        let stats = gc.run().unwrap();
        assert!(stats.entries_removed > 0);
        assert!(stats.current_size <= 1000);
    }

    #[test]
    fn test_dry_run() {
        let tmp = TempDir::new().unwrap();
        let file = create_test_file(tmp.path(), "file1.cache", 500);

        let gc = GarbageCollector::new()
            .cache_dir(tmp.path())
            .max_size(100) // Force removal
            .dry_run();

        let stats = gc.run().unwrap();
        // File should still exist in dry run mode
        assert!(file.exists());
        assert_eq!(stats.entries_removed, 0); // Dry run doesn't count as removed
    }

    #[test]
    fn test_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        create_test_file(tmp.path(), "root.cache", 100);
        create_test_file(&subdir, "nested.cache", 100);

        let gc = GarbageCollector::new().cache_dir(tmp.path());
        let stats = gc.run().unwrap();

        assert_eq!(stats.entries_scanned, 2);
    }

    #[test]
    fn test_nonexistent_cache_dir() {
        let gc = GarbageCollector::new().cache_dir("/nonexistent/path");
        let stats = gc.run().unwrap();
        assert_eq!(stats.entries_scanned, 0);
    }

    #[test]
    fn test_gc_stats_defaults() {
        let stats = GCStats::default();
        assert_eq!(stats.entries_scanned, 0);
        assert_eq!(stats.entries_removed, 0);
        assert_eq!(stats.bytes_freed, 0);
    }

    #[test]
    fn test_gc_stats_clone() {
        let stats = GCStats {
            entries_scanned: 10,
            entries_removed: 5,
            bytes_freed: 1024,
            current_size: 2048,
            duration_ms: 100,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.entries_scanned, 10);
        assert_eq!(cloned.entries_removed, 5);
        assert_eq!(cloned.bytes_freed, 1024);
    }

    #[test]
    fn test_gc_stats_debug() {
        let stats = GCStats::default();
        let debug_str = format!("{:?}", stats);
        assert!(debug_str.contains("GCStats"));
    }

    #[test]
    fn test_gc_config_default() {
        let config = GCConfig::default();
        assert_eq!(config.max_size_bytes, DEFAULT_MAX_SIZE_BYTES);
        assert_eq!(config.max_age_days, DEFAULT_MAX_AGE_DAYS);
        assert!(!config.run_nix_gc);
        assert!(!config.dry_run);
    }

    #[test]
    fn test_gc_config_clone() {
        let config = GCConfig {
            cache_dir: PathBuf::from("/test"),
            max_size_bytes: 1000,
            max_age_days: 7,
            run_nix_gc: true,
            dry_run: true,
        };
        let cloned = config.clone();
        assert_eq!(cloned.cache_dir, PathBuf::from("/test"));
        assert_eq!(cloned.max_size_bytes, 1000);
    }

    #[test]
    fn test_gc_config_debug() {
        let config = GCConfig::default();
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("GCConfig"));
    }

    #[test]
    fn test_garbage_collector_new() {
        let gc = GarbageCollector::new();
        // Just verify it can be created
        assert!(gc.config.cache_dir.to_string_lossy().contains("cache"));
    }

    #[test]
    fn test_garbage_collector_default() {
        let gc = GarbageCollector::default();
        assert!(gc.config.cache_dir.to_string_lossy().contains("cache"));
    }

    #[test]
    fn test_garbage_collector_with_config() {
        let config = GCConfig {
            cache_dir: PathBuf::from("/custom/path"),
            max_size_bytes: 5000,
            max_age_days: 14,
            run_nix_gc: false,
            dry_run: false,
        };
        let gc = GarbageCollector::with_config(config);
        assert_eq!(gc.config.cache_dir, PathBuf::from("/custom/path"));
        assert_eq!(gc.config.max_size_bytes, 5000);
    }

    #[test]
    fn test_garbage_collector_builder_cache_dir() {
        let gc = GarbageCollector::new().cache_dir("/test/cache");
        assert_eq!(gc.config.cache_dir, PathBuf::from("/test/cache"));
    }

    #[test]
    fn test_garbage_collector_builder_max_size() {
        let gc = GarbageCollector::new().max_size(12345);
        assert_eq!(gc.config.max_size_bytes, 12345);
    }

    #[test]
    fn test_garbage_collector_builder_max_age_days() {
        let gc = GarbageCollector::new().max_age_days(60);
        assert_eq!(gc.config.max_age_days, 60);
    }

    #[test]
    fn test_garbage_collector_builder_with_nix_gc() {
        let gc = GarbageCollector::new().with_nix_gc();
        assert!(gc.config.run_nix_gc);
    }

    #[test]
    fn test_garbage_collector_builder_dry_run() {
        let gc = GarbageCollector::new().dry_run();
        assert!(gc.config.dry_run);
    }

    #[test]
    fn test_garbage_collector_builder_chained() {
        let gc = GarbageCollector::new()
            .cache_dir("/test")
            .max_size(1000)
            .max_age_days(7)
            .with_nix_gc()
            .dry_run();

        assert_eq!(gc.config.cache_dir, PathBuf::from("/test"));
        assert_eq!(gc.config.max_size_bytes, 1000);
        assert_eq!(gc.config.max_age_days, 7);
        assert!(gc.config.run_nix_gc);
        assert!(gc.config.dry_run);
    }

    #[test]
    fn test_run_gc_convenience() {
        let tmp = TempDir::new().unwrap();
        let stats = run_gc(tmp.path()).unwrap();
        assert_eq!(stats.entries_scanned, 0);
    }

    #[test]
    fn test_preview_gc_convenience() {
        let tmp = TempDir::new().unwrap();
        create_test_file(tmp.path(), "file.cache", 100);

        let stats = preview_gc(tmp.path()).unwrap();
        assert_eq!(stats.entries_scanned, 1);
        // File should still exist after preview
        assert!(tmp.path().join("file.cache").exists());
    }

    #[test]
    fn test_gc_error_io() {
        let err = GCError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        let display = format!("{}", err);
        assert!(display.contains("IO error"));
    }

    #[test]
    fn test_gc_error_cache_dir_not_found() {
        let err = GCError::CacheDirNotFound(PathBuf::from("/test"));
        let display = format!("{}", err);
        assert!(display.contains("Cache directory not found"));
    }

    #[test]
    fn test_gc_error_nix_gc_failed() {
        let err = GCError::NixGCFailed("command failed".to_string());
        let display = format!("{}", err);
        assert!(display.contains("Nix garbage collection failed"));
    }

    #[test]
    fn test_gc_error_debug() {
        let err = GCError::NixGCFailed("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("NixGCFailed"));
    }

    #[test]
    fn test_constants() {
        // Verify the default constants are reasonable
        assert!(DEFAULT_MAX_SIZE_BYTES > 1024 * 1024); // > 1MB
        assert!(DEFAULT_MAX_AGE_DAYS > 0);
        assert!(DEFAULT_MAX_AGE_DAYS <= 365);
    }

    #[test]
    fn test_deeply_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let level1 = tmp.path().join("level1");
        let level2 = level1.join("level2");
        let level3 = level2.join("level3");
        fs::create_dir_all(&level3).unwrap();

        create_test_file(&level3, "deep.cache", 100);

        let gc = GarbageCollector::new().cache_dir(tmp.path());
        let stats = gc.run().unwrap();

        assert_eq!(stats.entries_scanned, 1);
    }

    #[test]
    fn test_gc_with_mixed_content() {
        let tmp = TempDir::new().unwrap();

        // Create files of various sizes
        create_test_file(tmp.path(), "small.cache", 10);
        create_test_file(tmp.path(), "medium.cache", 1000);
        create_test_file(tmp.path(), "large.cache", 10000);

        let gc = GarbageCollector::new().cache_dir(tmp.path()).max_size(5000);

        let stats = gc.run().unwrap();
        assert_eq!(stats.entries_scanned, 3);
        // At least one file should be removed to get under limit
        assert!(stats.entries_removed >= 1);
    }
}
