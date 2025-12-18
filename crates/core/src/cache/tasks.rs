use crate::{Error, Result};
use chrono::{DateTime, Utc};
use dirs::{cache_dir, home_dir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputIndexEntry {
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResultMeta {
    pub task_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env_summary: BTreeMap<String, String>,
    pub inputs_summary: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub cuenv_version: String,
    pub platform: String,
    pub duration_ms: u128,
    pub exit_code: i32,
    pub cache_key_envelope: serde_json::Value,
    pub output_index: Vec<OutputIndexEntry>,
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub key: String,
    pub path: PathBuf,
}

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
        // provide read‑only cache directories under $HOME.
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
        match std::fs::create_dir_all(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => continue,
            Err(_) => continue,
        }
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

pub fn key_to_path(key: &str, root: Option<&Path>) -> Result<PathBuf> {
    let base = if let Some(r) = root {
        r.to_path_buf()
    } else {
        cache_root()?
    };
    Ok(base.join(key))
}

pub fn lookup(key: &str, root: Option<&Path>) -> Option<CacheEntry> {
    let path = match key_to_path(key, root) {
        Ok(p) => p,
        Err(_) => return None,
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

pub struct TaskLogs {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

pub fn save_result(
    key: &str,
    meta: &TaskResultMeta,
    outputs_root: &Path,
    hermetic_root: &Path,
    logs: TaskLogs,
    root: Option<&Path>,
) -> Result<()> {
    let path = key_to_path(key, root)?;
    fs::create_dir_all(&path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.clone().into()),
        operation: "create_dir_all".into(),
    })?;

    // metadata.json
    let meta_path = path.join("metadata.json");
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| Error::configuration(format!("Failed to serialize metadata: {e}")))?;
    fs::write(&meta_path, json).map_err(|e| Error::Io {
        source: e,
        path: Some(meta_path.into()),
        operation: "write".into(),
    })?;

    // outputs/
    let out_dir = path.join("outputs");
    fs::create_dir_all(&out_dir).map_err(|e| Error::Io {
        source: e,
        path: Some(out_dir.clone().into()),
        operation: "create_dir_all".into(),
    })?;
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
            let rel = p
                .strip_prefix(outputs_root)
                .expect("WalkDir entry is under outputs_root");
            let dst = out_dir.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::copy(p, &dst).map_err(|e| Error::Io {
                source: e,
                path: Some(dst.into()),
                operation: "copy".into(),
            })?;
        }
    }

    // logs/
    let logs_dir = path.join("logs");
    fs::create_dir_all(&logs_dir).ok();
    if let Some(s) = logs.stdout.as_ref() {
        let _ = fs::write(logs_dir.join("stdout.log"), s);
    }
    if let Some(s) = logs.stderr.as_ref() {
        let _ = fs::write(logs_dir.join("stderr.log"), s);
    }

    // workspace snapshot
    let snapshot = path.join("workspace.tar.zst");
    crate::tasks::io::snapshot_workspace_tar_zst(hermetic_root, &snapshot)?;

    Ok(())
}

pub fn materialize_outputs(key: &str, destination: &Path, root: Option<&Path>) -> Result<usize> {
    let entry = lookup(key, root)
        .ok_or_else(|| Error::configuration(format!("Cache key not found: {key}")))?;
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
        let rel = p
            .strip_prefix(&out_dir)
            .expect("WalkDir entry is under out_dir");
        let dst = destination.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::copy(p, &dst).map_err(|e| Error::Io {
            source: e,
            path: Some(dst.into()),
            operation: "copy".into(),
        })?;
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
        .map_err(|e| Error::configuration(format!("Failed to serialize latest index: {e}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&path, json).map_err(|e| Error::Io {
        source: e,
        path: Some(path.into()),
        operation: "write".into(),
    })?;
    Ok(())
}

/// Look up the latest cache key for a task in a project
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
    let content = fs::read_to_string(&path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.clone().into()),
        operation: "read".into(),
    })?;
    let index: TaskLatestIndex =
        serde_json::from_str(&content).map_err(|e| Error::configuration(format!("Failed to parse task index: {e}")))?;
    let proj_hash = project_hash(project_root);
    Ok(index.entries.get(&proj_hash).cloned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheKeyEnvelope {
    pub inputs: BTreeMap<String, String>,
    pub command: String,
    pub args: Vec<String>,
    pub shell: Option<serde_json::Value>,
    pub env: BTreeMap<String, String>,
    pub cuenv_version: String,
    pub platform: String,
    /// Hashes of the workspace lockfiles (key = workspace name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_lockfile_hashes: Option<BTreeMap<String, String>>,
    /// Hashes of workspace member packages (if relevant)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_package_hashes: Option<BTreeMap<String, String>>,
}

pub fn compute_cache_key(envelope: &CacheKeyEnvelope) -> Result<(String, serde_json::Value)> {
    // Canonical JSON with sorted keys (BTreeMap ensures deterministic ordering for maps)
    let json = serde_json::to_value(envelope)
        .map_err(|e| Error::configuration(format!("Failed to encode envelope: {e}")))?;
    let bytes = serde_json::to_vec(&json)
        .map_err(|e| Error::configuration(format!("Failed to serialize envelope: {e}")))?;
    let digest = Sha256::digest(bytes);
    Ok((hex::encode(digest), json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[allow(dead_code)]
    struct EnvVarGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        #[allow(dead_code)]
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
            stderr: Some("".into()),
        };

        let key = "roundtrip-key-123";
        save_result(
            key,
            &meta,
            outputs.path(),
            herm.path(),
            logs,
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
}
