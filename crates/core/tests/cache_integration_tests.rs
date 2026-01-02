//! Integration tests for CAS-based task caching
//!
//! Tests cache behavior across different scenarios including cross-branch
//! sharing, cache invalidation, and concurrent access.

use cuenv_core::cache::cas::CasStore;
use cuenv_core::cache::gc::{gc, GcPolicy};
use cuenv_core::cache::tasks::{
    cas_stats, cas_store, compute_cache_key, lookup, materialize_outputs, record_latest,
    save_result, CacheKeyEnvelope, OutputIndexEntry, TaskLogs, TaskResultMeta,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

#[test]
fn test_cross_branch_deduplication() {
    // Simulate two different branches producing the same output
    let cache_tmp = TempDir::new().unwrap();

    // Branch A produces output
    let outputs_a = TempDir::new().unwrap();
    std::fs::write(outputs_a.path().join("lib.so"), b"shared library content").unwrap();

    let meta_a = TaskResultMeta {
        task_name: "build".into(),
        command: "cargo".into(),
        args: vec!["build".into()],
        env_summary: BTreeMap::new(),
        inputs_summary: BTreeMap::new(),
        created_at: chrono::Utc::now(),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        duration_ms: 1000,
        exit_code: 0,
        cache_key_envelope: serde_json::json!({}),
        output_index: vec![],
    };

    let herm_a = TempDir::new().unwrap();
    save_result(
        "branch-a-key",
        &meta_a,
        outputs_a.path(),
        herm_a.path(),
        TaskLogs {
            stdout: None,
            stderr: None,
        },
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Branch B produces identical output (different cache key, same content)
    let outputs_b = TempDir::new().unwrap();
    std::fs::write(outputs_b.path().join("lib.so"), b"shared library content").unwrap();

    let meta_b = meta_a.clone();
    let herm_b = TempDir::new().unwrap();
    save_result(
        "branch-b-key",
        &meta_b,
        outputs_b.path(),
        herm_b.path(),
        TaskLogs {
            stdout: None,
            stderr: None,
        },
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Verify that CAS only stored the blob once (deduplication)
    let store = cas_store(Some(cache_tmp.path())).unwrap();
    let blobs = store.list().unwrap();

    // Should have only 1 unique blob despite 2 cache entries
    assert_eq!(
        blobs.len(),
        1,
        "CAS should deduplicate identical content across branches"
    );

    // Both branches can restore the output
    let dest_a = TempDir::new().unwrap();
    let count_a = materialize_outputs("branch-a-key", dest_a.path(), Some(cache_tmp.path()))
        .unwrap();
    assert_eq!(count_a, 1);

    let dest_b = TempDir::new().unwrap();
    let count_b = materialize_outputs("branch-b-key", dest_b.path(), Some(cache_tmp.path()))
        .unwrap();
    assert_eq!(count_b, 1);

    // Content should be identical
    assert_eq!(
        std::fs::read(dest_a.path().join("lib.so")).unwrap(),
        std::fs::read(dest_b.path().join("lib.so")).unwrap()
    );
}

#[test]
fn test_cache_invalidation_on_input_change() {
    // Test that changing inputs invalidates the cache
    let cache_tmp = TempDir::new().unwrap();

    // Create envelope with specific inputs
    let mut inputs_v1 = BTreeMap::new();
    inputs_v1.insert("src/main.rs".to_string(), "hash_v1".to_string());

    let envelope_v1 = CacheKeyEnvelope {
        inputs: inputs_v1.clone(),
        command: "cargo".into(),
        args: vec!["build".into()],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key_v1, _) = compute_cache_key(&envelope_v1).unwrap();

    // Create envelope with changed inputs
    let mut inputs_v2 = BTreeMap::new();
    inputs_v2.insert("src/main.rs".to_string(), "hash_v2".to_string());

    let envelope_v2 = CacheKeyEnvelope {
        inputs: inputs_v2,
        command: "cargo".into(),
        args: vec!["build".into()],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key_v2, _) = compute_cache_key(&envelope_v2).unwrap();

    // Keys should be different
    assert_ne!(
        key_v1, key_v2,
        "Cache keys should differ when inputs change"
    );

    // Store result for v1
    let outputs = TempDir::new().unwrap();
    std::fs::write(outputs.path().join("output.txt"), b"v1 output").unwrap();

    let meta = TaskResultMeta {
        task_name: "build".into(),
        command: "cargo".into(),
        args: vec!["build".into()],
        env_summary: BTreeMap::new(),
        inputs_summary: inputs_v1,
        created_at: chrono::Utc::now(),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        duration_ms: 1000,
        exit_code: 0,
        cache_key_envelope: serde_json::json!({}),
        output_index: vec![],
    };

    let herm = TempDir::new().unwrap();
    save_result(
        &key_v1,
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

    // v1 should be cached
    assert!(lookup(&key_v1, Some(cache_tmp.path())).is_some());

    // v2 should not be cached
    assert!(lookup(&key_v2, Some(cache_tmp.path())).is_none());
}

#[test]
fn test_cache_invalidation_on_env_change() {
    // Test that changing environment variables invalidates the cache
    let mut env_v1 = BTreeMap::new();
    env_v1.insert("RUSTFLAGS".to_string(), "-C target-cpu=native".to_string());

    let envelope_v1 = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: "cargo".into(),
        args: vec!["build".into()],
        shell: None,
        env: env_v1,
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key_v1, _) = compute_cache_key(&envelope_v1).unwrap();

    // Change environment
    let mut env_v2 = BTreeMap::new();
    env_v2.insert("RUSTFLAGS".to_string(), "-C opt-level=3".to_string());

    let envelope_v2 = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: "cargo".into(),
        args: vec!["build".into()],
        shell: None,
        env: env_v2,
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key_v2, _) = compute_cache_key(&envelope_v2).unwrap();

    // Keys should be different
    assert_ne!(
        key_v1, key_v2,
        "Cache keys should differ when environment changes"
    );
}

#[test]
fn test_cache_shares_across_branches() {
    // Test that cache is accessible from different "branches" (different working dirs)
    let cache_tmp = TempDir::new().unwrap();

    // Branch 1: Store result
    let project_root_1 = TempDir::new().unwrap();
    let outputs_1 = TempDir::new().unwrap();
    std::fs::write(outputs_1.path().join("artifact.bin"), b"shared artifact").unwrap();

    let meta = TaskResultMeta {
        task_name: "build".into(),
        command: "make".into(),
        args: vec![],
        env_summary: BTreeMap::new(),
        inputs_summary: BTreeMap::new(),
        created_at: chrono::Utc::now(),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        duration_ms: 1000,
        exit_code: 0,
        cache_key_envelope: serde_json::json!({}),
        output_index: vec![],
    };

    let herm_1 = TempDir::new().unwrap();
    let cache_key = "shared-cache-key-123";
    save_result(
        cache_key,
        &meta,
        outputs_1.path(),
        herm_1.path(),
        TaskLogs {
            stdout: None,
            stderr: None,
        },
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Record as latest for branch 1
    record_latest(
        project_root_1.path(),
        "build",
        cache_key,
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Branch 2: Access the same cache
    let project_root_2 = TempDir::new().unwrap();
    let dest_2 = TempDir::new().unwrap();

    // Should be able to restore from cache
    let count = materialize_outputs(cache_key, dest_2.path(), Some(cache_tmp.path())).unwrap();
    assert_eq!(count, 1);

    // Content should match
    assert_eq!(
        std::fs::read(dest_2.path().join("artifact.bin")).unwrap(),
        b"shared artifact"
    );

    // Record as latest for branch 2 (different project)
    record_latest(
        project_root_2.path(),
        "build",
        cache_key,
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Both projects should have the same cache key recorded
    let keys_1 = cuenv_core::cache::tasks::get_project_cache_keys(
        project_root_1.path(),
        Some(cache_tmp.path()),
    )
    .unwrap()
    .unwrap();
    let keys_2 = cuenv_core::cache::tasks::get_project_cache_keys(
        project_root_2.path(),
        Some(cache_tmp.path()),
    )
    .unwrap()
    .unwrap();

    assert_eq!(keys_1.get("build"), Some(&cache_key.to_string()));
    assert_eq!(keys_2.get("build"), Some(&cache_key.to_string()));
}

#[test]
fn test_gc_preserves_latest_entries() {
    use chrono::Duration;

    let cache_tmp = TempDir::new().unwrap();
    let project_root = TempDir::new().unwrap();

    // Create old entry
    let outputs = TempDir::new().unwrap();
    let herm = TempDir::new().unwrap();

    let old_meta = TaskResultMeta {
        task_name: "build".into(),
        command: "make".into(),
        args: vec![],
        env_summary: BTreeMap::new(),
        inputs_summary: BTreeMap::new(),
        created_at: chrono::Utc::now() - Duration::days(60),
        cuenv_version: "0.21.0".into(),
        platform: "linux".into(),
        duration_ms: 1000,
        exit_code: 0,
        cache_key_envelope: serde_json::json!({}),
        output_index: vec![],
    };

    let old_key = "old-entry-key";
    save_result(
        old_key,
        &old_meta,
        outputs.path(),
        herm.path(),
        TaskLogs {
            stdout: None,
            stderr: None,
        },
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Mark as latest (should be protected)
    record_latest(
        project_root.path(),
        "build",
        old_key,
        Some(cache_tmp.path()),
    )
    .unwrap();

    // Run GC with aggressive policy
    let policy = GcPolicy {
        max_age_days: Some(30),
        max_size_bytes: None,
        min_entries_per_task: 1,
    };

    let result = gc(Some(cache_tmp.path()), &policy).unwrap();

    // Old entry should be preserved because it's marked as latest
    assert!(lookup(old_key, Some(cache_tmp.path())).is_some());
    assert_eq!(result.entries_removed, 0);
}

#[test]
fn test_cas_stats() {
    let cache_tmp = TempDir::new().unwrap();

    // Store some data
    let outputs = TempDir::new().unwrap();
    std::fs::write(outputs.path().join("file1.txt"), b"content 1").unwrap();
    std::fs::write(outputs.path().join("file2.txt"), b"content 2 is longer").unwrap();

    let meta = TaskResultMeta {
        task_name: "test".into(),
        command: "echo".into(),
        args: vec![],
        env_summary: BTreeMap::new(),
        inputs_summary: BTreeMap::new(),
        created_at: chrono::Utc::now(),
        cuenv_version: "0.21.0".into(),
        platform: "test".into(),
        duration_ms: 1,
        exit_code: 0,
        cache_key_envelope: serde_json::json!({}),
        output_index: vec![],
    };

    let herm = TempDir::new().unwrap();
    save_result(
        "test-key",
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

    // Get stats
    let stats = cas_stats(Some(cache_tmp.path())).unwrap();

    assert_eq!(stats.blob_count, 2);
    assert!(stats.total_size > 0);
    assert!(!stats.human_size.is_empty());
}
