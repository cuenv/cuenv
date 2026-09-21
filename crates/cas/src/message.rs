//! Action / Command / Directory / ActionResult messages.
//!
//! These mirror the equivalent protobuf messages from the Bazel Remote
//! Execution API v2. We use plain serde-serialized structs for now so the
//! crate stays tonic-free; when the remote backend lands we switch to the
//! `bazel-remote-apis` generated types and these become conversion targets.

use crate::digest::Digest;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Execution platform properties (CPU, OS, tooling version, etc.).
///
/// Mirrors `build.bazel.remote.execution.v2.Platform`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// Key/value properties. Stored in a `BTreeMap` for canonical ordering.
    pub properties: BTreeMap<String, String>,
}

/// A command to execute inside an action.
///
/// Mirrors `build.bazel.remote.execution.v2.Command`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Argv vector. `arguments[0]` is the executable.
    pub arguments: Vec<String>,
    /// Environment variables visible to the command.
    pub environment_variables: BTreeMap<String, String>,
    /// Files the action is expected to produce, relative to the working dir.
    pub output_files: Vec<String>,
    /// Directories the action is expected to produce, relative to the working dir.
    pub output_directories: Vec<String>,
    /// Working directory relative to the input root.
    pub working_directory: String,
}

/// A file inside a [`Directory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileNode {
    /// Base file name (no path separators).
    pub name: String,
    /// Content digest in the CAS.
    pub digest: Digest,
    /// Whether the file should be executable when materialized.
    pub is_executable: bool,
}

/// A subdirectory entry inside a [`Directory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryNode {
    /// Base directory name (no path separators).
    pub name: String,
    /// Digest of the sub-`Directory` message in the CAS.
    pub digest: Digest,
}

/// A symlink entry inside a [`Directory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymlinkNode {
    /// Base name of the symlink.
    pub name: String,
    /// Raw symlink target.
    pub target: String,
}

/// A Merkle-tree directory message.
///
/// Mirrors `build.bazel.remote.execution.v2.Directory`. Children are stored
/// in sorted order so the canonical encoding is stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directory {
    /// Files in this directory, sorted by `name`.
    pub files: Vec<FileNode>,
    /// Subdirectories, sorted by `name`.
    pub directories: Vec<DirectoryNode>,
    /// Symlinks, sorted by `name`.
    pub symlinks: Vec<SymlinkNode>,
}

/// A complete output directory tree.
///
/// REAPI stores output directories as one `Tree` blob: `root` is the
/// directory at the declared output path and `children` embeds every
/// descendant directory. `DirectoryNode` digests identify entries in
/// `children`; they are not independent CAS blobs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    /// Root directory at the output path.
    pub root: Directory,
    /// Every non-root directory in the tree.
    pub children: Vec<Directory>,
}

/// Version of cuenv's action execution semantics.
///
/// This is the cache-invalidation salt folded into every [`Action`]. It is
/// deliberately **not** the cuenv package version: keying on the package
/// version invalidates every entry on every release, which is merely
/// annoying for a local cache and unacceptable for a shared remote one.
///
/// Bump this only when a change alters what executing an action *means* —
/// the set of files an action can see, the environment it receives, how
/// outputs are collected, or the canonical encoding used for digests. A bug
/// fix that does not change the result of a correctly-cached action must not
/// bump it.
///
/// History:
///
/// - `1` — initial versioned semantics. Action environment is the declared
///   CUE environment plus explicitly declared host passthrough, replacing
///   the previous implicit merge of ambient host variables.
/// - `2` — canonical encoding moved from `serde_json` to REAPI protobuf, and
///   a command's declared outputs are sorted into REAPI's merged
///   `output_paths`. Every digest changes.
///
/// On the wire this travels in REAPI's `Action.salt`, which exists to place
/// an action into a separate cache namespace without altering what it does.
/// See [`crate::reapi::salt_for`].
pub const ACTION_SEMANTICS_VERSION: u32 = 2;

/// An action to execute.
///
/// Mirrors `build.bazel.remote.execution.v2.Action`. The [`Digest`] of this
/// struct (under canonical encoding) is the action cache key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    /// Digest of the [`Command`] to run.
    pub command_digest: Digest,
    /// Digest of the root [`Directory`] forming the input tree.
    pub input_root_digest: Digest,
    /// Execution platform.
    pub platform: Platform,
    /// Execution-semantics salt. See [`ACTION_SEMANTICS_VERSION`].
    pub action_semantics_version: u32,
}

/// A file produced by an action.
///
/// Mirrors `build.bazel.remote.execution.v2.OutputFile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Path relative to the action's working directory.
    pub path: String,
    /// Content digest.
    pub digest: Digest,
    /// Executable bit.
    pub is_executable: bool,
}

/// A directory produced by an action, stored as a digest of a [`Tree`].
///
/// Mirrors `build.bazel.remote.execution.v2.OutputDirectory`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDirectory {
    /// Path relative to the action's working directory.
    pub path: String,
    /// Digest of the [`Tree`] blob describing the complete output directory.
    pub tree_digest: Digest,
}

/// Metadata captured while the action executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    /// Identifier of the worker that ran the action ("local", hostname, etc).
    pub worker: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
    /// UTC timestamp when the result was recorded.
    pub created_at: DateTime<Utc>,
}

impl Default for ExecutionMetadata {
    fn default() -> Self {
        Self {
            worker: String::new(),
            duration_ms: 0,
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now),
        }
    }
}

/// The result of executing an action.
///
/// Mirrors `build.bazel.remote.execution.v2.ActionResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    /// Output files produced by the action.
    pub output_files: Vec<OutputFile>,
    /// Output directories produced by the action.
    pub output_directories: Vec<OutputDirectory>,
    /// Exit code of the command.
    pub exit_code: i32,
    /// Digest of stdout in the CAS, if captured.
    pub stdout_digest: Option<Digest>,
    /// Digest of stderr in the CAS, if captured.
    pub stderr_digest: Option<Digest>,
    /// Execution metadata.
    pub execution_metadata: ExecutionMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::digest_of;

    #[test]
    fn action_digest_is_order_invariant_on_platform_properties() {
        let mut a_props = BTreeMap::new();
        a_props.insert("os".into(), "linux".into());
        a_props.insert("arch".into(), "x86_64".into());
        let a = Action {
            command_digest: Digest::of_bytes(b"cmd"),
            input_root_digest: Digest::of_bytes(b"root"),
            platform: Platform {
                properties: a_props,
            },
            action_semantics_version: 1,
        };

        let mut b_props = BTreeMap::new();
        // inserted in different order
        b_props.insert("arch".into(), "x86_64".into());
        b_props.insert("os".into(), "linux".into());
        let b = Action {
            platform: Platform {
                properties: b_props,
            },
            ..a.clone()
        };

        assert_eq!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
    }

    #[test]
    fn action_digest_changes_with_command_digest() {
        let base = Action {
            command_digest: Digest::of_bytes(b"cmd-1"),
            input_root_digest: Digest::of_bytes(b"root"),
            platform: Platform::default(),
            action_semantics_version: 1,
        };
        let other = Action {
            command_digest: Digest::of_bytes(b"cmd-2"),
            ..base.clone()
        };
        assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
    }

    #[test]
    fn action_digest_changes_with_input_root() {
        let base = Action {
            command_digest: Digest::of_bytes(b"cmd"),
            input_root_digest: Digest::of_bytes(b"root-1"),
            platform: Platform::default(),
            action_semantics_version: 1,
        };
        let other = Action {
            input_root_digest: Digest::of_bytes(b"root-2"),
            ..base.clone()
        };
        assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
    }

    #[test]
    fn action_digest_changes_with_semantics_version() {
        let base = Action {
            command_digest: Digest::of_bytes(b"cmd"),
            input_root_digest: Digest::of_bytes(b"root"),
            platform: Platform::default(),
            action_semantics_version: 1,
        };
        let other = Action {
            action_semantics_version: 2,
            ..base.clone()
        };
        assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
    }
}
