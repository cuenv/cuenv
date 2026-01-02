//! Garbage collection for cache entries
//!
//! Provides utilities to clean up old or unused cache entries and CAS blobs.

use super::cas::{BlobId, CasStore};
use super::tasks::{cas_store, key_to_path, TaskLatestIndex, latest_index_path};
use crate::{Error, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// GC policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcPolicy {
    /// Maximum age for cache entries in days
    pub max_age_days: Option<u64>,
    /// Maximum total cache size in bytes
    pub max_size_bytes: Option<u64>,
    /// Keep at least this many recent entries per task
    pub min_entries_per_task: usize,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            max_age_days: Some(30), // 30 days default
            max_size_bytes: None,
            min_entries_per_task: 3,
        }
    }
}

/// Result of a GC operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcResult {
    /// Number of cache entries removed
    pub entries_removed: usize,
    /// Number of CAS blobs removed
    pub blobs_removed: usize,
    /// Bytes freed
    pub bytes_freed: u64,
}

/// Metadata for a cache entry used during GC
#[derive(Debug, Clone)]
struct CacheEntryInfo {
    key: String,
    path: PathBuf,
    created_at: DateTime<Utc>,
    size: u64,
}

/// Find all cache entries in the cache root
fn find_cache_entries(root: &Path) -> Result<Vec<CacheEntryInfo>> {
    let mut entries = Vec::new();

    if !root.exists() {
        return Ok(entries);
    }

    for entry in fs::read_dir(root).map_err(|e| Error::Io {
        source: e,
        path: Some(root.into()),
        operation: "read_dir".into(),
    })? {
        let entry = entry.map_err(|e| Error::Io {
            source: e,
            path: Some(root.into()),
            operation: "read_dir_entry".into(),
        })?;

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Skip CAS directory and index files
        if let Some(name) = path.file_name() {
            if name == "cas" || name.to_string_lossy().ends_with(".json") {
                continue;
            }
        }

        // Load metadata to get creation time
        let metadata_path = path.join("metadata.json");
        if metadata_path.exists() {
            if let Ok(meta_content) = fs::read_to_string(&metadata_path) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_content) {
                    if let Some(created_str) = meta.get("created_at").and_then(|v| v.as_str()) {
                        if let Ok(created_at) = DateTime::parse_from_rfc3339(created_str) {
                            // Calculate entry size
                            let size = calculate_directory_size(&path)?;

                            entries.push(CacheEntryInfo {
                                key: path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string(),
                                path: path.clone(),
                                created_at: created_at.with_timezone(&Utc),
                                size,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(entries)
}

/// Calculate total size of a directory recursively
fn calculate_directory_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;

    if path.is_file() {
        return Ok(fs::metadata(path)
            .map_err(|e| Error::Io {
                source: e,
                path: Some(path.into()),
                operation: "metadata".into(),
            })?
            .len());
    }

    for entry in fs::read_dir(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.into()),
        operation: "read_dir".into(),
    })? {
        let entry = entry.map_err(|e| Error::Io {
            source: e,
            path: Some(path.into()),
            operation: "read_dir_entry".into(),
        })?;
        total += calculate_directory_size(&entry.path())?;
    }

    Ok(total)
}

/// Find all blob IDs referenced by cache entries
fn find_referenced_blobs(root: &Path) -> Result<HashSet<String>> {
    let mut referenced = HashSet::new();
    let entries = find_cache_entries(root)?;

    for entry in entries {
        let metadata_path = entry.path.join("metadata.json");
        if let Ok(meta_content) = fs::read_to_string(&metadata_path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_content) {
                if let Some(output_index) = meta.get("output_index").and_then(|v| v.as_array()) {
                    for output in output_index {
                        if let Some(blob_id) = output.get("blob_id").and_then(|v| v.as_str()) {
                            referenced.insert(blob_id.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(referenced)
}

/// Run garbage collection on the cache
///
/// # Errors
///
/// Returns error if IO operations fail
pub fn gc(root: Option<&Path>, policy: &GcPolicy) -> Result<GcResult> {
    let cache_root = if let Some(r) = root {
        r.to_path_buf()
    } else {
        super::tasks::cache_root()?
    };

    let mut result = GcResult {
        entries_removed: 0,
        blobs_removed: 0,
        bytes_freed: 0,
    };

    // Find all cache entries
    let mut entries = find_cache_entries(&cache_root)?;

    // Sort by creation time (oldest first)
    entries.sort_by_key(|e| e.created_at);

    // Load the latest index to protect recent entries
    let latest_index = if let Ok(path) = latest_index_path(root) {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                serde_json::from_str::<TaskLatestIndex>(&content).ok()
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Build set of protected keys (latest entries)
    let protected_keys: HashSet<String> = if let Some(index) = &latest_index {
        index
            .entries
            .values()
            .flat_map(|task_map| task_map.values())
            .cloned()
            .collect()
    } else {
        HashSet::new()
    };

    // Calculate cutoff time for age-based cleanup
    let cutoff_time = if let Some(max_age_days) = policy.max_age_days {
        Some(Utc::now() - Duration::days(max_age_days as i64))
    } else {
        None
    };

    // Remove old entries (respecting protected keys)
    for entry in &entries {
        // Skip if this is a protected key
        if protected_keys.contains(&entry.key) {
            continue;
        }

        // Check age policy
        let should_remove = if let Some(cutoff) = cutoff_time {
            entry.created_at < cutoff
        } else {
            false
        };

        if should_remove {
            if let Ok(()) = remove_cache_entry(&entry.path) {
                result.entries_removed += 1;
                result.bytes_freed += entry.size;
                tracing::debug!(
                    key = %entry.key,
                    size = entry.size,
                    "Removed old cache entry"
                );
            }
        }
    }

    // Size-based cleanup: remove oldest entries if total size exceeds limit
    if let Some(max_size) = policy.max_size_bytes {
        // Recalculate remaining entries after age-based cleanup
        let remaining_entries = find_cache_entries(&cache_root)?;
        let total_size: u64 = remaining_entries.iter().map(|e| e.size).sum();

        if total_size > max_size {
            let mut sorted_entries = remaining_entries;
            sorted_entries.sort_by_key(|e| e.created_at);

            let mut current_size = total_size;
            for entry in sorted_entries {
                if current_size <= max_size {
                    break;
                }

                // Skip protected keys
                if protected_keys.contains(&entry.key) {
                    continue;
                }

                if let Ok(()) = remove_cache_entry(&entry.path) {
                    result.entries_removed += 1;
                    result.bytes_freed += entry.size;
                    current_size -= entry.size;
                    tracing::debug!(
                        key = %entry.key,
                        size = entry.size,
                        "Removed entry to reduce cache size"
                    );
                }
            }
        }
    }

    // GC unreferenced blobs in CAS
    let store = cas_store(root)?;
    let referenced_blobs = find_referenced_blobs(&cache_root)?;
    let all_blobs = store.list()?;

    for blob_id in all_blobs {
        if !referenced_blobs.contains(blob_id.as_hex()) {
            if let Ok(size) = store.size(&blob_id) {
                if store.delete(&blob_id).is_ok() {
                    result.blobs_removed += 1;
                    result.bytes_freed += size;
                    tracing::debug!(
                        blob_id = %blob_id,
                        size = size,
                        "Removed unreferenced CAS blob"
                    );
                }
            }
        }
    }

    Ok(result)
}

/// Remove a cache entry directory
fn remove_cache_entry(path: &Path) -> Result<()> {
    fs::remove_dir_all(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.into()),
        operation: "remove_dir_all".into(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tasks::{save_result, TaskLogs, TaskResultMeta, OutputIndexEntry};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    #[test]
    fn test_gc_removes_old_entries() {
        let cache_tmp = TempDir::new().unwrap();

        // Create some old cache entries
        let outputs = TempDir::new().unwrap();
        let herm = TempDir::new().unwrap();

        let meta = TaskResultMeta {
            task_name: "test".into(),
            command: "echo".into(),
            args: vec![],
            env_summary: BTreeMap::new(),
            inputs_summary: BTreeMap::new(),
            created_at: Utc::now() - Duration::days(60), // Old entry
            cuenv_version: "0.0.0".into(),
            platform: "test".into(),
            duration_ms: 1,
            exit_code: 0,
            cache_key_envelope: serde_json::json!({}),
            output_index: vec![],
        };

        save_result(
            "old-entry",
            &meta,
            outputs.path(),
            herm.path(),
            TaskLogs {
                stdout: None,
                stderr: None,
            },
            Some(cache_tmp.path()),
        )
        .unwrap();

        // Run GC with 30-day policy
        let policy = GcPolicy {
            max_age_days: Some(30),
            max_size_bytes: None,
            min_entries_per_task: 0,
        };

        let result = gc(Some(cache_tmp.path()), &policy).unwrap();
        assert!(result.entries_removed > 0);
    }

    #[test]
    fn test_gc_preserves_recent_entries() {
        let cache_tmp = TempDir::new().unwrap();

        // Create a recent cache entry
        let outputs = TempDir::new().unwrap();
        let herm = TempDir::new().unwrap();

        let meta = TaskResultMeta {
            task_name: "test".into(),
            command: "echo".into(),
            args: vec![],
            env_summary: BTreeMap::new(),
            inputs_summary: BTreeMap::new(),
            created_at: Utc::now(),
            cuenv_version: "0.0.0".into(),
            platform: "test".into(),
            duration_ms: 1,
            exit_code: 0,
            cache_key_envelope: serde_json::json!({}),
            output_index: vec![],
        };

        save_result(
            "recent-entry",
            &meta,
            outputs.path(),
            herm.path(),
            TaskLogs {
                stdout: None,
                stderr: None,
            },
            Some(cache_tmp.path()),
        )
        .unwrap();

        // Run GC
        let policy = GcPolicy::default();
        let result = gc(Some(cache_tmp.path()), &policy).unwrap();
        
        // Recent entry should not be removed
        assert_eq!(result.entries_removed, 0);
    }
}
