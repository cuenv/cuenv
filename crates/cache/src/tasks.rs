//! Task result caching with content-addressed storage

use crate::{Error, Result};
use chrono::{DateTime, Utc};
use dirs::{cache_dir, home_dir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Entry in the output file index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputIndexEntry {
    /// Relative path within output directory
    pub rel_path: String,
    /// File size in bytes
    pub size: u64,
    /// SHA256 hash of file contents
    pub sha256: String,
}

/// Metadata about a cached task result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResultMeta {
    /// Name of the task
    pub task_name: String,
    /// Command that was executed
    pub command: String,
    /// Arguments passed to the command
    pub args: Vec<String>,
    /// Summary of environment variables (non-secret)
    pub env_summary: BTreeMap<String, String>,
    /// Summary of input file hashes
    pub inputs_summary: BTreeMap<String, String>,
    /// When the result was created
    pub created_at: DateTime<Utc>,
    /// Version of cuenv that created this cache entry
    pub cuenv_version: String,
    /// Platform identifier
    pub platform: String,
    /// Execution duration in milliseconds
    pub duration_ms: u128,
    /// Exit code of the command
    pub exit_code: i32,
    /// Full cache key envelope for debugging
    pub cache_key_envelope: serde_json::Value,
    /// Index of output files
    pub output_index: Vec<OutputIndexEntry>,
}

/// A resolved cache entry
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The cache key
    pub key: String,
    /// Path to the cache entry directory
    pub path: PathBuf,
}

/// Inputs for determining cache root directory
#[derive(Debug, Clone)]
struct CacheInputs {
    cuenv_cache_dir: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    os_cache_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    temp_dir: PathBuf,
}

fn cache_root_from_inputs(inputs: CacheInputs) -> Result<PathBuf> {
    // Resolution order (first writable wins):
    // 1) CUENV_CACHE_DIR (explicit override)
    // 2) XDG_CACHE_HOME/cuenv/tasks
    // 3) OS cache dir/cuenv/tasks
    // 4) ~/.cuenv/cache/tasks (legacy)
    // 5) TMPDIR/cuenv/cache/tasks (fallback)
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(dir) = inputs.cuenv_cache_dir.filter(|p| !p.as_os_str().is_empty()) {
        candidates.push(dir);
    }
    if let Some(xdg) = inputs.xdg_cache_home {
        candidates.push(xdg.join("cuenv/tasks"));
    }
    if let Some(os_cache) = inputs.os_cache_dir {
        candidates.push(os_cache.join("cuenv/tasks"));
    }
    if let Some(home) = inputs.home_dir {
        candidates.push(home.join(".cuenv/cache/tasks"));
    }
    candidates.push(inputs.temp_dir.join("cuenv/cache/tasks"));

    for path in candidates {
        if path.starts_with("/homeless-shelter") {
            continue;
        }
        // If the path already exists, ensure it is writable; some CI environments
        // provide read-only cache directories under $HOME.
        if path.exists() {
            let probe = path.join(".write_probe");
            match std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&probe)
            {
                Ok(_) => {
                    let _ = std::fs::remove_file(&probe);
                    return Ok(path);
                }
                Err(_) => {
                    // Not writable, try next candidate
                    continue;
                }
            }
        }
        if std::fs::create_dir_all(&path).is_ok() {
            return Ok(path);
        }
        // Permission denied or other errors - try next candidate
    }
    Err(Error::configuration(
        "Failed to determine a writable cache directory",
    ))
}

fn cache_root() -> Result<PathBuf> {
    let inputs = CacheInputs {
        cuenv_cache_dir: std::env::var("CUENV_CACHE_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        xdg_cache_home: std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        os_cache_dir: cache_dir(),
        home_dir: home_dir(),
        temp_dir: std::env::temp_dir(),
    };
    cache_root_from_inputs(inputs)
}

/// Convert a cache key to its storage path
pub fn key_to_path(key: &str, root: Option<&Path>) -> Result<PathBuf> {
    let base = if let Some(r) = root {
        r.to_path_buf()
    } else {
        cache_root()?
    };
    Ok(base.join(key))
}

/// Look up a cache entry by key
#[must_use]
pub fn lookup(key: &str, root: Option<&Path>) -> Option<CacheEntry> {
    let Ok(path) = key_to_path(key, root) else {
        return None;
    };
    if path.exists() {
        Some(CacheEntry {
            key: key.to_string(),
            path,
        })
    } else {
        None
    }
}

/// Task execution logs
pub struct TaskLogs {
    /// Standard output from task
    pub stdout: Option<String>,
    /// Standard error from task
    pub stderr: Option<String>,
}

/// Save a task result to the cache
#[allow(clippy::too_many_arguments)] // Task result caching requires multiple path parameters
pub fn save_result(
    key: &str,
    meta: &TaskResultMeta,
    outputs_root: &Path,
    hermetic_root: &Path,
    logs: &TaskLogs,
    root: Option<&Path>,
) -> Result<()> {
    let path = key_to_path(key, root)?;
    fs::create_dir_all(&path).map_err(|e| Error::io(e, &path, "create_dir_all"))?;

    // metadata.json
    let meta_path = path.join("metadata.json");
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| Error::serialization(format!("Failed to serialize metadata: {e}")))?;
    fs::write(&meta_path, json).map_err(|e| Error::io(e, &meta_path, "write"))?;

    // outputs/
    let out_dir = path.join("outputs");
    fs::create_dir_all(&out_dir).map_err(|e| Error::io(e, &out_dir, "create_dir_all"))?;
    // Copy tree from outputs_root (already collected) if exists
    if outputs_root.exists() {
        for entry in walkdir::WalkDir::new(outputs_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if p.is_dir() {
                continue;
            }
            let rel = p.strip_prefix(outputs_root).map_err(|_| {
                Error::configuration(format!(
                    "path {} is not under outputs_root {}",
                    p.display(),
                    outputs_root.display()
                ))
            })?;
            let dst = out_dir.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::copy(p, &dst).map_err(|e| Error::io(e, &dst, "copy"))?;
        }
    }

    // logs/ - redact secrets before writing to disk
    let logs_dir = path.join("logs");
    fs::create_dir_all(&logs_dir).ok();
    if let Some(s) = logs.stdout.as_ref() {
        let redacted = cuenv_events::redact(s);
        let _ = fs::write(logs_dir.join("stdout.log"), redacted);
    }
    if let Some(s) = logs.stderr.as_ref() {
        let redacted = cuenv_events::redact(s);
        let _ = fs::write(logs_dir.join("stderr.log"), redacted);
    }

    // workspace snapshot
    let snapshot = path.join("workspace.tar.zst");
    snapshot_workspace_tar_zst(hermetic_root, &snapshot)?;

    Ok(())
}

/// Materialize cached outputs to a destination directory
pub fn materialize_outputs(key: &str, destination: &Path, root: Option<&Path>) -> Result<usize> {
    let entry = lookup(key, root).ok_or_else(|| Error::not_found(key))?;
    let out_dir = entry.path.join("outputs");
    if !out_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for e in walkdir::WalkDir::new(&out_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = e.path();
        if p.is_dir() {
            continue;
        }
        let rel = p.strip_prefix(&out_dir).map_err(|_| {
            Error::configuration(format!(
                "path {} is not under out_dir {}",
                p.display(),
                out_dir.display()
            ))
        })?;
        let dst = destination.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::copy(p, &dst).map_err(|e| Error::io(e, &dst, "copy"))?;
        count += 1;
    }
    Ok(count)
}

/// Index mapping task names to their latest cache keys (per project)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskLatestIndex {
    /// Map of (project_root_hash, task_name) -> cache_key
    pub entries: BTreeMap<String, BTreeMap<String, String>>,
}

fn latest_index_path(root: Option<&Path>) -> Result<PathBuf> {
    let base = if let Some(r) = root {
        r.to_path_buf()
    } else {
        cache_root()?
    };
    Ok(base.join("task-latest.json"))
}

fn project_hash(project_root: &Path) -> String {
    let digest = Sha256::digest(project_root.to_string_lossy().as_bytes());
    hex::encode(&digest[..8])
}

/// Record the latest cache key for a task in a project
pub fn record_latest(
    project_root: &Path,
    task_name: &str,
    cache_key: &str,
    root: Option<&Path>,
) -> Result<()> {
    let path = latest_index_path(root)?;
    let mut index: TaskLatestIndex = if path.exists() {
        let content = fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        TaskLatestIndex::default()
    };

    let proj_hash = project_hash(project_root);
    index
        .entries
        .entry(proj_hash)
        .or_default()
        .insert(task_name.to_string(), cache_key.to_string());

    let json = serde_json::to_string_pretty(&index)
        .map_err(|e| Error::serialization(format!("Failed to serialize latest index: {e}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&path, json).map_err(|e| Error::io(e, &path, "write"))?;
    Ok(())
}

/// Look up the latest cache key for a task in a project
#[must_use]
pub fn lookup_latest(project_root: &Path, task_name: &str, root: Option<&Path>) -> Option<String> {
    let path = latest_index_path(root).ok()?;
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    let index: TaskLatestIndex = serde_json::from_str(&content).ok()?;
    let proj_hash = project_hash(project_root);
    index.entries.get(&proj_hash)?.get(task_name).cloned()
}

/// Retrieve all latest cache keys for a given project
pub fn get_project_cache_keys(
    project_root: &Path,
    root: Option<&Path>,
) -> Result<Option<BTreeMap<String, String>>> {
    let path = latest_index_path(root)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|e| Error::io(e, &path, "read"))?;
    let index: TaskLatestIndex = serde_json::from_str(&content)
        .map_err(|e| Error::serialization(format!("Failed to parse task index: {e}")))?;
    let proj_hash = project_hash(project_root);
    Ok(index.entries.get(&proj_hash).cloned())
}

/// Cache key envelope for computing deterministic cache keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheKeyEnvelope {
    /// Input file hashes
    pub inputs: BTreeMap<String, String>,
    /// Command to execute
    pub command: String,
    /// Command arguments
    pub args: Vec<String>,
    /// Shell configuration
    pub shell: Option<serde_json::Value>,
    /// Environment variables
    pub env: BTreeMap<String, String>,
    /// cuenv version
    pub cuenv_version: String,
    /// Platform identifier
    pub platform: String,
    /// Hashes of the workspace lockfiles (key = workspace name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_lockfile_hashes: Option<BTreeMap<String, String>>,
    /// Hashes of workspace member packages (if relevant)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_package_hashes: Option<BTreeMap<String, String>>,
}

/// Compute a deterministic cache key from the envelope
pub fn compute_cache_key(envelope: &CacheKeyEnvelope) -> Result<(String, serde_json::Value)> {
    // Canonical JSON with sorted keys (BTreeMap ensures deterministic ordering for maps)
    let json = serde_json::to_value(envelope)
        .map_err(|e| Error::serialization(format!("Failed to encode envelope: {e}")))?;
    let bytes = serde_json::to_vec(&json)
        .map_err(|e| Error::serialization(format!("Failed to serialize envelope: {e}")))?;
    let digest = Sha256::digest(bytes);
    Ok((hex::encode(digest), json))
}

/// Create a compressed tar archive of a workspace directory
pub fn snapshot_workspace_tar_zst(src_root: &Path, dst_file: &Path) -> Result<()> {
    let file = fs::File::create(dst_file).map_err(|e| Error::io(e, dst_file, "create"))?;
    let enc = zstd::Encoder::new(file, 3)
        .map_err(|e| Error::configuration(format!("zstd encoder error: {e}")))?;
    let mut builder = tar::Builder::new(enc);

    match builder.append_dir_all(".", src_root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Workspace contents can legitimately disappear during a task (e.g.
            // package managers removing temp files). Skip snapshotting instead
            // of failing the whole task cache write.
            let _ = fs::remove_file(dst_file);
            tracing::warn!(
                root = %src_root.display(),
                "Skipping workspace snapshot; files disappeared during archive: {e}"
            );
            return Ok(());
        }
        Err(e) => {
            return Err(Error::configuration(format!("tar append failed: {e}")));
        }
    }

    let enc = builder
        .into_inner()
        .map_err(|e| Error::configuration(format!("tar finalize failed: {e}")))?;
    enc.finish()
        .map_err(|e| Error::configuration(format!("zstd finish failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[allow(dead_code, unsafe_code)]
    struct EnvVarGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        #[allow(dead_code, unsafe_code)]
        fn set<K: Into<String>, V: Into<String>>(key: K, value: V) -> Self {
            let key_s = key.into();
            let prev = std::env::var(&key_s).ok();
            // Rust 2024 makes env mutation unsafe; this test confines changes to the current thread
            // and restores previous values via Drop.
            unsafe {
                std::env::set_var(&key_s, value.into());
            }
            Self { key: key_s, prev }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(ref v) = self.prev {
                unsafe {
                    std::env::set_var(&self.key, v);
                }
            } else {
                unsafe {
                    std::env::remove_var(&self.key);
                }
            }
        }
    }

    // ==========================================================================
    // OutputIndexEntry tests
    // ==========================================================================

    #[test]
    fn test_output_index_entry_serde() {
        let entry = OutputIndexEntry {
            rel_path: "output/file.txt".to_string(),
            size: 1024,
            sha256: "abc123".to_string(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: OutputIndexEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.rel_path, "output/file.txt");
        assert_eq!(parsed.size, 1024);
        assert_eq!(parsed.sha256, "abc123");
    }

    #[test]
    fn test_output_index_entry_clone() {
        let entry = OutputIndexEntry {
            rel_path: "test.txt".to_string(),
            size: 100,
            sha256: "hash".to_string(),
        };

        let cloned = entry.clone();
        assert_eq!(cloned.rel_path, "test.txt");
    }

    // ==========================================================================
    // TaskResultMeta tests
    // ==========================================================================

    #[test]
    fn test_task_result_meta_serde() {
        let meta = TaskResultMeta {
            task_name: "build".to_string(),
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            env_summary: BTreeMap::new(),
            inputs_summary: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            cuenv_version: "0.1.0".to_string(),
            platform: "linux-x86_64".to_string(),
            duration_ms: 5000,
            exit_code: 0,
            cache_key_envelope: serde_json::json!({}),
            output_index: vec![],
        };

        let json = serde_json::to_string(&meta).unwrap();
        let parsed: TaskResultMeta = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.task_name, "build");
        assert_eq!(parsed.command, "cargo");
        assert_eq!(parsed.exit_code, 0);
    }

    #[test]
    fn test_task_result_meta_with_env() {
        let mut env_summary = BTreeMap::new();
        env_summary.insert("RUST_LOG".to_string(), "debug".to_string());

        let meta = TaskResultMeta {
            task_name: "test".to_string(),
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            env_summary,
            inputs_summary: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            cuenv_version: "0.1.0".to_string(),
            platform: "linux-x86_64".to_string(),
            duration_ms: 10000,
            exit_code: 0,
            cache_key_envelope: serde_json::json!({}),
            output_index: vec![],
        };

        assert_eq!(meta.env_summary.len(), 1);
        assert_eq!(meta.env_summary.get("RUST_LOG"), Some(&"debug".to_string()));
    }

    // ==========================================================================
    // CacheEntry tests
    // ==========================================================================

    #[test]
    fn test_cache_entry_fields() {
        let entry = CacheEntry {
            key: "abc123".to_string(),
            path: PathBuf::from("/cache/abc123"),
        };

        assert_eq!(entry.key, "abc123");
        assert_eq!(entry.path, PathBuf::from("/cache/abc123"));
    }

    #[test]
    fn test_cache_entry_clone() {
        let entry = CacheEntry {
            key: "key".to_string(),
            path: PathBuf::from("/path"),
        };

        let cloned = entry.clone();
        assert_eq!(cloned.key, "key");
    }

    // ==========================================================================
    // TaskLatestIndex tests
    // ==========================================================================

    #[test]
    fn test_task_latest_index_default() {
        let index = TaskLatestIndex::default();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn test_task_latest_index_serde() {
        let mut index = TaskLatestIndex::default();
        let mut tasks = BTreeMap::new();
        tasks.insert("build".to_string(), "key123".to_string());
        index.entries.insert("project_hash".to_string(), tasks);

        let json = serde_json::to_string(&index).unwrap();
        let parsed: TaskLatestIndex = serde_json::from_str(&json).unwrap();

        assert!(parsed.entries.contains_key("project_hash"));
    }

    // ==========================================================================
    // CacheKeyEnvelope tests
    // ==========================================================================

    #[test]
    fn test_cache_key_envelope_serde() {
        let envelope = CacheKeyEnvelope {
            inputs: BTreeMap::from([("file.txt".to_string(), "hash1".to_string())]),
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            shell: None,
            env: BTreeMap::new(),
            cuenv_version: "0.1.0".to_string(),
            platform: "linux".to_string(),
            workspace_lockfile_hashes: None,
            workspace_package_hashes: None,
        };

        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: CacheKeyEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.command, "echo");
    }

    #[test]
    fn test_cache_key_envelope_with_optional_fields() {
        let envelope = CacheKeyEnvelope {
            inputs: BTreeMap::new(),
            command: "npm".to_string(),
            args: vec!["install".to_string()],
            shell: Some(serde_json::json!({"type": "bash"})),
            env: BTreeMap::new(),
            cuenv_version: "0.1.0".to_string(),
            platform: "darwin".to_string(),
            workspace_lockfile_hashes: Some(BTreeMap::from([(
                "npm".to_string(),
                "lockfile_hash".to_string(),
            )])),
            workspace_package_hashes: Some(BTreeMap::from([(
                "pkg".to_string(),
                "pkg_hash".to_string(),
            )])),
        };

        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("workspace_lockfile_hashes"));
        assert!(json.contains("workspace_package_hashes"));
    }

    // ==========================================================================
    // key_to_path tests
    // ==========================================================================

    #[test]
    fn test_key_to_path_with_root() {
        let temp = TempDir::new().unwrap();
        let path = key_to_path("mykey", Some(temp.path())).unwrap();
        assert!(path.ends_with("mykey"));
        assert!(path.starts_with(temp.path()));
    }

    // ==========================================================================
    // lookup tests
    // ==========================================================================

    #[test]
    fn test_lookup_not_found() {
        let temp = TempDir::new().unwrap();
        let result = lookup("nonexistent", Some(temp.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_lookup_found() {
        let temp = TempDir::new().unwrap();
        let key_dir = temp.path().join("mykey");
        fs::create_dir_all(&key_dir).unwrap();

        let result = lookup("mykey", Some(temp.path()));
        assert!(result.is_some());
        let entry = result.unwrap();
        assert_eq!(entry.key, "mykey");
    }

    // ==========================================================================
    // record_latest and lookup_latest tests
    // ==========================================================================

    #[test]
    fn test_record_and_lookup_latest() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();

        record_latest(&project_root, "build", "key123", Some(temp.path())).unwrap();

        let result = lookup_latest(&project_root, "build", Some(temp.path()));
        assert_eq!(result, Some("key123".to_string()));
    }

    #[test]
    fn test_lookup_latest_not_found() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");

        let result = lookup_latest(&project_root, "nonexistent", Some(temp.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_record_latest_overwrites() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();

        record_latest(&project_root, "build", "key1", Some(temp.path())).unwrap();
        record_latest(&project_root, "build", "key2", Some(temp.path())).unwrap();

        let result = lookup_latest(&project_root, "build", Some(temp.path()));
        assert_eq!(result, Some("key2".to_string()));
    }

    // ==========================================================================
    // get_project_cache_keys tests
    // ==========================================================================

    #[test]
    fn test_get_project_cache_keys_empty() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");

        let result = get_project_cache_keys(&project_root, Some(temp.path())).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_project_cache_keys_with_data() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();

        record_latest(&project_root, "build", "key1", Some(temp.path())).unwrap();
        record_latest(&project_root, "test", "key2", Some(temp.path())).unwrap();

        let result = get_project_cache_keys(&project_root, Some(temp.path()))
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("build"), Some(&"key1".to_string()));
        assert_eq!(result.get("test"), Some(&"key2".to_string()));
    }

    // ==========================================================================
    // compute_cache_key tests
    // ==========================================================================

    #[test]
    fn cache_key_is_deterministic_and_order_invariant() {
        let mut env_a = BTreeMap::new();
        env_a.insert("A".to_string(), "1".to_string());
        env_a.insert("B".to_string(), "2".to_string());
        let mut inputs1 = BTreeMap::new();
        inputs1.insert("b.txt".to_string(), "hashb".to_string());
        inputs1.insert("a.txt".to_string(), "hasha".to_string());
        let e1 = CacheKeyEnvelope {
            inputs: inputs1,
            command: "echo".into(),
            args: vec!["hi".into()],
            shell: None,
            env: env_a.clone(),
            cuenv_version: "0.1.1".into(),
            platform: "linux-x86_64".into(),
            workspace_lockfile_hashes: None,
            workspace_package_hashes: None,
        };
        let (k1, _) = compute_cache_key(&e1).unwrap();

        // Same data but different insertion orders
        let mut env_b = BTreeMap::new();
        env_b.insert("B".to_string(), "2".to_string());
        env_b.insert("A".to_string(), "1".to_string());
        let mut inputs2 = BTreeMap::new();
        inputs2.insert("a.txt".to_string(), "hasha".to_string());
        inputs2.insert("b.txt".to_string(), "hashb".to_string());
        let e2 = CacheKeyEnvelope {
            inputs: inputs2,
            command: "echo".into(),
            args: vec!["hi".into()],
            shell: None,
            env: env_b,
            cuenv_version: "0.1.1".into(),
            platform: "linux-x86_64".into(),
            workspace_lockfile_hashes: None,
            workspace_package_hashes: None,
        };
        let (k2, _) = compute_cache_key(&e2).unwrap();

        assert_eq!(k1, k2);
    }

    // ==========================================================================
    // Cache Invalidation Behavioral Tests
    // ==========================================================================
    // These tests verify the behavioral contracts around cache invalidation:
    // When any component of the cache key changes, the key MUST change.

    /// Helper to create a baseline envelope for invalidation tests
    fn baseline_envelope() -> CacheKeyEnvelope {
        CacheKeyEnvelope {
            inputs: BTreeMap::from([
                ("src/main.rs".to_string(), "abc123".to_string()),
                ("Cargo.toml".to_string(), "def456".to_string()),
            ]),
            command: "cargo".to_string(),
            args: vec!["build".to_string(), "--release".to_string()],
            shell: None,
            env: BTreeMap::from([
                ("RUST_LOG".to_string(), "debug".to_string()),
                ("CC".to_string(), "clang".to_string()),
            ]),
            cuenv_version: "1.0.0".to_string(),
            platform: "linux-x86_64".to_string(),
            workspace_lockfile_hashes: None,
            workspace_package_hashes: None,
        }
    }

    #[test]
    fn cache_invalidates_when_input_file_content_changes() {
        // Given: A task with specific input file hashes
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: An input file's content changes (different hash)
        let mut modified = base.clone();
        modified
            .inputs
            .insert("src/main.rs".to_string(), "changed_hash".to_string());
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different (cache is invalidated)
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when input file content changes"
        );
    }

    #[test]
    fn cache_invalidates_when_new_input_file_added() {
        // Given: A task with specific inputs
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: A new input file is added
        let mut modified = base.clone();
        modified
            .inputs
            .insert("src/lib.rs".to_string(), "new_file_hash".to_string());
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when new input file is added"
        );
    }

    #[test]
    fn cache_invalidates_when_input_file_removed() {
        // Given: A task with specific inputs
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: An input file is removed
        let mut modified = base.clone();
        modified.inputs.remove("src/main.rs");
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when input file is removed"
        );
    }

    #[test]
    fn cache_invalidates_when_command_changes() {
        // Given: A task with a specific command
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: The command changes
        let mut modified = base.clone();
        modified.command = "rustc".to_string();
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when command changes"
        );
    }

    #[test]
    fn cache_invalidates_when_args_change() {
        // Given: A task with specific arguments
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: Arguments change
        let mut modified = base.clone();
        modified.args = vec!["build".to_string()]; // removed --release
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when command arguments change"
        );
    }

    #[test]
    fn cache_invalidates_when_env_var_value_changes() {
        // Given: A task with specific environment variables
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: An environment variable value changes
        let mut modified = base.clone();
        modified
            .env
            .insert("RUST_LOG".to_string(), "info".to_string());
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when environment variable value changes"
        );
    }

    #[test]
    fn cache_invalidates_when_env_var_added() {
        // Given: A task with specific environment variables
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: A new environment variable is added
        let mut modified = base.clone();
        modified
            .env
            .insert("NEW_VAR".to_string(), "value".to_string());
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when new environment variable is added"
        );
    }

    #[test]
    fn cache_invalidates_when_platform_changes() {
        // Given: A task built for a specific platform
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: The platform changes (cross-compilation or different machine)
        let mut modified = base.clone();
        modified.platform = "darwin-aarch64".to_string();
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when platform changes"
        );
    }

    #[test]
    fn cache_invalidates_when_cuenv_version_changes() {
        // Given: A task built with a specific cuenv version
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: cuenv version changes (may affect execution semantics)
        let mut modified = base.clone();
        modified.cuenv_version = "2.0.0".to_string();
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when cuenv version changes"
        );
    }

    #[test]
    fn cache_invalidates_when_workspace_lockfile_changes() {
        // Given: A task with no workspace lockfile hashes
        let base = baseline_envelope();
        let (base_key, _) = compute_cache_key(&base).unwrap();

        // When: Workspace lockfile is added or changes
        let mut modified = base.clone();
        modified.workspace_lockfile_hashes = Some(BTreeMap::from([(
            "cargo".to_string(),
            "lockfile_hash_123".to_string(),
        )]));
        let (new_key, _) = compute_cache_key(&modified).unwrap();

        // Then: Cache key must be different
        assert_ne!(
            base_key, new_key,
            "Cache must invalidate when workspace lockfile changes"
        );
    }

    #[test]
    fn cache_stable_when_nothing_changes() {
        // Given: A task configuration
        let envelope = baseline_envelope();

        // When: We compute the cache key multiple times
        let (key1, _) = compute_cache_key(&envelope).unwrap();
        let (key2, _) = compute_cache_key(&envelope).unwrap();
        let (key3, _) = compute_cache_key(&envelope).unwrap();

        // Then: All keys should be identical (cache hits work correctly)
        assert_eq!(key1, key2, "Cache key must be stable across calls");
        assert_eq!(key2, key3, "Cache key must be stable across calls");
    }

    #[test]
    fn cache_root_skips_homeless_shelter() {
        let tmp = std::env::temp_dir();
        let inputs = CacheInputs {
            cuenv_cache_dir: None,
            xdg_cache_home: Some(PathBuf::from("/homeless-shelter/.cache")),
            os_cache_dir: None,
            home_dir: Some(PathBuf::from("/homeless-shelter")),
            temp_dir: tmp.clone(),
        };
        let dir =
            cache_root_from_inputs(inputs).expect("cache_root should choose a writable fallback");
        assert!(!dir.starts_with("/homeless-shelter"));
        assert!(dir.starts_with(&tmp));
    }

    #[test]
    fn cache_root_respects_override_env() {
        let tmp = std::env::temp_dir().join("cuenv-test-override");
        let _ = std::fs::remove_dir_all(&tmp);
        let inputs = CacheInputs {
            cuenv_cache_dir: Some(tmp.clone()),
            xdg_cache_home: None,
            os_cache_dir: None,
            home_dir: None,
            temp_dir: std::env::temp_dir(),
        };
        let dir = cache_root_from_inputs(inputs).expect("cache_root should use override");
        assert!(dir.starts_with(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_and_materialize_outputs_roundtrip() {
        // Force cache root into a temp directory to avoid touching user dirs
        let cache_tmp = TempDir::new().expect("tempdir");

        // Prepare fake outputs
        let outputs = TempDir::new().expect("outputs tempdir");
        std::fs::create_dir_all(outputs.path().join("dir")).unwrap();
        std::fs::write(outputs.path().join("foo.txt"), b"foo").unwrap();
        std::fs::write(outputs.path().join("dir/bar.bin"), b"bar").unwrap();

        // Prepare hermetic workspace to snapshot
        let herm = TempDir::new().expect("hermetic tempdir");
        std::fs::create_dir_all(herm.path().join("work")).unwrap();
        std::fs::write(herm.path().join("work/a.txt"), b"a").unwrap();

        // Minimal metadata
        let mut env_summary = BTreeMap::new();
        env_summary.insert("FOO".to_string(), "1".to_string());
        let inputs_summary = BTreeMap::new();
        let output_index = vec![
            OutputIndexEntry {
                rel_path: "foo.txt".to_string(),
                size: 3,
                sha256: {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(b"foo");
                    hex::encode(h.finalize())
                },
            },
            OutputIndexEntry {
                rel_path: "dir/bar.bin".to_string(),
                size: 3,
                sha256: {
                    use sha2::{Digest, Sha256};
                    let mut h = Sha256::new();
                    h.update(b"bar");
                    hex::encode(h.finalize())
                },
            },
        ];

        let meta = TaskResultMeta {
            task_name: "unit".into(),
            command: "echo".into(),
            args: vec!["ok".into()],
            env_summary,
            inputs_summary,
            created_at: chrono::Utc::now(),
            cuenv_version: "0.0.0-test".into(),
            platform: std::env::consts::OS.to_string(),
            duration_ms: 1,
            exit_code: 0,
            cache_key_envelope: serde_json::json!({}),
            output_index,
        };

        let logs = TaskLogs {
            stdout: Some("hello".into()),
            stderr: Some(String::new()),
        };

        let key = "roundtrip-key-123";
        save_result(
            key,
            &meta,
            outputs.path(),
            herm.path(),
            &logs,
            Some(cache_tmp.path()),
        )
        .expect("save_result");

        // Verify cache layout
        let base = key_to_path(key, Some(cache_tmp.path())).expect("key_to_path");
        assert!(base.join("metadata.json").exists());
        assert!(base.join("outputs/foo.txt").exists());
        assert!(base.join("outputs/dir/bar.bin").exists());
        assert!(base.join("logs/stdout.log").exists());
        let snapshot = base.join("workspace.tar.zst");
        let snap_meta = std::fs::metadata(&snapshot).unwrap();
        assert!(snap_meta.len() > 0);

        // Materialize into fresh destination
        let dest = TempDir::new().expect("dest tempdir");
        let copied = materialize_outputs(key, dest.path(), Some(cache_tmp.path()))
            .expect("materialize_outputs");
        assert_eq!(copied, 2);
        assert_eq!(std::fs::read(dest.path().join("foo.txt")).unwrap(), b"foo");
        assert_eq!(
            std::fs::read(dest.path().join("dir/bar.bin")).unwrap(),
            b"bar"
        );
    }

    #[test]
    fn test_snapshot_workspace_tar_zst() {
        let src = TempDir::new().unwrap();
        std::fs::create_dir_all(src.path().join("subdir")).unwrap();
        std::fs::write(src.path().join("file.txt"), "content").unwrap();
        std::fs::write(src.path().join("subdir/nested.txt"), "nested").unwrap();

        let dst = TempDir::new().unwrap();
        let archive_path = dst.path().join("archive.tar.zst");

        snapshot_workspace_tar_zst(src.path(), &archive_path).unwrap();
        assert!(archive_path.exists());
        // Verify the archive is non-empty
        let metadata = std::fs::metadata(&archive_path).unwrap();
        assert!(metadata.len() > 0);
    }
}
