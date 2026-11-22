//! Action inspection and cache-miss diffing over the local CAS and ActionCache.
//!
//! Decodes REAPI `Action`, `Command`, and Merkle `Directory` blobs so users
//! can inspect what entered a cache key (`cuenv cache inspect`) and diff two
//! action digests (`cuenv cache diff`) to see the exact argument, environment
//! variable, platform property, output declaration, or input file that caused
//! a cache miss.

use crate::action_cache::{ActionCache, LocalActionCache};
use crate::cas::{Cas, LocalCas};
use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::message::{Action, Command, Directory};
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use prost::Message as _;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A flattened file entry from an action's Merkle input tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedInput {
    /// Content SHA-256 digest of the input file.
    pub digest: Digest,
    /// Whether the file is staged executable.
    pub is_executable: bool,
}

/// Summary of an action's cached result, if present in the local ActionCache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedActionResult {
    /// Process exit code recorded in the cached result.
    pub exit_code: i32,
    /// Output file paths recorded in the cached result.
    pub output_files: Vec<String>,
    /// Output directory paths recorded in the cached result.
    pub output_directories: Vec<String>,
}

/// Complete decoded view of a cached [`Action`] and its [`Command`] + input tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionInspection {
    /// Digest of the [`Action`] message (the action cache key).
    pub action_digest: Digest,
    /// Execution-semantics salt version.
    pub action_semantics_version: u32,
    /// Command argv vector.
    pub arguments: Vec<String>,
    /// Working directory relative to the input root.
    pub working_directory: String,
    /// Environment variables recorded in the action key.
    pub environment_variables: BTreeMap<String, String>,
    /// Declared output paths (sorted).
    pub output_paths: Vec<String>,
    /// Platform properties (including `cuenv.sandbox`, `cuenv.sandbox.policy`, runtime identity).
    pub platform: BTreeMap<String, String>,
    /// Root digest of the Merkle input tree.
    pub input_root_digest: Digest,
    /// Flattened input files (`relative path -> digest + executable`).
    pub inputs: BTreeMap<String, InspectedInput>,
    /// Cached `ActionResult` summary if present in the local ActionCache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_result: Option<InspectedActionResult>,
}

/// A single changed scalar or vector field between two actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldDiff<T> {
    /// Value in the left (baseline) action.
    pub left: T,
    /// Value in the right (target) action.
    pub right: T,
}

/// Key/value map differences between two actions (env vars or platform properties).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MapDiff {
    /// Keys present only in the right action.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub added: BTreeMap<String, String>,
    /// Keys present only in the left action.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub removed: BTreeMap<String, String>,
    /// Keys present in both with different values.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<String, FieldDiff<String>>,
}

impl MapDiff {
    /// Whether no keys were added, removed, or changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Set differences between two sorted string lists (declared outputs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SetDiff {
    /// Entries present only in the right action.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// Entries present only in the left action.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

impl SetDiff {
    /// Whether no entries were added or removed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// File-level differences between two actions' Merkle input trees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InputTreeDiff {
    /// Input file paths present only in the right action.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// Input file paths present only in the left action.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// Input file paths present in both with different content digest or mode.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<String, FieldDiff<InspectedInput>>,
}

impl InputTreeDiff {
    /// Whether the two input trees contain identical files and modes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Structured diff explaining why two [`Action`] digests differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionDiff {
    /// Baseline action digest.
    pub left: Digest,
    /// Compared action digest.
    pub right: Digest,
    /// True when both actions have identical decoded keys.
    pub identical: bool,
    /// Difference in execution-semantics version, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_semantics_version: Option<FieldDiff<u32>>,
    /// Difference in working directory, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<FieldDiff<String>>,
    /// Difference in command arguments, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<FieldDiff<Vec<String>>>,
    /// Differences in environment variables.
    pub environment: MapDiff,
    /// Differences in platform properties.
    pub platform: MapDiff,
    /// Differences in declared outputs.
    pub outputs: SetDiff,
    /// Differences in flattened Merkle input files.
    pub inputs: InputTreeDiff,
}

/// Resolve a digest specification (`<64-hex>` or `<64-hex>/<size>`) against
/// the local CAS on disk.
///
/// # Errors
///
/// Returns an error if the digest format is invalid or the blob is not found
/// in the local CAS.
pub fn resolve_local_digest(cache_root: &Path, spec: &str) -> Result<Digest> {
    let trimmed = spec.trim();
    if let Some((hash, size_str)) = trimmed.split_once('/') {
        let size_bytes: u64 = size_str
            .parse()
            .map_err(|_| Error::configuration(format!("invalid digest size in '{trimmed}'")))?;
        return Digest::try_new(hash.to_ascii_lowercase(), size_bytes);
    }

    let hash = trimmed.to_ascii_lowercase();
    let probe = Digest::try_new(hash.clone(), 0)?;
    let (prefix, rest) = hash.split_at(2);
    let blob_path = cache_root
        .join("cas")
        .join("sha256")
        .join(prefix)
        .join(rest);
    let meta = std::fs::metadata(&blob_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::not_found(probe.hash)
        } else {
            Error::io(err, &blob_path, "metadata")
        }
    })?;
    Digest::try_new(hash, meta.len())
}

/// Inspect a cached [`Action`] by digest (`<hash>` or `<hash>/<size>`) in `cache_root`.
///
/// # Errors
///
/// Returns an error if the action, command, or input-tree blobs are missing or
/// cannot be decoded.
pub async fn inspect_action(cache_root: &Path, digest_spec: &str) -> Result<ActionInspection> {
    let action_digest = resolve_local_digest(cache_root, digest_spec)?;
    let cas = LocalCas::open(cache_root)?;
    let ac = LocalActionCache::open(cache_root)?;

    let action_bytes = cas.get(&action_digest).await?;
    let proto_action = pb::Action::decode(action_bytes.as_slice())
        .map_err(|e| Error::serialization(format!("decode Action protobuf: {e}")))?;
    let action = Action::from_proto(&proto_action)?;

    let command_bytes = cas.get(&action.command_digest).await?;
    let proto_command = pb::Command::decode(command_bytes.as_slice())
        .map_err(|e| Error::serialization(format!("decode Command protobuf: {e}")))?;
    let command = Command::from_proto(&proto_command);

    let inputs = flatten_directory(&cas, &action.input_root_digest).await?;

    let mut output_paths: Vec<String> = command
        .output_files
        .into_iter()
        .chain(command.output_directories)
        .collect();
    output_paths.sort();
    output_paths.dedup();

    let cached_result = ac
        .lookup(&action_digest)
        .await?
        .map(|result| InspectedActionResult {
            exit_code: result.exit_code,
            output_files: result.output_files.into_iter().map(|f| f.path).collect(),
            output_directories: result
                .output_directories
                .into_iter()
                .map(|d| d.path)
                .collect(),
        });

    Ok(ActionInspection {
        action_digest,
        action_semantics_version: action.action_semantics_version,
        arguments: command.arguments,
        working_directory: command.working_directory,
        environment_variables: command.environment_variables,
        output_paths,
        platform: action.platform.properties,
        input_root_digest: action.input_root_digest,
        inputs,
        cached_result,
    })
}

/// Diff two cached [`Action`] digests to explain why they produced different cache keys.
///
/// # Errors
///
/// Returns an error if either action cannot be loaded or decoded from `cache_root`.
pub async fn diff_actions(cache_root: &Path, left: &str, right: &str) -> Result<ActionDiff> {
    let left_info = inspect_action(cache_root, left).await?;
    let right_info = inspect_action(cache_root, right).await?;
    Ok(compute_action_diff(&left_info, &right_info))
}

/// Compute a structured diff between two decoded [`ActionInspection`] records.
#[must_use]
pub fn compute_action_diff(left: &ActionInspection, right: &ActionInspection) -> ActionDiff {
    let action_semantics_version =
        (left.action_semantics_version != right.action_semantics_version).then_some(FieldDiff {
            left: left.action_semantics_version,
            right: right.action_semantics_version,
        });
    let working_directory =
        (left.working_directory != right.working_directory).then(|| FieldDiff {
            left: left.working_directory.clone(),
            right: right.working_directory.clone(),
        });
    let arguments = (left.arguments != right.arguments).then(|| FieldDiff {
        left: left.arguments.clone(),
        right: right.arguments.clone(),
    });
    let environment = diff_maps(&left.environment_variables, &right.environment_variables);
    let platform = diff_maps(&left.platform, &right.platform);
    let outputs = diff_sets(&left.output_paths, &right.output_paths);
    let inputs = diff_inputs(&left.inputs, &right.inputs);

    let identical = action_semantics_version.is_none()
        && working_directory.is_none()
        && arguments.is_none()
        && environment.is_empty()
        && platform.is_empty()
        && outputs.is_empty()
        && inputs.is_empty();

    ActionDiff {
        left: left.action_digest.clone(),
        right: right.action_digest.clone(),
        identical,
        action_semantics_version,
        working_directory,
        arguments,
        environment,
        platform,
        outputs,
        inputs,
    }
}

fn diff_maps(left: &BTreeMap<String, String>, right: &BTreeMap<String, String>) -> MapDiff {
    let mut diff = MapDiff::default();
    for (k, v) in left {
        match right.get(k) {
            None => {
                diff.removed.insert(k.clone(), v.clone());
            }
            Some(rv) if rv != v => {
                diff.changed.insert(
                    k.clone(),
                    FieldDiff {
                        left: v.clone(),
                        right: rv.clone(),
                    },
                );
            }
            Some(_) => {}
        }
    }
    for (k, v) in right {
        if !left.contains_key(k) {
            diff.added.insert(k.clone(), v.clone());
        }
    }
    diff
}

fn diff_sets(left: &[String], right: &[String]) -> SetDiff {
    let left_set: BTreeSet<&String> = left.iter().collect();
    let right_set: BTreeSet<&String> = right.iter().collect();
    SetDiff {
        added: right_set
            .difference(&left_set)
            .map(|s| (*s).clone())
            .collect(),
        removed: left_set
            .difference(&right_set)
            .map(|s| (*s).clone())
            .collect(),
    }
}

fn diff_inputs(
    left: &BTreeMap<String, InspectedInput>,
    right: &BTreeMap<String, InspectedInput>,
) -> InputTreeDiff {
    let mut diff = InputTreeDiff::default();
    for (path, info) in left {
        match right.get(path) {
            None => diff.removed.push(path.clone()),
            Some(rinfo) if rinfo != info => {
                diff.changed.insert(
                    path.clone(),
                    FieldDiff {
                        left: info.clone(),
                        right: rinfo.clone(),
                    },
                );
            }
            Some(_) => {}
        }
    }
    for path in right.keys() {
        if !left.contains_key(path) {
            diff.added.push(path.clone());
        }
    }
    diff
}

async fn flatten_directory(
    cas: &LocalCas,
    root_digest: &Digest,
) -> Result<BTreeMap<String, InspectedInput>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root_digest.clone(), String::new())];
    while let Some((digest, prefix)) = stack.pop() {
        let bytes = cas.get(&digest).await?;
        let proto_dir = pb::Directory::decode(bytes.as_slice())
            .map_err(|e| Error::serialization(format!("decode Directory protobuf: {e}")))?;
        let dir = Directory::from_proto(&proto_dir)?;
        for file in dir.files {
            let path = if prefix.is_empty() {
                file.name
            } else {
                format!("{prefix}/{}", file.name)
            };
            out.insert(
                path,
                InspectedInput {
                    digest: file.digest,
                    is_executable: file.is_executable,
                },
            );
        }
        for sub in dir.directories {
            let sub_prefix = if prefix.is_empty() {
                sub.name
            } else {
                format!("{prefix}/{}", sub.name)
            };
            stack.push((sub.digest, sub_prefix));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ACTION_SEMANTICS_VERSION, FileNode, Platform};
    use crate::reapi::CanonicalMessage;

    async fn store_action(cas: &LocalCas, env_val: &str, file_bytes: &[u8]) -> Result<Digest> {
        let file_digest = cas.put_bytes(file_bytes).await?;
        let dir = Directory {
            files: vec![FileNode {
                name: "main.rs".to_string(),
                digest: file_digest,
                is_executable: false,
            }],
            directories: Vec::new(),
            symlinks: Vec::new(),
        };
        let root_digest = cas.put_bytes(&dir.to_canonical_bytes()?).await?;

        let mut env = BTreeMap::new();
        env.insert("RUST_LOG".to_string(), env_val.to_string());
        let cmd = Command {
            arguments: vec!["cargo".to_string(), "build".to_string()],
            environment_variables: env,
            output_files: vec!["target/app".to_string()],
            output_directories: Vec::new(),
            working_directory: ".".to_string(),
        };
        let cmd_digest = cas.put_bytes(&cmd.to_canonical_bytes()?).await?;

        let mut props = BTreeMap::new();
        props.insert("cuenv.sandbox".to_string(), "strict".to_string());
        let action = Action {
            command_digest: cmd_digest,
            input_root_digest: root_digest,
            platform: Platform { properties: props },
            action_semantics_version: ACTION_SEMANTICS_VERSION,
        };
        cas.put_bytes(&action.to_canonical_bytes()?).await
    }

    #[tokio::test]
    async fn inspect_and_diff_identify_changed_env_and_input_file() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let cas = LocalCas::open(dir.path())?;
        let a = store_action(&cas, "info", b"fn main() {}").await?;
        let b = store_action(&cas, "debug", b"fn main() { println!(\"hi\"); }").await?;

        let info_a = inspect_action(dir.path(), &a.hash).await?;
        assert_eq!(info_a.arguments, vec!["cargo", "build"]);
        assert_eq!(info_a.inputs.len(), 1);
        assert!(info_a.inputs.contains_key("main.rs"));

        let diff = diff_actions(dir.path(), &a.hash, &b.hash).await?;
        assert!(!diff.identical);
        assert!(diff.arguments.is_none());
        assert_eq!(diff.environment.changed["RUST_LOG"].left, "info");
        assert_eq!(diff.environment.changed["RUST_LOG"].right, "debug");
        assert!(diff.inputs.changed.contains_key("main.rs"));
        Ok(())
    }
}
