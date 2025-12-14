//! Cache implementation for CUE evaluation results

use crate::error::{CueEngineError, Result};
use lru::LruCache;
use parking_lot::RwLock;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    package_name: String,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    value: String,
    timestamp: Instant,
}

/// Thread-safe LRU cache for CUE evaluation results
pub struct EvaluationCache {
    cache: RwLock<LruCache<CacheKey, CacheEntry>>,
    ttl: Duration,
}

impl EvaluationCache {
    /// Creates a new evaluation cache
    ///
    /// # Errors
    ///
    /// Returns an error if capacity is 0
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self> {
        let capacity = NonZeroUsize::new(capacity)
            .ok_or_else(|| CueEngineError::cache("Cache capacity must be non-zero"))?;

        Ok(Self {
            cache: RwLock::new(LruCache::new(capacity)),
            ttl,
        })
    }

    /// Gets a value from the cache if it exists and hasn't expired
    pub fn get(&self, path: &Path, package_name: &str) -> Option<String> {
        let key = CacheKey {
            path: path.to_path_buf(),
            package_name: package_name.to_string(),
        };

        let mut cache = self.cache.write();

        if let Some(entry) = cache.get(&key) {
            // Check if entry is still valid
            if entry.timestamp.elapsed() < self.ttl {
                return Some(entry.value.clone());
            }
            // Remove expired entry
            cache.pop(&key);
        }

        None
    }

    /// Inserts a value into the cache
    pub fn insert(&self, path: &Path, package_name: &str, value: String) {
        let key = CacheKey {
            path: path.to_path_buf(),
            package_name: package_name.to_string(),
        };

        let entry = CacheEntry {
            value,
            timestamp: Instant::now(),
        };

        self.cache.write().put(key, entry);
    }

    /// Clears all entries from the cache
    pub fn clear(&self) {
        self.cache.write().clear();
    }

    /// Returns the number of entries in the cache
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Returns true if the cache is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_cache_basic_operations() {
        let cache = EvaluationCache::new(10, Duration::from_secs(60)).unwrap();
        let path = Path::new("/test");

        // Test insertion and retrieval
        cache.insert(path, "pkg1", "result1".to_string());
        assert_eq!(cache.get(path, "pkg1"), Some("result1".to_string()));

        // Test different package
        assert_eq!(cache.get(path, "pkg2"), None);

        // Test cache size
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_expiration() {
        let cache = EvaluationCache::new(10, Duration::from_millis(100)).unwrap();
        let path = Path::new("/test");

        cache.insert(path, "pkg", "result".to_string());
        assert_eq!(cache.get(path, "pkg"), Some("result".to_string()));

        // Wait for expiration
        thread::sleep(Duration::from_millis(150));
        assert_eq!(cache.get(path, "pkg"), None);
    }

    #[test]
    fn test_cache_lru_eviction() {
        let cache = EvaluationCache::new(2, Duration::from_secs(60)).unwrap();

        cache.insert(Path::new("/test1"), "pkg", "result1".to_string());
        cache.insert(Path::new("/test2"), "pkg", "result2".to_string());
        cache.insert(Path::new("/test3"), "pkg", "result3".to_string());

        // First entry should be evicted
        assert_eq!(cache.get(Path::new("/test1"), "pkg"), None);
        assert_eq!(
            cache.get(Path::new("/test2"), "pkg"),
            Some("result2".to_string())
        );
        assert_eq!(
            cache.get(Path::new("/test3"), "pkg"),
            Some("result3".to_string())
        );
    }
}
