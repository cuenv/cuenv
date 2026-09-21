//! Conversions between cuenv's message types and the Bazel Remote Execution
//! API v2 protobuf types.
//!
//! This module is the single place where cuenv's shapes meet the wire format.
//! Everything cuenv stores in the CAS or the action cache, and everything it
//! will later send to a remote server, is encoded here.
//!
//! # Why protobuf is the canonical form
//!
//! A digest is only meaningful relative to an encoding. REAPI defines a blob's
//! name as the SHA-256 of its **protobuf** serialization, and servers act on
//! that: a CAS verifies `digest == sha256(bytes)` before accepting a blob, and
//! an action cache parses the `ActionResult` it is handed in order to validate
//! and reference-count the blobs it names. JSON-encoded messages would be
//! opaque garbage to every one of them.
//!
//! So cuenv digests protobuf bytes, which means a cuenv local cache is already
//! byte-compatible with bazel-remote, buildbarn, BuildBuddy, NativeLink,
//! EngFlow and Namespace before any network code exists.
//!
//! # Canonical ordering
//!
//! Protobuf itself is not canonical — the same message can have several valid
//! encodings — so REAPI pins down the parts that matter, and cuenv must
//! respect that or two identical actions will hash differently:
//!
//! - `Command.environment_variables` sorted by name.
//! - `Command.output_paths` sorted lexicographically.
//! - `Platform.properties` sorted by name, then value.
//! - `Directory.files` / `.directories` / `.symlinks` sorted by name.
//!
//! cuenv holds these in `BTreeMap`s, which sorts the first three for free;
//! output paths come from user configuration in whatever order the user wrote
//! them, so they are sorted explicitly on the way out.
//!
//! # What is deliberately not mapped
//!
//! cuenv does not yet produce output symlinks, node properties, or tree output
//! directories, so those REAPI fields are left at their defaults. Because
//! proto3 elides defaults, they contribute nothing to the digest, and adding
//! them later changes the digest only for actions that actually use them.

use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::message::{
    Action, ActionResult, Command, Directory, DirectoryNode, ExecutionMetadata, FileNode,
    OutputDirectory, OutputFile, Platform, SymlinkNode, Tree,
};
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::google::protobuf::Timestamp;
use chrono::{DateTime, Utc};

/// A message with a canonical REAPI protobuf encoding.
///
/// Implemented for the cuenv message types that are named by digest. The
/// digest of a value is the SHA-256 of `to_canonical_bytes`.
pub trait CanonicalMessage {
    /// The REAPI protobuf type this message encodes as.
    type Proto: prost::Message;

    /// Convert to the protobuf representation.
    ///
    /// # Errors
    ///
    /// Returns an error if a field cannot be represented in REAPI — in
    /// practice only a blob size that exceeds `i64::MAX`.
    fn to_proto(&self) -> Result<Self::Proto>;

    /// Encode to canonical protobuf bytes.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Self::to_proto`].
    fn to_canonical_bytes(&self) -> Result<Vec<u8>> {
        Ok(prost::Message::encode_to_vec(&self.to_proto()?))
    }
}

// =============================================================================
// Digest
// =============================================================================

impl Digest {
    /// Convert to the REAPI digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the size exceeds `i64::MAX`, which REAPI cannot
    /// represent.
    pub fn to_proto(&self) -> Result<pb::Digest> {
        self.validate()?;
        let size_bytes = i64::try_from(self.size_bytes).map_err(|_| {
            Error::serialization(format!(
                "blob size {} exceeds the REAPI maximum of {}",
                self.size_bytes,
                i64::MAX
            ))
        })?;
        Ok(pb::Digest {
            hash: self.hash.clone(),
            size_bytes,
        })
    }

    /// Build from a REAPI digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the size is negative or the hash is not canonical
    /// lowercase SHA-256 hexadecimal.
    pub fn from_proto(proto: &pb::Digest) -> Result<Self> {
        let size_bytes = u64::try_from(proto.size_bytes).map_err(|_| {
            Error::serialization(format!("negative blob size {}", proto.size_bytes))
        })?;
        Self::new(proto.hash.clone(), size_bytes)
    }
}

/// Convert an optional REAPI digest, treating absence as an error.
///
/// REAPI marks nested messages optional because proto3 has no required
/// fields, not because the field is genuinely optional. A missing digest is
/// malformed input, not a default.
fn require_digest(proto: Option<&pb::Digest>, field: &str) -> Result<Digest> {
    let proto =
        proto.ok_or_else(|| Error::serialization(format!("missing required digest: {field}")))?;
    Digest::from_proto(proto)
}

// =============================================================================
// Action
// =============================================================================

/// Prefix for the value cuenv puts in `Action.salt`.
///
/// REAPI reserves `salt` for exactly this: placing an action into a separate
/// cache namespace without changing what it does. A self-describing textual
/// value keeps it readable in server-side browsers such as buildbarn's.
const SALT_PREFIX: &str = "cuenv/action-semantics/v";

/// Build the salt bytes for a semantics version.
#[must_use]
pub fn salt_for(action_semantics_version: u32) -> Vec<u8> {
    format!("{SALT_PREFIX}{action_semantics_version}").into_bytes()
}

/// Recover a semantics version from salt bytes, if it is one cuenv wrote.
#[must_use]
pub fn semantics_version_from_salt(salt: &[u8]) -> Option<u32> {
    std::str::from_utf8(salt)
        .ok()?
        .strip_prefix(SALT_PREFIX)?
        .parse()
        .ok()
}

impl CanonicalMessage for Action {
    type Proto = pb::Action;

    fn to_proto(&self) -> Result<pb::Action> {
        Ok(pb::Action {
            command_digest: Some(self.command_digest.to_proto()?),
            input_root_digest: Some(self.input_root_digest.to_proto()?),
            // cuenv enforces timeouts itself, on the host process group, and
            // records nothing about them in the key: a task that times out
            // produces no cache entry either way.
            do_not_cache: false,
            salt: salt_for(self.action_semantics_version),
            platform: Some(self.platform.to_proto()),
            ..Default::default()
        })
    }
}

impl Action {
    /// Build from a REAPI action.
    ///
    /// # Errors
    ///
    /// Returns an error if a required digest is missing or malformed, or if
    /// the salt is not one cuenv wrote.
    pub fn from_proto(proto: &pb::Action) -> Result<Self> {
        let action_semantics_version = semantics_version_from_salt(&proto.salt).ok_or_else(|| {
            Error::serialization(format!(
                "action salt {:?} was not written by cuenv",
                String::from_utf8_lossy(&proto.salt)
            ))
        })?;
        Ok(Self {
            command_digest: require_digest(
                proto.command_digest.as_ref(),
                "Action.command_digest",
            )?,
            input_root_digest: require_digest(
                proto.input_root_digest.as_ref(),
                "Action.input_root_digest",
            )?,
            platform: proto
                .platform
                .as_ref()
                .map(Platform::from_proto)
                .unwrap_or_default(),
            action_semantics_version,
        })
    }
}

// =============================================================================
// Platform
// =============================================================================

impl Platform {
    /// Convert to the REAPI platform, sorted by name then value.
    #[must_use]
    pub fn to_proto(&self) -> pb::Platform {
        // A `BTreeMap` iterates name-sorted, and a map cannot hold two values
        // for one name, so name order is already the REAPI order.
        pb::Platform {
            properties: self
                .properties
                .iter()
                .map(|(name, value)| pb::platform::Property {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        }
    }

    /// Build from a REAPI platform.
    #[must_use]
    pub fn from_proto(proto: &pb::Platform) -> Self {
        Self {
            properties: proto
                .properties
                .iter()
                .map(|property| (property.name.clone(), property.value.clone()))
                .collect(),
        }
    }
}

// =============================================================================
// Command
// =============================================================================

impl CanonicalMessage for Command {
    type Proto = pb::Command;

    fn to_proto(&self) -> Result<pb::Command> {
        // REAPI v2.1 replaced the separate `output_files` / `output_directories`
        // lists with one `output_paths`, and deprecated the originals. cuenv
        // does not know which of a task's declared outputs are files and which
        // are directories until the task has run, so the merged field is also
        // the only one it can fill honestly.
        let mut output_paths: Vec<String> = self
            .output_files
            .iter()
            .chain(self.output_directories.iter())
            .cloned()
            .collect();
        output_paths.sort();
        output_paths.dedup();

        Ok(pb::Command {
            arguments: self.arguments.clone(),
            environment_variables: self
                .environment_variables
                .iter()
                .map(|(name, value)| pb::command::EnvironmentVariable {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            output_paths,
            working_directory: self.working_directory.clone(),
            // `Command.platform` is left at its default. `Action.platform` is
            // the current home for platform properties; the `Command` field is
            // deprecated, and populating both would hash the same fact twice.
            ..Default::default()
        })
    }
}

impl Command {
    /// Build from a REAPI command.
    ///
    /// Output paths land in [`Command::output_files`]: REAPI's merged
    /// `output_paths` does not record which entries were directories, and
    /// cuenv does not need that distinction to compute a key.
    #[must_use]
    pub fn from_proto(proto: &pb::Command) -> Self {
        Self {
            arguments: proto.arguments.clone(),
            environment_variables: proto
                .environment_variables
                .iter()
                .map(|variable| (variable.name.clone(), variable.value.clone()))
                .collect(),
            output_files: proto.output_paths.clone(),
            output_directories: Vec::new(),
            working_directory: proto.working_directory.clone(),
        }
    }
}

// =============================================================================
// Directory
// =============================================================================

impl CanonicalMessage for Directory {
    type Proto = pb::Directory;

    fn to_proto(&self) -> Result<pb::Directory> {
        let mut files = Vec::with_capacity(self.files.len());
        for file in &self.files {
            files.push(pb::FileNode {
                name: file.name.clone(),
                digest: Some(file.digest.to_proto()?),
                is_executable: file.is_executable,
                ..Default::default()
            });
        }
        let mut directories = Vec::with_capacity(self.directories.len());
        for directory in &self.directories {
            directories.push(pb::DirectoryNode {
                name: directory.name.clone(),
                digest: Some(directory.digest.to_proto()?),
            });
        }
        let mut symlinks: Vec<pb::SymlinkNode> = self
            .symlinks
            .iter()
            .map(|symlink| pb::SymlinkNode {
                name: symlink.name.clone(),
                target: symlink.target.clone(),
                ..Default::default()
            })
            .collect();

        // Callers build these from `BTreeMap`s and so hand them over sorted,
        // but the canonical form is a REAPI requirement rather than a caller
        // convention: sort here so it holds however the value was built.
        files.sort_by(|a, b| a.name.cmp(&b.name));
        directories.sort_by(|a, b| a.name.cmp(&b.name));
        symlinks.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(pb::Directory {
            files,
            directories,
            symlinks,
            ..Default::default()
        })
    }
}

impl Directory {
    /// Build from a REAPI directory.
    ///
    /// # Errors
    ///
    /// Returns an error if any child digest is missing or malformed.
    pub fn from_proto(proto: &pb::Directory) -> Result<Self> {
        let mut files = Vec::with_capacity(proto.files.len());
        for file in &proto.files {
            files.push(FileNode {
                name: file.name.clone(),
                digest: require_digest(file.digest.as_ref(), "FileNode.digest")?,
                is_executable: file.is_executable,
            });
        }
        let mut directories = Vec::with_capacity(proto.directories.len());
        for directory in &proto.directories {
            directories.push(DirectoryNode {
                name: directory.name.clone(),
                digest: require_digest(directory.digest.as_ref(), "DirectoryNode.digest")?,
            });
        }
        Ok(Self {
            files,
            directories,
            symlinks: proto
                .symlinks
                .iter()
                .map(|symlink| SymlinkNode {
                    name: symlink.name.clone(),
                    target: symlink.target.clone(),
                })
                .collect(),
        })
    }
}

// =============================================================================
// Tree
// =============================================================================

impl CanonicalMessage for Tree {
    type Proto = pb::Tree;

    fn to_proto(&self) -> Result<pb::Tree> {
        let root = Some(self.root.to_proto()?);
        let mut children = self
            .children
            .iter()
            .map(|directory| {
                let digest = crate::digest::digest_of(directory)?;
                Ok((digest.hash, directory.to_proto()?))
            })
            .collect::<Result<Vec<_>>>()?;
        children.sort_by(|(left, _), (right, _)| left.cmp(right));

        Ok(pb::Tree {
            root,
            children: children
                .into_iter()
                .map(|(_, directory)| directory)
                .collect(),
        })
    }
}

impl Tree {
    /// Build from a REAPI output tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the required root is absent or a child digest is
    /// malformed.
    pub fn from_proto(proto: &pb::Tree) -> Result<Self> {
        let root = proto
            .root
            .as_ref()
            .ok_or_else(|| Error::serialization("missing required Tree.root"))
            .and_then(Directory::from_proto)?;
        let children = proto
            .children
            .iter()
            .map(Directory::from_proto)
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { root, children })
    }
}

// =============================================================================
// ActionResult
// =============================================================================

impl CanonicalMessage for ActionResult {
    type Proto = pb::ActionResult;

    fn to_proto(&self) -> Result<pb::ActionResult> {
        let mut output_files = Vec::with_capacity(self.output_files.len());
        for file in &self.output_files {
            output_files.push(pb::OutputFile {
                path: file.path.clone(),
                digest: Some(file.digest.to_proto()?),
                is_executable: file.is_executable,
                ..Default::default()
            });
        }
        let mut output_directories = Vec::with_capacity(self.output_directories.len());
        for directory in &self.output_directories {
            output_directories.push(pb::OutputDirectory {
                path: directory.path.clone(),
                tree_digest: Some(directory.tree_digest.to_proto()?),
                ..Default::default()
            });
        }
        output_files.sort_by(|a, b| a.path.cmp(&b.path));
        output_directories.sort_by(|a, b| a.path.cmp(&b.path));

        let stdout_digest = self.stdout_digest.as_ref().map(Digest::to_proto).transpose()?;
        let stderr_digest = self.stderr_digest.as_ref().map(Digest::to_proto).transpose()?;

        Ok(pb::ActionResult {
            output_files,
            output_directories,
            exit_code: self.exit_code,
            stdout_digest,
            stderr_digest,
            execution_metadata: Some(self.execution_metadata.to_proto()),
            ..Default::default()
        })
    }
}

impl ActionResult {
    /// Build from a REAPI action result.
    ///
    /// # Errors
    ///
    /// Returns an error if any referenced digest is missing or malformed.
    pub fn from_proto(proto: &pb::ActionResult) -> Result<Self> {
        let mut output_files = Vec::with_capacity(proto.output_files.len());
        for file in &proto.output_files {
            output_files.push(OutputFile {
                path: file.path.clone(),
                digest: require_digest(file.digest.as_ref(), "OutputFile.digest")?,
                is_executable: file.is_executable,
            });
        }
        let mut output_directories = Vec::with_capacity(proto.output_directories.len());
        for directory in &proto.output_directories {
            output_directories.push(OutputDirectory {
                path: directory.path.clone(),
                tree_digest: require_digest(
                    directory.tree_digest.as_ref(),
                    "OutputDirectory.tree_digest",
                )?,
            });
        }

        Ok(Self {
            output_files,
            output_directories,
            exit_code: proto.exit_code,
            stdout_digest: proto
                .stdout_digest
                .as_ref()
                .map(Digest::from_proto)
                .transpose()?,
            stderr_digest: proto
                .stderr_digest
                .as_ref()
                .map(Digest::from_proto)
                .transpose()?,
            execution_metadata: proto
                .execution_metadata
                .as_ref()
                .map(ExecutionMetadata::from_proto)
                .unwrap_or_default(),
        })
    }
}

// =============================================================================
// ExecutionMetadata
// =============================================================================

impl ExecutionMetadata {
    /// Convert to REAPI execution metadata.
    ///
    /// cuenv records a completion instant and a duration; REAPI records
    /// instants only. The start timestamp is therefore derived as
    /// `completed - duration`, which recovers the duration exactly on the way
    /// back.
    #[must_use]
    pub fn to_proto(&self) -> pb::ExecutedActionMetadata {
        let completed = self.created_at;
        let started = i64::try_from(self.duration_ms)
            .ok()
            .and_then(|millis| chrono::TimeDelta::try_milliseconds(millis).map(|d| completed - d));

        pb::ExecutedActionMetadata {
            worker: self.worker.clone(),
            worker_start_timestamp: started.map(to_timestamp),
            worker_completed_timestamp: Some(to_timestamp(completed)),
            ..Default::default()
        }
    }

    /// Build from REAPI execution metadata.
    #[must_use]
    pub fn from_proto(proto: &pb::ExecutedActionMetadata) -> Self {
        let completed = proto
            .worker_completed_timestamp
            .as_ref()
            .and_then(from_timestamp)
            .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now));
        let started = proto
            .worker_start_timestamp
            .as_ref()
            .and_then(from_timestamp);
        let duration_ms = started
            .map(|start| completed.signed_duration_since(start).num_milliseconds())
            .and_then(|millis| u128::try_from(millis).ok())
            .unwrap_or(0);

        Self {
            worker: proto.worker.clone(),
            duration_ms,
            created_at: completed,
        }
    }
}

fn to_timestamp(value: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: value.timestamp(),
        nanos: i32::try_from(value.timestamp_subsec_nanos()).unwrap_or(0),
    }
}

fn from_timestamp(value: &Timestamp) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(value.seconds, u32::try_from(value.nanos).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn digest(bytes: &[u8]) -> Digest {
        Digest::of_bytes(bytes)
    }

    fn sample_action() -> Action {
        let mut properties = BTreeMap::new();
        properties.insert("os".to_string(), "linux".to_string());
        properties.insert("arch".to_string(), "x86_64".to_string());
        Action {
            command_digest: digest(b"cmd"),
            input_root_digest: digest(b"root"),
            platform: Platform { properties },
            action_semantics_version: 2,
        }
    }

    fn sample_command() -> Command {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/nix/store/x/bin".to_string());
        env.insert("BUILD".to_string(), "release".to_string());
        Command {
            arguments: vec!["cargo".into(), "build".into()],
            environment_variables: env,
            output_files: vec!["b.txt".into(), "a.txt".into()],
            output_directories: vec!["dist".into()],
            working_directory: "sub".into(),
        }
    }

    #[test]
    fn action_round_trips() {
        let action = sample_action();
        let proto = action.to_proto().unwrap();
        assert_eq!(Action::from_proto(&proto).unwrap(), action);
    }

    #[test]
    fn action_salt_carries_the_semantics_version() {
        let proto = sample_action().to_proto().unwrap();
        assert_eq!(proto.salt, b"cuenv/action-semantics/v2".to_vec());
        assert_eq!(semantics_version_from_salt(&proto.salt), Some(2));
    }

    #[test]
    fn action_from_foreign_salt_is_rejected() {
        let mut proto = sample_action().to_proto().unwrap();
        proto.salt = b"some-other-tool".to_vec();
        assert!(Action::from_proto(&proto).is_err());
    }

    #[test]
    fn malformed_remote_digest_is_rejected() {
        let proto = pb::Digest {
            hash: "../escape".to_string(),
            size_bytes: 0,
        };
        assert!(Digest::from_proto(&proto).is_err());

        let proto = pb::Digest {
            hash: "A".repeat(64),
            size_bytes: 0,
        };
        assert!(Digest::from_proto(&proto).is_err());
    }

    #[test]
    fn semantics_version_changes_the_encoding() {
        let mut other = sample_action();
        other.action_semantics_version = 3;
        assert_ne!(
            sample_action().to_canonical_bytes().unwrap(),
            other.to_canonical_bytes().unwrap()
        );
    }

    #[test]
    fn command_output_paths_are_sorted_and_merged() {
        let proto = sample_command().to_proto().unwrap();
        assert_eq!(proto.output_paths, vec!["a.txt", "b.txt", "dist"]);
    }

    #[test]
    fn command_output_order_does_not_change_the_digest() {
        let mut reordered = sample_command();
        reordered.output_files = vec!["a.txt".into(), "b.txt".into()];
        assert_eq!(
            sample_command().to_canonical_bytes().unwrap(),
            reordered.to_canonical_bytes().unwrap(),
            "declaring the same outputs in a different order must not miss the cache"
        );
    }

    #[test]
    fn command_environment_is_name_sorted() {
        let proto = sample_command().to_proto().unwrap();
        let names: Vec<&str> = proto
            .environment_variables
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(names, vec!["BUILD", "PATH"]);
    }

    #[test]
    fn platform_properties_are_name_sorted() {
        let proto = sample_action().to_proto().unwrap();
        let names: Vec<&str> = proto
            .platform
            .as_ref()
            .unwrap()
            .properties
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["arch", "os"]);
    }

    #[test]
    #[expect(
        deprecated,
        reason = "asserting cuenv leaves the deprecated Command.platform unset"
    )]
    fn command_platform_is_left_to_the_action() {
        // Setting both would hash the same fact twice and `Command.platform`
        // is the deprecated spelling.
        assert!(sample_command().to_proto().unwrap().platform.is_none());
    }

    #[test]
    fn directory_round_trips_and_sorts() {
        let directory = Directory {
            files: vec![
                FileNode {
                    name: "z.txt".into(),
                    digest: digest(b"z"),
                    is_executable: true,
                },
                FileNode {
                    name: "a.txt".into(),
                    digest: digest(b"a"),
                    is_executable: false,
                },
            ],
            directories: vec![DirectoryNode {
                name: "sub".into(),
                digest: digest(b"sub"),
            }],
            symlinks: vec![SymlinkNode {
                name: "link".into(),
                target: "a.txt".into(),
            }],
        };
        let proto = directory.to_proto().unwrap();
        assert_eq!(
            proto.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "z.txt"]
        );
        let back = Directory::from_proto(&proto).unwrap();
        assert_eq!(back.files.len(), 2);
        assert_eq!(back.directories, directory.directories);
        assert_eq!(back.symlinks, directory.symlinks);
    }

    #[test]
    fn action_result_round_trips() {
        let result = ActionResult {
            output_files: vec![OutputFile {
                path: "out/a.txt".into(),
                digest: digest(b"a"),
                is_executable: true,
            }],
            output_directories: vec![OutputDirectory {
                path: "dist".into(),
                tree_digest: digest(b"tree"),
            }],
            exit_code: 0,
            stdout_digest: Some(digest(b"stdout")),
            stderr_digest: None,
            execution_metadata: ExecutionMetadata {
                worker: "local".into(),
                duration_ms: 1234,
                created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            },
        };
        let proto = result.to_proto().unwrap();
        assert_eq!(ActionResult::from_proto(&proto).unwrap(), result);
    }

    #[test]
    fn execution_metadata_duration_survives_the_round_trip() {
        let metadata = ExecutionMetadata {
            worker: "local".into(),
            duration_ms: 4_242,
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 500_000_000).unwrap(),
        };
        let back = ExecutionMetadata::from_proto(&metadata.to_proto());
        assert_eq!(back, metadata);
    }

    #[test]
    fn oversized_digest_is_rejected_rather_than_wrapped() {
        let huge = Digest {
            hash: "ab".repeat(32),
            size_bytes: u64::MAX,
        };
        assert!(huge.to_proto().is_err());
    }

    #[test]
    fn negative_proto_size_is_rejected() {
        let negative = pb::Digest {
            hash: "ab".repeat(32),
            size_bytes: -1,
        };
        assert!(Digest::from_proto(&negative).is_err());
    }

    #[test]
    fn encoding_is_deterministic() {
        let action = sample_action();
        assert_eq!(
            action.to_canonical_bytes().unwrap(),
            action.to_canonical_bytes().unwrap()
        );
    }

    #[test]
    fn encoded_bytes_decode_as_real_protobuf() {
        // The point of the exercise: a REAPI server must be able to parse what
        // cuenv stores.
        let bytes = sample_action().to_canonical_bytes().unwrap();
        let decoded = <pb::Action as prost::Message>::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.salt, b"cuenv/action-semantics/v2".to_vec());
    }
}
