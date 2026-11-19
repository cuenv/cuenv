//! Inspection, integrity verification, and safe garbage collection for a local
//! CAS + ActionCache directory.
//!
//! Read-only diagnostics (`stats`, `verify`) never mutate on-disk state.
//! Destructive operations (`gc`, `prune`, `verify_and_repair`) protect both
//! transitive references (nested `Tree` leaf blobs) and freshly written blobs
//! within a configurable grace window whose `ActionCache` entry may still be
//! pending.

use crate::action_cache::LocalActionCache;
use crate::cas::LocalCas;
use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::integrity::{missing_blobs, referenced_blobs};
use crate::message::ActionResult;
use bazel_remote_apis::build::bazel::remote::execution::v2::ActionResult as PbActionResult;
use prost::Message as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Summary statistics for a local cuenv cache root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheStats {
    /// On-disk root directory inspected.
    pub root: PathBuf,
    /// Number of blob files under `cas/sha256`.
    pub cas_blobs: u64,
    /// Total bytes occupied by blob files under `cas/sha256`.
    pub cas_bytes: u64,
    /// Number of action-cache entries under `ac/sha256`.
    pub action_entries: u64,
    /// Total bytes occupied by action-cache entries under `ac/sha256`.
    pub action_bytes: u64,
    /// Total bytes currently sitting in `tmp/` staging directories.
    pub temp_bytes: u64,
    /// Total bytes currently sitting in `exec/` sandbox roots.
    pub exec_bytes: u64,
}

impl CacheStats {
    /// Total bytes across CAS, ActionCache, `tmp/`, and `exec/`.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.cas_bytes
            .saturating_add(self.action_bytes)
            .saturating_add(self.temp_bytes)
            .saturating_add(self.exec_bytes)
    }
}

/// A corrupt CAS blob discovered during verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorruptBlob {
    /// Expected SHA-256 hex digest derived from the blob's on-disk path.
    pub expected_hash: String,
    /// Actual SHA-256 hex digest computed from the file's bytes, if readable.
    pub actual_hash: Option<String>,
    /// On-disk path of the blob.
    pub path: PathBuf,
    /// Human-readable reason for the failure.
    pub reason: String,
}

/// A corrupt or dangling ActionCache entry discovered during verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvalidActionEntry {
    /// SHA-256 hex digest derived from the entry's on-disk path.
    pub action_hash: String,
    /// On-disk path of the entry.
    pub path: PathBuf,
    /// Human-readable reason (decode error or missing referenced blobs).
    pub reason: String,
    /// Referenced CAS digests missing from the local store, if decoded.
    pub missing_blobs: Vec<String>,
}

/// Result of a read-only integrity check over a local cache root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyReport {
    /// On-disk root directory verified.
    pub root: PathBuf,
    /// Total CAS blobs checked.
    pub cas_blobs_checked: u64,
    /// Total ActionCache entries checked.
    pub action_entries_checked: u64,
    /// Corrupt or unreadable CAS blobs.
    pub corrupt_blobs: Vec<CorruptBlob>,
    /// Undecodable or dangling ActionCache entries.
    pub invalid_actions: Vec<InvalidActionEntry>,
}

impl VerifyReport {
    /// True when no corrupt blobs or invalid/dangling action entries were found.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.corrupt_blobs.is_empty() && self.invalid_actions.is_empty()
    }
}

/// Collect disk-usage and entry-count statistics for a local cache root.
///
/// Missing subdirectories (`cas/sha256`, `ac/sha256`, `tmp`, `exec`) are
/// treated as empty rather than errors so `cuenv cache stats` works on a fresh
/// machine before any task has run.
///
/// # Errors
///
/// Returns an I/O error if an existing cache directory cannot be traversed.
pub fn stats(root: &Path) -> Result<CacheStats> {
    let (cas_blobs, cas_bytes) = walk_files(&root.join("cas").join("sha256"))?;
    let (action_entries, action_bytes) = walk_files(&root.join("ac").join("sha256"))?;
    let (_, temp_bytes) = walk_files(&root.join("tmp"))?;
    let (_, exec_bytes) = walk_files(&root.join("exec"))?;

    Ok(CacheStats {
        root: root.to_path_buf(),
        cas_blobs,
        cas_bytes,
        action_entries,
        action_bytes,
        temp_bytes,
        exec_bytes,
    })
}

/// Perform a read-only integrity check of all local CAS blobs and ActionCache
/// entries under `root`.
///
/// Every CAS blob is re-hashed and compared against its path-derived SHA-256
/// digest. Every ActionCache entry is decoded as a REAPI `ActionResult` and
/// checked transitively with [`missing_blobs`] so dangling output files or
/// nested `Tree` entries are surfaced.
///
/// # Errors
///
/// Returns an error if the local store directories cannot be opened or
/// traversed.
pub async fn verify(root: &Path) -> Result<VerifyReport> {
    let mut report = VerifyReport {
        root: root.to_path_buf(),
        ..VerifyReport::default()
    };

    let cas_root = root.join("cas").join("sha256");
    if cas_root.exists() {
        for (expected_hash, path) in collect_sharded_entries(&cas_root)? {
            report.cas_blobs_checked += 1;
            match hash_file(&path) {
                Ok(actual_hash) => {
                    if actual_hash != expected_hash {
                        report.corrupt_blobs.push(CorruptBlob {
                            expected_hash,
                            actual_hash: Some(actual_hash),
                            path,
                            reason: "SHA-256 content digest does not match path".to_string(),
                        });
                    }
                }
                Err(error) => {
                    report.corrupt_blobs.push(CorruptBlob {
                        expected_hash,
                        actual_hash: None,
                        path,
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    let ac_root = root.join("ac").join("sha256");
    if ac_root.exists() {
        let local_cas = LocalCas::open(root)?;
        let _ = LocalActionCache::open(root)?;

        for (action_hash, path) in collect_sharded_entries(&ac_root)? {
            report.action_entries_checked += 1;
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    report.invalid_actions.push(InvalidActionEntry {
                        action_hash,
                        path,
                        reason: format!("unreadable action entry: {error}"),
                        missing_blobs: Vec::new(),
                    });
                    continue;
                }
            };

            let proto = match PbActionResult::decode(bytes.as_slice()) {
                Ok(proto) => proto,
                Err(error) => {
                    report.invalid_actions.push(InvalidActionEntry {
                        action_hash,
                        path,
                        reason: format!("invalid ActionResult protobuf: {error}"),
                        missing_blobs: Vec::new(),
                    });
                    continue;
                }
            };

            let result = match ActionResult::from_proto(&proto) {
                Ok(result) => result,
                Err(error) => {
                    report.invalid_actions.push(InvalidActionEntry {
                        action_hash,
                        path,
                        reason: format!("invalid ActionResult fields: {error}"),
                        missing_blobs: Vec::new(),
                    });
                    continue;
                }
            };

            match missing_blobs(&local_cas, &result).await {
                Ok(missing) if missing.is_empty() => {}
                Ok(missing) => {
                    report.invalid_actions.push(InvalidActionEntry {
                        action_hash,
                        path,
                        reason: format!("references {} missing CAS blob(s)", missing.len()),
                        missing_blobs: missing.iter().map(Digest::to_resource).collect(),
                    });
                }
                Err(error) => {
                    report.invalid_actions.push(InvalidActionEntry {
                        action_hash,
                        path,
                        reason: format!("referential integrity check failed: {error}"),
                        missing_blobs: Vec::new(),
                    });
                }
            }
        }
    }

    Ok(report)
}

/// Default grace window protecting in-flight tasks whose CAS blobs have been
/// written before their `ActionCache` entry is published.
pub const DEFAULT_GC_GRACE_WINDOW: Duration = Duration::from_mins(10);

/// Options controlling [`gc`].
#[derive(Debug, Clone)]
pub struct GcOptions {
    /// Target upper bound on total local cache size (CAS + ActionCache bytes).
    pub max_bytes: Option<u64>,
    /// Evict `ActionCache` entries older than this duration.
    pub max_age: Option<Duration>,
    /// Protect blobs, action entries, and temp staging directories modified
    /// more recently than this window so concurrent tasks are not disrupted.
    pub grace_window: Duration,
    /// Compute the exact eviction set without deleting any files.
    pub dry_run: bool,
}

impl Default for GcOptions {
    fn default() -> Self {
        Self {
            max_bytes: None,
            max_age: None,
            grace_window: DEFAULT_GC_GRACE_WINDOW,
            dry_run: false,
        }
    }
}

/// Options controlling [`prune`].
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Protect blobs and temp staging directories modified more recently than
    /// this window.
    pub grace_window: Duration,
    /// Compute the unreferenced set without deleting any files.
    pub dry_run: bool,
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            grace_window: DEFAULT_GC_GRACE_WINDOW,
            dry_run: false,
        }
    }
}

/// Summary of a [`gc`] or [`prune`] pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GcReport {
    /// On-disk root directory cleaned.
    pub root: PathBuf,
    /// True when no files were actually deleted.
    pub dry_run: bool,
    /// Number of `ActionCache` entries removed (or selected in dry-run mode).
    pub actions_removed: u64,
    /// Bytes freed from `ac/sha256`.
    pub actions_bytes_freed: u64,
    /// Number of unreferenced CAS blobs removed (or selected in dry-run mode).
    pub blobs_removed: u64,
    /// Bytes freed from `cas/sha256`.
    pub blobs_bytes_freed: u64,
    /// Number of stale `tmp/` entries removed (or selected in dry-run mode).
    pub temp_entries_removed: u64,
    /// Bytes freed from `tmp/`.
    pub temp_bytes_freed: u64,
    /// Unreferenced CAS blobs skipped because they were within `grace_window`.
    pub protected_recent_blobs: u64,
    /// Eligible `ActionCache` entries skipped because they were within `grace_window`.
    pub protected_recent_actions: u64,
}

impl GcReport {
    /// Total bytes freed across ActionCache, CAS, and `tmp/`.
    #[must_use]
    pub fn total_bytes_freed(&self) -> u64 {
        self.actions_bytes_freed
            .saturating_add(self.blobs_bytes_freed)
            .saturating_add(self.temp_bytes_freed)
    }
}

/// Summary of [`verify_and_repair`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairReport {
    /// Findings from the initial verification pass.
    pub verify: VerifyReport,
    /// Number of corrupt CAS blobs deleted.
    pub blobs_removed: u64,
    /// Number of invalid or dangling `ActionCache` entries deleted.
    pub actions_removed: u64,
}

struct IndexedEntry {
    hash: String,
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Garbage-collect a local cache root, evicting aged or excess `ActionCache`
/// entries and sweeping unreferenced CAS blobs and stale `tmp/` directories.
///
/// Transitive references (including nested `Tree` file leaves) of every
/// surviving `ActionCache` entry are retained, and any entry modified within
/// `options.grace_window` is protected from deletion.
///
/// # Errors
///
/// Returns an error if the cache directories cannot be read or files cannot be
/// removed.
pub async fn gc(root: &Path, options: GcOptions) -> Result<GcReport> {
    let now = SystemTime::now();
    let mut report = GcReport {
        root: root.to_path_buf(),
        dry_run: options.dry_run,
        ..GcReport::default()
    };

    let cas_root = root.join("cas").join("sha256");
    let ac_root = root.join("ac").join("sha256");
    let local_cas = if cas_root.exists() {
        Some(LocalCas::open(root)?)
    } else {
        None
    };

    // Index all CAS blobs with size and mtime.
    let mut cas_blobs: HashMap<String, IndexedEntry> = HashMap::new();
    if cas_root.exists() {
        for (hash, path) in collect_sharded_entries(&cas_root)? {
            if let Ok(meta) = fs::metadata(&path) {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                cas_blobs.insert(
                    hash.clone(),
                    IndexedEntry {
                        hash,
                        path,
                        size: meta.len(),
                        mtime,
                    },
                );
            }
        }
    }

    // Index all ActionCache entries sorted oldest-first.
    let mut ac_entries: Vec<IndexedEntry> = Vec::new();
    if ac_root.exists() {
        for (hash, path) in collect_sharded_entries(&ac_root)? {
            if let Ok(meta) = fs::metadata(&path) {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                ac_entries.push(IndexedEntry {
                    hash,
                    path,
                    size: meta.len(),
                    mtime,
                });
            }
        }
    }
    ac_entries.sort_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.hash.cmp(&b.hash)));

    // Resolve transitive CAS blob hashes referenced by each ActionCache entry.
    let mut ac_refs: Vec<HashSet<String>> = Vec::with_capacity(ac_entries.len());
    for entry in &ac_entries {
        let mut refs = HashSet::new();
        if let Some(cas) = &local_cas
            && let Ok(bytes) = fs::read(&entry.path)
            && let Ok(proto) = PbActionResult::decode(bytes.as_slice())
            && let Ok(result) = ActionResult::from_proto(&proto)
            && let Ok(digests) = referenced_blobs(cas, &result).await
        {
            for digest in digests {
                refs.insert(digest.hash);
            }
        }
        ac_refs.push(refs);
    }

    // Determine which ActionCache entries to evict by age, then by size budget.
    let mut evicted_ac = vec![false; ac_entries.len()];
    if let Some(max_age) = options.max_age {
        for (idx, entry) in ac_entries.iter().enumerate() {
            let age = now.duration_since(entry.mtime).unwrap_or_default();
            if age > max_age {
                if is_within_grace(entry.mtime, now, options.grace_window) {
                    report.protected_recent_actions += 1;
                } else {
                    evicted_ac[idx] = true;
                }
            }
        }
    }

    // Build reference counts from surviving ActionCache entries.
    let mut ref_counts: HashMap<String, usize> = HashMap::new();
    for (idx, refs) in ac_refs.iter().enumerate() {
        if evicted_ac[idx] {
            continue;
        }
        for hash in refs {
            *ref_counts.entry(hash.clone()).or_insert(0) += 1;
        }
    }

    // Enforce max_bytes budget by evicting oldest surviving ActionCache entries.
    if let Some(max_bytes) = options.max_bytes {
        let mut live_bytes: u64 = ac_entries
            .iter()
            .enumerate()
            .filter(|(idx, _)| !evicted_ac[*idx])
            .map(|(_, entry)| entry.size)
            .sum();
        for (hash, blob) in &cas_blobs {
            let referenced = ref_counts.get(hash).copied().unwrap_or(0) > 0;
            if referenced || is_within_grace(blob.mtime, now, options.grace_window) {
                live_bytes = live_bytes.saturating_add(blob.size);
            }
        }

        for (idx, entry) in ac_entries.iter().enumerate() {
            if live_bytes <= max_bytes {
                break;
            }
            if evicted_ac[idx] {
                continue;
            }
            if is_within_grace(entry.mtime, now, options.grace_window) {
                report.protected_recent_actions += 1;
                continue;
            }
            evicted_ac[idx] = true;
            live_bytes = live_bytes.saturating_sub(entry.size);
            for hash in &ac_refs[idx] {
                if let Some(count) = ref_counts.get_mut(hash) {
                    *count = count.saturating_sub(1);
                    if *count == 0
                        && let Some(blob) = cas_blobs.get(hash)
                        && !is_within_grace(blob.mtime, now, options.grace_window)
                    {
                        live_bytes = live_bytes.saturating_sub(blob.size);
                    }
                }
            }
        }
    }

    // Delete evicted ActionCache entries.
    for (idx, entry) in ac_entries.iter().enumerate() {
        if !evicted_ac[idx] {
            continue;
        }
        if !options.dry_run {
            remove_file_if_exists(&entry.path)?;
        }
        report.actions_removed += 1;
        report.actions_bytes_freed = report.actions_bytes_freed.saturating_add(entry.size);
    }

    // Sweep unreferenced CAS blobs older than the grace window.
    let mut cas_list: Vec<&IndexedEntry> = cas_blobs.values().collect();
    cas_list.sort_by(|a, b| a.hash.cmp(&b.hash));
    for blob in cas_list {
        let referenced = ref_counts.get(&blob.hash).copied().unwrap_or(0) > 0;
        if referenced {
            continue;
        }
        if is_within_grace(blob.mtime, now, options.grace_window) {
            report.protected_recent_blobs += 1;
            continue;
        }
        if !options.dry_run {
            remove_file_if_exists(&blob.path)?;
        }
        report.blobs_removed += 1;
        report.blobs_bytes_freed = report.blobs_bytes_freed.saturating_add(blob.size);
    }

    // Sweep stale tmp/ entries older than the grace window.
    let tmp_root = root.join("tmp");
    if tmp_root.exists() {
        let entries = match fs::read_dir(&tmp_root) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(Error::io(e, &tmp_root, "read_dir")),
        };
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(e, &tmp_root, "read_dir entry"))?;
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Error::io(e, &path, "metadata")),
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if is_within_grace(mtime, now, options.grace_window) {
                continue;
            }
            let bytes = if meta.is_dir() {
                walk_files(&path)?.1
            } else {
                meta.len()
            };
            if !options.dry_run {
                if meta.is_dir() {
                    if let Err(e) = fs::remove_dir_all(&path)
                        && e.kind() != io::ErrorKind::NotFound
                    {
                        return Err(Error::io(e, &path, "remove_dir_all"));
                    }
                } else {
                    remove_file_if_exists(&path)?;
                }
            }
            report.temp_entries_removed += 1;
            report.temp_bytes_freed = report.temp_bytes_freed.saturating_add(bytes);
        }
    }

    Ok(report)
}

/// Remove unreferenced CAS blobs and stale `tmp/` staging entries without
/// evicting any valid `ActionCache` entries.
///
/// # Errors
///
/// Returns an error if the cache directories cannot be read or files cannot be
/// removed.
pub async fn prune(root: &Path, options: PruneOptions) -> Result<GcReport> {
    gc(
        root,
        GcOptions {
            max_bytes: None,
            max_age: None,
            grace_window: options.grace_window,
            dry_run: options.dry_run,
        },
    )
    .await
}

/// Verify a local cache root and remove any corrupt CAS blobs and invalid or
/// dangling `ActionCache` entries.
///
/// Runs a second check after deleting corrupt blobs so any `ActionCache`
/// entries that became dangling due to a deleted corrupt blob are also removed.
///
/// # Errors
///
/// Returns an error if the store cannot be read or corrupt entries cannot be
/// deleted.
pub async fn verify_and_repair(root: &Path) -> Result<RepairReport> {
    let initial = verify(root).await?;
    let mut blobs_removed = 0u64;
    let mut actions_removed = 0u64;

    for blob in &initial.corrupt_blobs {
        if remove_file_if_exists(&blob.path)? {
            blobs_removed += 1;
        }
    }
    for action in &initial.invalid_actions {
        if remove_file_if_exists(&action.path)? {
            actions_removed += 1;
        }
    }

    // If any corrupt blobs were removed, re-verify so newly dangling AC entries
    // are also cleaned up.
    if blobs_removed > 0 {
        let follow_up = verify(root).await?;
        for action in &follow_up.invalid_actions {
            if remove_file_if_exists(&action.path)? {
                actions_removed += 1;
            }
        }
    }

    Ok(RepairReport {
        verify: initial,
        blobs_removed,
        actions_removed,
    })
}

fn is_within_grace(mtime: SystemTime, now: SystemTime, grace: Duration) -> bool {
    if grace.is_zero() {
        return false;
    }
    match now.duration_since(mtime) {
        Ok(age) => age < grace,
        // Future mtime (clock skew) is treated as recent.
        Err(_) => true,
    }
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(e, path, "remove_file")),
    }
}

fn walk_files(dir: &Path) -> Result<(u64, u64)> {
    if !dir.exists() {
        return Ok((0, 0));
    }

    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(e, &current, "read_dir")),
        };

        for entry in entries {
            let entry = entry.map_err(|e| Error::io(e, &current, "read_dir entry"))?;
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Error::io(e, &path, "metadata")),
            };

            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                count = count.saturating_add(1);
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }

    Ok((count, bytes))
}

fn collect_sharded_entries(base: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut entries = Vec::new();
    let prefixes = match fs::read_dir(base) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(entries),
        Err(e) => return Err(Error::io(e, base, "read_dir")),
    };

    for prefix_entry in prefixes {
        let prefix_entry = prefix_entry.map_err(|e| Error::io(e, base, "read_dir entry"))?;
        let prefix_path = prefix_entry.path();
        let meta = prefix_entry
            .metadata()
            .map_err(|e| Error::io(e, &prefix_path, "metadata"))?;
        if !meta.is_dir() {
            continue;
        }
        let Some(prefix_name) = prefix_entry.file_name().to_str().map(str::to_string) else {
            continue;
        };

        for file_entry in
            fs::read_dir(&prefix_path).map_err(|e| Error::io(e, &prefix_path, "read_dir"))?
        {
            let file_entry =
                file_entry.map_err(|e| Error::io(e, &prefix_path, "read_dir entry"))?;
            let file_path = file_entry.path();
            let file_meta = file_entry
                .metadata()
                .map_err(|e| Error::io(e, &file_path, "metadata"))?;
            if !file_meta.is_file() {
                continue;
            }
            let Some(rest_name) = file_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            entries.push((format!("{prefix_name}{rest_name}"), file_path));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| Error::io(e, path, "open"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| Error::io(e, path, "read"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_cache::ActionCache;
    use crate::cas::Cas;
    use crate::merkle::build_output_tree;
    use crate::message::{OutputDirectory, OutputFile};

    #[tokio::test]
    async fn stats_and_verify_succeed_on_healthy_local_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path()).unwrap();
        let ac = LocalActionCache::open(dir.path()).unwrap();

        let blob = cas.put_bytes(b"hello world").await.unwrap();
        let action_digest = Digest::of_bytes(b"action-1");
        let result = ActionResult {
            exit_code: 0,
            output_files: vec![OutputFile {
                path: "out.txt".into(),
                digest: blob.clone(),
                is_executable: false,
            }],
            ..Default::default()
        };
        ac.update(&action_digest, &result).await.unwrap();

        let s = stats(dir.path()).unwrap();
        assert_eq!(s.cas_blobs, 1);
        assert_eq!(s.cas_bytes, 11);
        assert_eq!(s.action_entries, 1);
        assert!(s.action_bytes > 0);

        let report = verify(dir.path()).await.unwrap();
        assert!(report.is_ok(), "expected healthy cache, got {report:?}");
        assert_eq!(report.cas_blobs_checked, 1);
        assert_eq!(report.action_entries_checked, 1);
    }

    #[tokio::test]
    async fn verify_detects_corrupt_blob_and_dangling_action_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path()).unwrap();
        let ac = LocalActionCache::open(dir.path()).unwrap();

        let blob = cas.put_bytes(b"original").await.unwrap();
        let missing = Digest::of_bytes(b"never-stored");
        let action_digest = Digest::of_bytes(b"action-dangling");
        ac.update(
            &action_digest,
            &ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile {
                    path: "missing.txt".into(),
                    digest: missing,
                    is_executable: false,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Corrupt the stored CAS blob on disk.
        let blob_path = cas.blob_path(&blob).unwrap();
        fs::write(&blob_path, b"tampered").unwrap();

        let report = verify(dir.path()).await.unwrap();
        assert!(!report.is_ok());
        assert_eq!(report.corrupt_blobs.len(), 1);
        assert_eq!(report.corrupt_blobs[0].expected_hash, blob.hash);
        assert_eq!(report.invalid_actions.len(), 1);
        assert_eq!(report.invalid_actions[0].action_hash, action_digest.hash);
        assert_eq!(report.invalid_actions[0].missing_blobs.len(), 1);
    }

    #[test]
    fn stats_handles_missing_cache_root_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does-not-exist");
        let s = stats(&nonexistent).unwrap();
        assert_eq!(s.cas_blobs, 0);
        assert_eq!(s.action_entries, 0);
        assert_eq!(s.total_bytes(), 0);
    }

    #[tokio::test]
    async fn prune_preserves_transitive_tree_leaves_and_grace_window_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path()).unwrap();
        let ac = LocalActionCache::open(dir.path()).unwrap();

        // Build a nested output directory tree stored in CAS.
        let out_src = tempfile::tempdir().unwrap();
        fs::create_dir_all(out_src.path().join("sub")).unwrap();
        fs::write(out_src.path().join("sub/leaf.txt"), b"nested leaf content").unwrap();
        let tree_digest = build_output_tree(out_src.path(), &cas).await.unwrap();
        let action_digest = Digest::of_bytes(b"action-with-tree");
        ac.update(
            &action_digest,
            &ActionResult {
                exit_code: 0,
                output_directories: vec![OutputDirectory {
                    path: "dist".into(),
                    tree_digest: tree_digest.clone(),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Write an unreferenced blob simulating an in-flight task whose AC entry is pending.
        let pending_blob = cas.put_bytes(b"in-flight pending output").await.unwrap();

        // With a 10-minute grace window, the pending blob must be protected.
        let protected = prune(dir.path(), PruneOptions::default()).await.unwrap();
        assert_eq!(protected.blobs_removed, 0);
        assert_eq!(protected.protected_recent_blobs, 1);
        assert!(cas.contains(&pending_blob).await.unwrap());

        // With zero grace window, the unreferenced blob is swept while the
        // Tree and its nested leaf blob remain intact.
        let swept = prune(
            dir.path(),
            PruneOptions {
                grace_window: Duration::ZERO,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(swept.blobs_removed, 1);
        assert!(!cas.contains(&pending_blob).await.unwrap());
        assert!(verify(dir.path()).await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn gc_enforces_max_bytes_and_supports_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path()).unwrap();
        let ac = LocalActionCache::open(dir.path()).unwrap();

        let blob1 = cas.put_bytes(&vec![b'a'; 2048]).await.unwrap();
        let blob2 = cas.put_bytes(&vec![b'b'; 2048]).await.unwrap();
        let act1 = Digest::of_bytes(b"act-1");
        let act2 = Digest::of_bytes(b"act-2");

        ac.update(
            &act1,
            &ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile {
                    path: "a.bin".into(),
                    digest: blob1.clone(),
                    is_executable: false,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        ac.update(
            &act2,
            &ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile {
                    path: "b.bin".into(),
                    digest: blob2.clone(),
                    is_executable: false,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let dry = gc(
            dir.path(),
            GcOptions {
                max_bytes: Some(2500),
                max_age: None,
                grace_window: Duration::ZERO,
                dry_run: true,
            },
        )
        .await
        .unwrap();
        assert!(dry.dry_run);
        assert_eq!(dry.actions_removed, 1);
        assert_eq!(dry.blobs_removed, 1);
        // Dry run must not delete anything.
        assert_eq!(stats(dir.path()).unwrap().action_entries, 2);

        let actual = gc(
            dir.path(),
            GcOptions {
                max_bytes: Some(2500),
                max_age: None,
                grace_window: Duration::ZERO,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(actual.actions_removed, 1);
        assert_eq!(actual.blobs_removed, 1);
        let after = stats(dir.path()).unwrap();
        assert_eq!(after.action_entries, 1);
        assert_eq!(after.cas_blobs, 1);
        assert!(verify(dir.path()).await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn verify_and_repair_cleans_corrupt_blobs_and_newly_dangling_actions() {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path()).unwrap();
        let ac = LocalActionCache::open(dir.path()).unwrap();

        let blob = cas.put_bytes(b"good-content").await.unwrap();
        let action_digest = Digest::of_bytes(b"action-referencing-corrupt");
        ac.update(
            &action_digest,
            &ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile {
                    path: "out.txt".into(),
                    digest: blob.clone(),
                    is_executable: false,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Corrupt the blob after writing the action entry.
        fs::write(cas.blob_path(&blob).unwrap(), b"corrupted").unwrap();

        let repair = verify_and_repair(dir.path()).await.unwrap();
        assert_eq!(repair.blobs_removed, 1);
        assert_eq!(repair.actions_removed, 1);
        // Cache must now be completely clean and healthy.
        assert!(verify(dir.path()).await.unwrap().is_ok());
    }
}
