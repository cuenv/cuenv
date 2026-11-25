//! Flake.lock purity analysis
//!
//! Analyzes flake.lock files to detect unlocked inputs and compute
//! deterministic digests from locked content hashes.

use super::lock::{FlakeLock, FlakeNode, InputRef};
use crate::error::FlakeLockError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Result of flake.lock purity analysis
#[derive(Debug, Clone)]
pub struct PurityAnalysis {
    /// Whether all inputs are properly locked
    pub is_pure: bool,

    /// List of unlocked inputs with reasons
    pub unlocked_inputs: Vec<UnlockedInput>,

    /// Computed digest from locked inputs (for cache key)
    /// Format: "sha256:<hex>"
    pub locked_digest: String,
}

/// An input that is not properly locked
#[derive(Debug, Clone)]
pub struct UnlockedInput {
    /// Input name/path (e.g., "nixpkgs" or "rust-overlay/nixpkgs")
    pub name: String,

    /// Reason why this input is considered unlocked
    pub reason: UnlockReason,
}

/// Reasons why an input may be unlocked
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockReason {
    /// No `locked` section present in the input
    MissingLockedSection,

    /// `locked.narHash` is missing (required for reproducibility)
    MissingNarHash,

    /// Input uses `follows` but the target is unlocked
    FollowsUnlocked {
        /// The target input that is unlocked
        target: String,
    },

    /// Input has a branch `ref` but no pinned `rev`
    UnpinnedReference {
        /// The unpinned reference (e.g., "nixos-unstable")
        reference: String,
    },
}

impl std::fmt::Display for UnlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLockedSection => write!(f, "missing locked section"),
            Self::MissingNarHash => write!(f, "missing narHash"),
            Self::FollowsUnlocked { target } => write!(f, "follows unlocked input '{target}'"),
            Self::UnpinnedReference { reference } => {
                write!(f, "unpinned reference '{reference}'")
            }
        }
    }
}

/// Digest entry for a relative `path` input: its path plus, when the root flake
/// directory is known, a hash of the files it holds.
fn path_input_digest(flake_dir: Option<&Path>, path: &str) -> String {
    let Some(dir) = flake_dir else {
        return format!("path:{path}");
    };
    let mut hasher = Sha256::new();
    hash_tree(&dir.join(path), Path::new(""), &mut hasher);
    format!("path:{path}:{}", hex::encode(hasher.finalize()))
}

/// Directories never part of a flake's evaluated source.
const SKIPPED_DIRS: [&str; 4] = [".git", "node_modules", "target", "result"];

/// Hash every regular file under `dir` in sorted order. Unreadable or missing
/// entries contribute a marker so the digest still changes when they appear.
fn hash_tree(dir: &Path, relative: &Path, hasher: &mut Sha256) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        hasher.update(b"missing\0");
        return;
    };
    let mut entries: Vec<_> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let rel = relative.join(&name);
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !SKIPPED_DIRS.iter().any(|skip| name == *skip) {
                hash_tree(&entry.path(), &rel, hasher);
            }
        } else if kind.is_file() {
            hasher.update(rel.to_string_lossy().as_bytes());
            hasher.update([0u8]);
            if let Ok(bytes) = std::fs::read(entry.path()) {
                hasher.update(Sha256::digest(&bytes));
            }
        }
    }
}

/// Analyzer for flake.lock purity
pub struct FlakeLockAnalyzer {
    lock: FlakeLock,
}

#[derive(Default)]
struct AnalysisState {
    /// Directory of the root flake, used to resolve relative `path` inputs.
    flake_dir: Option<PathBuf>,
    unlocked: Vec<UnlockedInput>,
    hashes: Vec<String>,
    checked: HashSet<String>,
}

impl FlakeLockAnalyzer {
    /// Create analyzer from parsed `FlakeLock`
    #[must_use]
    pub const fn new(lock: FlakeLock) -> Self {
        Self { lock }
    }

    /// Parse and create analyzer from JSON string
    ///
    /// # Errors
    /// Returns an error if the JSON is invalid or doesn't match the schema
    pub fn from_json(json: &str) -> Result<Self, FlakeLockError> {
        let lock = FlakeLock::from_json(json).map_err(|e| FlakeLockError::parse(e.to_string()))?;
        Ok(Self::new(lock))
    }

    /// Parse and create analyzer from file path
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed
    pub fn from_path(path: &Path) -> Result<Self, FlakeLockError> {
        if !path.exists() {
            return Err(FlakeLockError::missing(path));
        }

        let content =
            std::fs::read_to_string(path).map_err(|e| FlakeLockError::io(path, e.to_string()))?;
        Self::from_json(&content)
    }

    /// Analyze the flake.lock for purity
    ///
    /// Returns a `PurityAnalysis` containing:
    /// - Whether all inputs are pure (locked)
    /// - List of unlocked inputs with reasons
    /// - Deterministic digest computed from all locked `narHash` values
    #[must_use]
    pub fn analyze(&self) -> PurityAnalysis {
        self.analyze_inner(None)
    }

    /// Analyze the flake.lock, also folding the contents of relative `path`
    /// inputs (resolved against `flake_dir`, the directory holding the root
    /// `flake.nix`) into the digest.
    ///
    /// Nix pins a relative path input through its parent flake rather than a
    /// `narHash`, so without this a change inside the referenced directory
    /// would leave the digest, and any cache key built from it, unchanged.
    #[must_use]
    pub fn analyze_in(&self, flake_dir: &Path) -> PurityAnalysis {
        self.analyze_inner(Some(flake_dir.to_path_buf()))
    }

    fn analyze_inner(&self, flake_dir: Option<PathBuf>) -> PurityAnalysis {
        let mut state = AnalysisState {
            flake_dir,
            ..AnalysisState::default()
        };

        // Get root node and check all its inputs
        if let Some(root) = self.lock.nodes.get(&self.lock.root) {
            for (input_name, input_ref) in &root.inputs {
                self.check_input(input_name, input_ref, &mut state);
            }
        }

        // Compute deterministic digest from all locked hashes
        let locked_digest = Self::compute_locked_digest(&state.hashes);

        PurityAnalysis {
            is_pure: state.unlocked.is_empty(),
            unlocked_inputs: state.unlocked,
            locked_digest,
        }
    }

    /// Check an input reference recursively
    fn check_input(&self, name: &str, input_ref: &InputRef, state: &mut AnalysisState) {
        match input_ref {
            InputRef::Direct(node_name) => {
                // Skip if already checked (handles cycles)
                if state.checked.contains(node_name) {
                    return;
                }
                state.checked.insert(node_name.clone());

                if let Some(input) = self.lock.nodes.get(node_name) {
                    // Only check input nodes (not root)
                    if input.is_input() {
                        self.check_input_node(name, input, state);
                    }
                }
            }
            InputRef::Follows(path) => {
                // Follows references inherit from another input
                // Resolve the target and check if it's locked
                if let Some(target_name) = path.first()
                    && let Some(target) = self.lock.nodes.get(target_name)
                {
                    // Check if the target is unlocked (has no locked section)
                    if target.is_input() && target.locked.is_none() {
                        state.unlocked.push(UnlockedInput {
                            name: name.to_string(),
                            reason: UnlockReason::FollowsUnlocked {
                                target: target_name.clone(),
                            },
                        });
                    }
                }
            }
        }
    }

    /// Check an input node for purity
    fn check_input_node(&self, input_name: &str, input: &FlakeNode, state: &mut AnalysisState) {
        // Check 1: Missing locked section
        let Some(locked) = &input.locked else {
            state.unlocked.push(UnlockedInput {
                name: input_name.to_string(),
                reason: UnlockReason::MissingLockedSection,
            });
            return;
        };

        // Check 2: Missing narHash (critical for reproducibility).
        // Relative path inputs (`path:./sub`) live inside the parent flake's
        // source tree, so Nix locks them without a narHash; the parent pins them.
        let nar_hash = match (&locked.nar_hash, locked.relative_path()) {
            (Some(hash), _) => hash.clone(),
            (None, Some(path)) => path_input_digest(state.flake_dir.as_deref(), path),
            (None, None) => {
                state.unlocked.push(UnlockedInput {
                    name: input_name.to_string(),
                    reason: UnlockReason::MissingNarHash,
                });
                return;
            }
        };

        // Check 3: Has ref but no rev (unpinned branch reference)
        // This is actually OK in Nix - if narHash exists, it's pinned
        // But we warn if original.ref exists without locked.rev for transparency
        if let Some(original) = &input.original
            && original.reference.is_some()
            && locked.rev.is_none()
        {
            // Only warn if narHash is also missing - if narHash exists, it's still pure
            // Actually, with narHash present, this is fine. Skip this check.
        }

        // Input is properly locked - add hash to list
        state.hashes.push(nar_hash);

        // Recursively check transitive inputs
        for (sub_name, sub_ref) in &input.inputs {
            let full_name = format!("{input_name}/{sub_name}");
            self.check_input(&full_name, sub_ref, state);
        }
    }

    /// Compute a deterministic digest from all locked hashes
    fn compute_locked_digest(input_hashes: &[String]) -> String {
        let mut sha_hasher = Sha256::new();

        // Sort hashes for deterministic ordering
        let mut sorted_hashes = input_hashes.to_vec();
        sorted_hashes.sort();

        for hash in sorted_hashes {
            sha_hasher.update(hash.as_bytes());
            sha_hasher.update([0u8]); // separator
        }

        format!("sha256:{}", hex::encode(sha_hasher.finalize()))
    }

    /// Get the underlying `FlakeLock`
    #[must_use]
    pub const fn lock(&self) -> &FlakeLock {
        &self.lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_minimal_pure() {
        let json = r#"{
            "nodes": {
                "root": { "inputs": {} }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(analysis.is_pure);
        assert!(analysis.unlocked_inputs.is_empty());
    }

    #[test]
    fn test_detect_missing_locked_section() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(!analysis.is_pure);
        assert_eq!(analysis.unlocked_inputs.len(), 1);
        assert_eq!(analysis.unlocked_inputs[0].name, "nixpkgs");
        assert!(matches!(
            analysis.unlocked_inputs[0].reason,
            UnlockReason::MissingLockedSection
        ));
    }

    #[test]
    fn test_relative_path_input_is_locked_without_nar_hash() {
        // Shape written by Nix for `bun.url = "path:../../../flakes/bun"` with
        // `bun.inputs.nixpkgs.follows = "nixpkgs"`.
        let json = r#"{
            "nodes": {
                "bun": {
                    "inputs": { "nixpkgs": ["nixpkgs"] },
                    "locked": { "path": "../../../flakes/bun", "type": "path" },
                    "original": { "path": "../../../flakes/bun", "type": "path" },
                    "parent": []
                },
                "nixpkgs": {
                    "locked": {
                        "lastModified": 1794042290,
                        "narHash": "sha256-ziZn2PvCJE5JEEpqTdxXwJpPB+L3YqoCpTAMCzyaP3U=",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "f4a03c008967841f2a0b3cb7c436fc6fc31671f6",
                        "type": "github"
                    },
                    "original": { "owner": "NixOS", "ref": "nixos-unstable", "repo": "nixpkgs", "type": "github" }
                },
                "root": { "inputs": { "bun": "bun", "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analysis = FlakeLockAnalyzer::from_json(json).unwrap().analyze();

        assert!(analysis.is_pure, "{:?}", analysis.unlocked_inputs);
        assert!(analysis.locked_digest.starts_with("sha256:"));
    }

    #[test]
    fn test_relative_path_input_contents_change_the_digest() {
        let json = r#"{
            "nodes": {
                "bun": {
                    "locked": { "path": "../flakes/bun", "type": "path" },
                    "original": { "path": "../flakes/bun", "type": "path" },
                    "parent": []
                },
                "root": { "inputs": { "bun": "bun" } }
            },
            "root": "root",
            "version": 7
        }"#;
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let shared = tmp.path().join("flakes/bun");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("flake.nix"), "{ a = 1; }").unwrap();

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let before = analyzer.analyze_in(&project).locked_digest;
        assert_eq!(before, analyzer.analyze_in(&project).locked_digest);

        std::fs::write(shared.join("flake.nix"), "{ a = 2; }").unwrap();
        let after = analyzer.analyze_in(&project).locked_digest;
        assert_ne!(before, after);
    }

    #[test]
    fn test_absolute_path_input_without_nar_hash_is_unlocked() {
        let json = r#"{
            "nodes": {
                "local": { "locked": { "type": "path", "path": "/abs/sub" } },
                "root": { "inputs": { "local": "local" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analysis = FlakeLockAnalyzer::from_json(json).unwrap().analyze();

        assert!(!analysis.is_pure);
    }

    #[test]
    fn test_detect_missing_nar_hash() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123"
                    },
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(!analysis.is_pure);
        assert_eq!(analysis.unlocked_inputs.len(), 1);
        assert!(matches!(
            analysis.unlocked_inputs[0].reason,
            UnlockReason::MissingNarHash
        ));
    }

    #[test]
    fn test_fully_locked_is_pure() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123",
                        "narHash": "sha256-xxxxxxxxxxxxx"
                    },
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(analysis.is_pure);
        assert!(analysis.unlocked_inputs.is_empty());
        assert!(analysis.locked_digest.starts_with("sha256:"));
    }

    #[test]
    fn test_follows_unlocked_target() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "rust-overlay": {
                    "inputs": { "nixpkgs": ["nixpkgs"] },
                    "locked": {
                        "type": "github",
                        "owner": "oxalica",
                        "repo": "rust-overlay",
                        "rev": "def456",
                        "narHash": "sha256-yyyyyyyyy"
                    }
                },
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "rust-overlay": "rust-overlay"
                    }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(!analysis.is_pure);
        // Should detect both: nixpkgs is unlocked, and rust-overlay follows unlocked nixpkgs
        assert!(analysis.unlocked_inputs.iter().any(|u| u.name == "nixpkgs"));
    }

    #[test]
    fn test_digest_determinism() {
        let json = r#"{
            "nodes": {
                "a": {
                    "locked": { "type": "github", "narHash": "sha256-aaa" }
                },
                "b": {
                    "locked": { "type": "github", "narHash": "sha256-bbb" }
                },
                "root": {
                    "inputs": { "a": "a", "b": "b" }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer1 = FlakeLockAnalyzer::from_json(json).unwrap();
        let analyzer2 = FlakeLockAnalyzer::from_json(json).unwrap();

        let analysis1 = analyzer1.analyze();
        let analysis2 = analyzer2.analyze();

        assert_eq!(analysis1.locked_digest, analysis2.locked_digest);
    }

    #[test]
    fn test_digest_changes_with_different_hashes() {
        let json1 = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": { "type": "github", "narHash": "sha256-version1" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let json2 = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": { "type": "github", "narHash": "sha256-version2" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let analysis1 = FlakeLockAnalyzer::from_json(json1).unwrap().analyze();
        let analysis2 = FlakeLockAnalyzer::from_json(json2).unwrap().analyze();

        assert_ne!(analysis1.locked_digest, analysis2.locked_digest);
    }

    #[test]
    fn test_malformed_json_error() {
        let result = FlakeLockAnalyzer::from_json("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_inputs_all_locked() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": { "type": "github", "narHash": "sha256-aaa" }
                },
                "crane": {
                    "locked": { "type": "github", "narHash": "sha256-bbb" }
                },
                "flake-utils": {
                    "locked": { "type": "github", "narHash": "sha256-ccc" }
                },
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "crane": "crane",
                        "flake-utils": "flake-utils"
                    }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let analyzer = FlakeLockAnalyzer::from_json(json).unwrap();
        let analysis = analyzer.analyze();

        assert!(analysis.is_pure);
        assert!(analysis.locked_digest.starts_with("sha256:"));
    }

    #[test]
    fn test_real_project_flake_lock() {
        // Test with the actual project's flake.lock (if it exists)
        let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("flake.lock");

        if lock_path.exists() {
            let analyzer = FlakeLockAnalyzer::from_path(&lock_path).unwrap();
            let analysis = analyzer.analyze();

            // The project's flake.lock should be fully locked
            assert!(
                analysis.is_pure,
                "Project flake.lock has unlocked inputs: {:?}",
                analysis.unlocked_inputs
            );
            assert!(analysis.locked_digest.starts_with("sha256:"));

            // Verify the digest is deterministic
            let analysis2 = FlakeLockAnalyzer::from_path(&lock_path).unwrap().analyze();
            assert_eq!(analysis.locked_digest, analysis2.locked_digest);
        }
    }
}
