//! Property tests for [`digest_of::<Action>`].
//!
//! These encode the behavioral contract of the action cache key:
//!
//! - **Determinism**: the same `Action` always digests to the same value.
//! - **Sensitivity**: changing any component of the `Action` changes the digest.
//! - **Order invariance**: `BTreeMap`-backed fields (platform properties,
//!   command env vars) are insertion-order independent.
//!
//! These properties replace the old `CacheKeyEnvelope` proptests from the
//! legacy `cuenv_cache` crate.

use cuenv_cas::{Action, Command, Digest, Platform, digest_of};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn digest_strategy() -> impl Strategy<Value = Digest> {
    ("[a-f0-9]{64}", 0u64..10_000).prop_map(|(hash, size_bytes)| Digest { hash, size_bytes })
}

fn env_key_strategy() -> impl Strategy<Value = String> {
    "[A-Z][A-Z0-9_]{0,15}".prop_map(String::from)
}

fn platform_strategy() -> impl Strategy<Value = Platform> {
    prop::collection::btree_map(
        "[a-z][a-z0-9_-]{0,10}".prop_map(String::from),
        "[a-z0-9_-]{0,10}".prop_map(String::from),
        0..4,
    )
    .prop_map(|properties| Platform { properties })
}

fn action_strategy() -> impl Strategy<Value = Action> {
    (
        digest_strategy(),
        digest_strategy(),
        platform_strategy(),
        "[0-9]+\\.[0-9]+\\.[0-9]+".prop_map(String::from),
    )
        .prop_map(
            |(command_digest, input_root_digest, platform, cuenv_version)| Action {
                command_digest,
                input_root_digest,
                platform,
                cuenv_version,
            },
        )
}

proptest! {
    /// Same action ⇒ same digest, always.
    #[test]
    fn digest_is_deterministic(action in action_strategy()) {
        let d1 = digest_of(&action).expect("digest_of");
        let d2 = digest_of(&action).expect("digest_of");
        prop_assert_eq!(d1, d2);
    }

    /// Cloning must not change the digest.
    #[test]
    fn digest_stable_across_clone(action in action_strategy()) {
        let clone = action.clone();
        let d1 = digest_of(&action).expect("digest_of");
        let d2 = digest_of(&clone).expect("digest_of");
        prop_assert_eq!(d1, d2);
    }

    /// Different command digest ⇒ different action digest.
    #[test]
    fn command_digest_sensitivity(mut action in action_strategy(), other in digest_strategy()) {
        prop_assume!(action.command_digest != other);
        let original = digest_of(&action).expect("digest_of");
        action.command_digest = other;
        let modified = digest_of(&action).expect("digest_of");
        prop_assert_ne!(original, modified);
    }

    /// Different input root ⇒ different action digest.
    #[test]
    fn input_root_sensitivity(mut action in action_strategy(), other in digest_strategy()) {
        prop_assume!(action.input_root_digest != other);
        let original = digest_of(&action).expect("digest_of");
        action.input_root_digest = other;
        let modified = digest_of(&action).expect("digest_of");
        prop_assert_ne!(original, modified);
    }

    /// Different cuenv_version ⇒ different action digest.
    #[test]
    fn cuenv_version_sensitivity(
        mut action in action_strategy(),
        other in "[0-9]+\\.[0-9]+\\.[0-9]+".prop_map(String::from),
    ) {
        prop_assume!(action.cuenv_version != other);
        let original = digest_of(&action).expect("digest_of");
        action.cuenv_version = other;
        let modified = digest_of(&action).expect("digest_of");
        prop_assert_ne!(original, modified);
    }
}

// =============================================================================
// Order invariance (BTreeMap guarantees this, but assert explicitly)
// =============================================================================

proptest! {
    #[test]
    fn platform_property_order_invariant(
        pairs in prop::collection::btree_map(
            "[a-z]{1,5}".prop_map(String::from),
            "[a-z0-9]{0,5}".prop_map(String::from),
            0..6,
        )
    ) {
        // BTreeMap iterates in sorted order regardless of insertion order;
        // to exercise "different insertion orders ⇒ same digest" we rebuild
        // the map from the pairs in reverse order.
        let forward: BTreeMap<String, String> = pairs.clone();
        let reverse: BTreeMap<String, String> =
            pairs.into_iter().rev().collect();

        let base = Action {
            command_digest: Digest::of_bytes(b"cmd"),
            input_root_digest: Digest::of_bytes(b"root"),
            platform: Platform { properties: BTreeMap::new() },
            cuenv_version: "0.30.8".into(),
        };
        let mut a = base.clone();
        a.platform = Platform { properties: forward };
        let mut b = base;
        b.platform = Platform { properties: reverse };

        prop_assert_eq!(
            digest_of(&a).expect("digest_of"),
            digest_of(&b).expect("digest_of"),
        );
    }

    /// Command environment ordering must not affect the command digest.
    #[test]
    fn command_env_order_invariant(
        pairs in prop::collection::btree_map(
            env_key_strategy(),
            "[a-z0-9]{0,5}".prop_map(String::from),
            0..6,
        )
    ) {
        let forward: BTreeMap<String, String> = pairs.clone();
        let reverse: BTreeMap<String, String> =
            pairs.into_iter().rev().collect();

        let a = Command {
            arguments: vec!["cargo".into(), "build".into()],
            environment_variables: forward,
            output_files: vec![],
            output_directories: vec![],
            working_directory: String::new(),
        };
        let b = Command {
            environment_variables: reverse,
            ..a.clone()
        };

        prop_assert_eq!(
            digest_of(&a).expect("digest_of"),
            digest_of(&b).expect("digest_of"),
        );
    }
}

// =============================================================================
// Explicit format checks
// =============================================================================

#[test]
fn digest_hash_is_64_hex_chars() {
    let action = Action {
        command_digest: Digest::of_bytes(b"c"),
        input_root_digest: Digest::of_bytes(b"r"),
        platform: Platform::default(),
        cuenv_version: "0.30.8".into(),
    };
    let d = digest_of(&action).expect("digest_of");
    assert_eq!(d.hash.len(), 64);
    assert!(d.hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn empty_action_still_produces_valid_digest() {
    let action = Action {
        command_digest: Digest::of_bytes(b""),
        input_root_digest: Digest::of_bytes(b""),
        platform: Platform::default(),
        cuenv_version: String::new(),
    };
    let d = digest_of(&action).expect("digest_of");
    assert_eq!(d.hash.len(), 64);
}

#[test]
fn command_argument_order_matters() {
    let a = Command {
        arguments: vec!["build".into(), "--release".into()],
        environment_variables: BTreeMap::new(),
        output_files: vec![],
        output_directories: vec![],
        working_directory: String::new(),
    };
    let b = Command {
        arguments: vec!["--release".into(), "build".into()],
        ..a.clone()
    };
    assert_ne!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
}
