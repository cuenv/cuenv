//! Property-based tests for cache key stability and invalidation behaviors.
//!
//! These tests verify the behavioral contracts of the caching system:
//! - Determinism: Same inputs always produce the same cache key
//! - Sensitivity: Different inputs produce different cache keys
//! - Order invariance: Map ordering doesn't affect the cache key

use cuenv_core::cache::tasks::{compute_cache_key, CacheKeyEnvelope};
use proptest::prelude::*;
use std::collections::BTreeMap;

// =============================================================================
// Strategies for generating test data
// =============================================================================

/// Generate valid task/command names (alphanumeric + common separators)
fn command_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,20}".prop_map(String::from)
}

/// Generate valid file paths for inputs
fn input_path_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("src/**/*.rs".to_string()),
        Just("Cargo.toml".to_string()),
        Just("Cargo.lock".to_string()),
        Just("*.json".to_string()),
        "[a-z]{1,10}/[a-z]{1,10}\\.[a-z]{1,4}".prop_map(String::from),
    ]
}

/// Generate a SHA256-like hash string
fn hash_strategy() -> impl Strategy<Value = String> {
    "[a-f0-9]{64}".prop_map(String::from)
}

/// Generate environment variable names
fn env_var_name_strategy() -> impl Strategy<Value = String> {
    "[A-Z][A-Z0-9_]{0,15}".prop_map(String::from)
}

/// Generate a valid CacheKeyEnvelope
fn envelope_strategy() -> impl Strategy<Value = CacheKeyEnvelope> {
    (
        prop::collection::btree_map(input_path_strategy(), hash_strategy(), 0..5),
        command_strategy(),
        prop::collection::vec(command_strategy(), 0..3),
        prop::collection::btree_map(env_var_name_strategy(), "[a-z0-9]{1,10}".prop_map(String::from), 0..3),
        "[0-9]+\\.[0-9]+\\.[0-9]+".prop_map(String::from), // version
        prop_oneof![
            Just("linux-x86_64".to_string()),
            Just("darwin-aarch64".to_string()),
            Just("windows-x86_64".to_string()),
        ],
    )
        .prop_map(|(inputs, command, args, env, cuenv_version, platform)| {
            CacheKeyEnvelope {
                inputs,
                command,
                args,
                shell: None,
                env,
                cuenv_version,
                platform,
                workspace_lockfile_hashes: None,
                workspace_package_hashes: None,
            }
        })
}

// =============================================================================
// Property Tests: Determinism
// =============================================================================

proptest! {
    /// Contract: Same envelope always produces the same cache key
    ///
    /// This is critical for cache correctness - if the same inputs
    /// produce different keys, we'd never get cache hits.
    #[test]
    fn cache_key_is_deterministic(envelope in envelope_strategy()) {
        let (key1, _json1) = compute_cache_key(&envelope)
            .expect("compute_cache_key should succeed");
        let (key2, _json2) = compute_cache_key(&envelope)
            .expect("compute_cache_key should succeed on second call");

        prop_assert_eq!(
            key1, key2,
            "Same envelope must produce identical cache keys"
        );
    }

    /// Contract: Cloned envelope produces the same key as original
    #[test]
    fn cache_key_stable_across_clone(envelope in envelope_strategy()) {
        let cloned = envelope.clone();

        let (key1, _) = compute_cache_key(&envelope)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&cloned)
            .expect("compute_cache_key should succeed for clone");

        prop_assert_eq!(
            key1, key2,
            "Cloned envelope must produce same cache key"
        );
    }
}

// =============================================================================
// Property Tests: Sensitivity (cache invalidation)
// =============================================================================

proptest! {
    /// Contract: Changing the command produces a different cache key
    ///
    /// If the command changes, we must invalidate the cache.
    #[test]
    fn different_command_produces_different_key(
        base in envelope_strategy(),
        new_command in command_strategy().prop_filter("must differ", |c| !c.is_empty())
    ) {
        // Skip if commands happen to be the same
        prop_assume!(base.command != new_command);

        let mut modified = base.clone();
        modified.command = new_command;

        let (key1, _) = compute_cache_key(&base)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&modified)
            .expect("compute_cache_key should succeed for modified");

        prop_assert_ne!(
            key1, key2,
            "Different command must produce different cache key"
        );
    }

    /// Contract: Changing input file hashes produces a different cache key
    ///
    /// If an input file changes, we must invalidate the cache.
    #[test]
    fn different_input_hash_produces_different_key(
        base in envelope_strategy(),
        new_path in input_path_strategy(),
        new_hash in hash_strategy(),
    ) {
        // Only test if we're actually changing something
        let original_hash = base.inputs.get(&new_path).cloned();
        prop_assume!(original_hash.as_ref() != Some(&new_hash));

        let mut modified = base.clone();
        modified.inputs.insert(new_path, new_hash);

        let (key1, _) = compute_cache_key(&base)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&modified)
            .expect("compute_cache_key should succeed for modified");

        prop_assert_ne!(
            key1, key2,
            "Different input hashes must produce different cache key"
        );
    }

    /// Contract: Changing environment variables produces a different cache key
    ///
    /// Environment variables affect task execution, so cache must be invalidated.
    #[test]
    fn different_env_produces_different_key(
        base in envelope_strategy(),
        env_name in env_var_name_strategy(),
        env_value in "[a-z0-9]{1,10}".prop_map(String::from),
    ) {
        // Only test if we're actually changing something
        let original_value = base.env.get(&env_name).cloned();
        prop_assume!(original_value.as_ref() != Some(&env_value));

        let mut modified = base.clone();
        modified.env.insert(env_name, env_value);

        let (key1, _) = compute_cache_key(&base)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&modified)
            .expect("compute_cache_key should succeed for modified");

        prop_assert_ne!(
            key1, key2,
            "Different environment variables must produce different cache key"
        );
    }

    /// Contract: Changing platform produces a different cache key
    ///
    /// Platform-specific builds must not share cache keys.
    #[test]
    fn different_platform_produces_different_key(
        base in envelope_strategy(),
    ) {
        let new_platform = if base.platform == "linux-x86_64" {
            "darwin-aarch64"
        } else {
            "linux-x86_64"
        };

        let mut modified = base.clone();
        modified.platform = new_platform.to_string();

        let (key1, _) = compute_cache_key(&base)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&modified)
            .expect("compute_cache_key should succeed for modified");

        prop_assert_ne!(
            key1, key2,
            "Different platform must produce different cache key"
        );
    }

    /// Contract: Changing cuenv version produces a different cache key
    ///
    /// Version changes may affect task execution behavior.
    #[test]
    fn different_version_produces_different_key(
        base in envelope_strategy(),
        new_version in "[0-9]+\\.[0-9]+\\.[0-9]+".prop_map(String::from),
    ) {
        prop_assume!(base.cuenv_version != new_version);

        let mut modified = base.clone();
        modified.cuenv_version = new_version;

        let (key1, _) = compute_cache_key(&base)
            .expect("compute_cache_key should succeed");
        let (key2, _) = compute_cache_key(&modified)
            .expect("compute_cache_key should succeed for modified");

        prop_assert_ne!(
            key1, key2,
            "Different cuenv version must produce different cache key"
        );
    }
}

// =============================================================================
// Property Tests: Order Invariance
// =============================================================================

proptest! {
    /// Contract: BTreeMap ordering ensures deterministic serialization
    ///
    /// Inputs and env vars are stored in BTreeMap, which has deterministic
    /// iteration order. This test verifies that property holds by using
    /// unique keys to avoid overwrite complications.
    #[test]
    fn btreemap_order_is_deterministic(
        keys in prop::collection::hash_set("[a-z]{1,5}".prop_map(String::from), 2..5),
        values in prop::collection::vec("[a-z]{1,5}".prop_map(String::from), 2..5),
    ) {
        let keys: Vec<_> = keys.into_iter().collect();
        prop_assume!(keys.len() <= values.len());
        let values: Vec<_> = values.into_iter().take(keys.len()).collect();

        // Insert in forward order
        let mut map1: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in keys.iter().zip(values.iter()) {
            map1.insert(k.clone(), v.clone());
        }

        // Insert in reverse order
        let mut map2: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in keys.iter().rev().zip(values.iter().rev()) {
            map2.insert(k.clone(), v.clone());
        }

        // Both maps should have same content and iteration order (sorted by key)
        let vec1: Vec<_> = map1.iter().collect();
        let vec2: Vec<_> = map2.iter().collect();

        prop_assert_eq!(vec1, vec2, "BTreeMap iteration order must be deterministic");
    }
}

// =============================================================================
// Behavioral Tests (non-proptest)
// =============================================================================

#[test]
fn cache_key_format_is_valid_hex() {
    let envelope = CacheKeyEnvelope {
        inputs: BTreeMap::from([("file.txt".to_string(), "abc123".to_string())]),
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "1.0.0".to_string(),
        platform: "linux-x86_64".to_string(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key, _) = compute_cache_key(&envelope).expect("should compute key");

    // Key should be 64 hex characters (SHA256)
    assert_eq!(key.len(), 64, "Cache key should be 64 hex characters");
    assert!(
        key.chars().all(|c| c.is_ascii_hexdigit()),
        "Cache key should contain only hex characters"
    );
}

#[test]
fn empty_envelope_produces_valid_key() {
    let envelope = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: String::new(),
        args: vec![],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: String::new(),
        platform: String::new(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let result = compute_cache_key(&envelope);
    assert!(result.is_ok(), "Empty envelope should produce valid key");

    let (key, _) = result.unwrap();
    assert_eq!(key.len(), 64, "Empty envelope key should still be 64 chars");
}

#[test]
fn workspace_hashes_affect_cache_key() {
    let base = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: "npm".to_string(),
        args: vec!["install".to_string()],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "1.0.0".to_string(),
        platform: "linux-x86_64".to_string(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let mut with_lockfile = base.clone();
    with_lockfile.workspace_lockfile_hashes = Some(BTreeMap::from([(
        "npm".to_string(),
        "lockfile_hash_abc123".to_string(),
    )]));

    let (key1, _) = compute_cache_key(&base).unwrap();
    let (key2, _) = compute_cache_key(&with_lockfile).unwrap();

    assert_ne!(
        key1, key2,
        "Adding workspace lockfile hashes should change cache key"
    );
}

#[test]
fn args_order_matters_for_cache_key() {
    let envelope1 = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: "cmd".to_string(),
        args: vec!["a".to_string(), "b".to_string()],
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "1.0.0".to_string(),
        platform: "linux".to_string(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let envelope2 = CacheKeyEnvelope {
        inputs: BTreeMap::new(),
        command: "cmd".to_string(),
        args: vec!["b".to_string(), "a".to_string()], // Different order
        shell: None,
        env: BTreeMap::new(),
        cuenv_version: "1.0.0".to_string(),
        platform: "linux".to_string(),
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };

    let (key1, _) = compute_cache_key(&envelope1).unwrap();
    let (key2, _) = compute_cache_key(&envelope2).unwrap();

    assert_ne!(
        key1, key2,
        "Different argument order should produce different cache keys"
    );
}
