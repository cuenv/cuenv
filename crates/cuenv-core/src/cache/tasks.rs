use crate::{Error, Result};
use chrono::{DateTime, Utc};
use dirs::home_dir;
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

fn cache_root() -> Result<PathBuf> {
    let home = home_dir()
        .ok_or_else(|| Error::configuration("Failed to find home directory for cache"))?;
    Ok(home.join(".cuenv/cache/tasks"))
}

pub fn key_to_path(key: &str) -> Result<PathBuf> {
    Ok(cache_root()?.join(key))
}

pub fn lookup(key: &str) -> Option<CacheEntry> {
    let path = match key_to_path(key) {
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
) -> Result<()> {
    let path = key_to_path(key)?;
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
            let rel = p.strip_prefix(outputs_root).unwrap();
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

pub fn materialize_outputs(key: &str, destination: &Path) -> Result<usize> {
    let entry =
        lookup(key).ok_or_else(|| Error::configuration(format!("Cache key not found: {key}")))?;
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
        let rel = p.strip_prefix(&out_dir).unwrap();
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheKeyEnvelope {
    pub inputs: BTreeMap<String, String>,
    pub command: String,
    pub args: Vec<String>,
    pub shell: Option<serde_json::Value>,
    pub env: BTreeMap<String, String>,
    pub cuenv_version: String,
    pub platform: String,
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
        };
        let (k2, _) = compute_cache_key(&e2).unwrap();

        assert_eq!(k1, k2);
    }
}
