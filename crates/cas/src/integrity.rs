//! Referential integrity checks for [`ActionResult`] records.
//!
//! An [`ActionCache`](crate::action_cache::ActionCache) entry is only a
//! promise: it names blobs by digest, and nothing stops those blobs from
//! being evicted, garbage-collected or never uploaded in the first place.
//! Serving such an entry as a cache hit fails partway through
//! materialization and leaves the caller with a half-restored output tree.
//!
//! [`missing_blobs`] lets a caller check the promise before acting on it,
//! so a dangling entry degrades to an ordinary cache miss.

use crate::cas::Cas;
use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::message::{ActionResult, Directory};
use async_recursion::async_recursion;
use std::collections::{HashMap, HashSet};

/// Maximum directory nesting walked while checking an output tree.
///
/// Content addressing makes genuine cycles impossible — a child's digest is
/// derived from its content, so a directory cannot contain itself — but a
/// corrupt or hostile store is not bound by that, and an unbounded walk
/// would hang rather than report the corruption.
const MAX_TREE_DEPTH: usize = 128;

/// Digests referenced by `result` that `cas` does not currently hold.
///
/// An empty vector means every blob the result names is present and the
/// result can be materialized without a mid-way failure. Output directory
/// trees are walked transitively, so a missing leaf deep inside a cached
/// tree is reported rather than discovered during materialization.
///
/// Digests are returned in discovery order with duplicates removed.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, if a directory message
/// cannot be decoded, or if an output tree nests deeper than
/// [`MAX_TREE_DEPTH`].
pub async fn missing_blobs(cas: &dyn Cas, result: &ActionResult) -> Result<Vec<Digest>> {
    let mut missing = Vec::new();
    let mut seen = HashSet::new();

    let direct: Vec<&Digest> = result
        .output_files
        .iter()
        .map(|file| &file.digest)
        .chain(result.stdout_digest.iter())
        .chain(result.stderr_digest.iter())
        .collect();
    for digest in direct {
        check(cas, digest, &mut seen, &mut missing).await?;
    }

    for directory in &result.output_directories {
        if !check(cas, &directory.tree_digest, &mut seen, &mut missing).await? {
            // Root is absent: nothing to walk, and the caller already knows
            // the tree cannot be materialized.
            continue;
        }
        let tree =
            crate::merkle::decode_tree(&cas.get(&directory.tree_digest).await?).map_err(|e| {
                Error::serialization(format!("decode Tree {}: {e}", directory.tree_digest))
            })?;
        let mut children = HashMap::with_capacity(tree.children.len());
        for child in tree.children {
            children.insert(crate::merkle::directory_digest(&child)?, child);
        }
        walk_tree(cas, &tree.root, &children, 0, &mut seen, &mut missing).await?;
    }

    Ok(missing)
}

/// Record `digest` as missing if the store does not hold it.
///
/// Returns whether the blob is present. Digests already visited are skipped
/// and reported using their first-seen presence.
async fn check(
    cas: &dyn Cas,
    digest: &Digest,
    seen: &mut HashSet<Digest>,
    missing: &mut Vec<Digest>,
) -> Result<bool> {
    if !seen.insert(digest.clone()) {
        return Ok(!missing.contains(digest));
    }
    if cas.contains(digest).await? {
        return Ok(true);
    }
    missing.push(digest.clone());
    Ok(false)
}

#[async_recursion]
async fn walk_tree(
    cas: &dyn Cas,
    directory: &Directory,
    children: &HashMap<Digest, Directory>,
    depth: usize,
    seen: &mut HashSet<Digest>,
    missing: &mut Vec<Digest>,
) -> Result<()> {
    if depth >= MAX_TREE_DEPTH {
        return Err(Error::serialization(format!(
            "output tree nests deeper than {MAX_TREE_DEPTH} levels"
        )));
    }

    for file in &directory.files {
        check(cas, &file.digest, seen, missing).await?;
    }
    for child in &directory.directories {
        let child_directory = children.get(&child.digest).ok_or_else(|| {
            Error::serialization(format!(
                "output Tree is missing child directory {} ({})",
                child.name, child.digest
            ))
        })?;
        walk_tree(cas, child_directory, children, depth + 1, seen, missing).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::LocalCas;
    use crate::digest::digest_of;
    use crate::message::{
        Directory, DirectoryNode, ExecutionMetadata, FileNode, OutputDirectory, OutputFile, Tree,
    };
    use crate::reapi::CanonicalMessage;
    use tempfile::TempDir;

    fn result_with_output(digest: Digest) -> ActionResult {
        ActionResult {
            output_files: vec![OutputFile {
                path: "out.txt".into(),
                digest,
                is_executable: false,
            }],
            output_directories: vec![],
            exit_code: 0,
            stdout_digest: None,
            stderr_digest: None,
            execution_metadata: ExecutionMetadata::default(),
        }
    }

    #[tokio::test]
    async fn complete_result_reports_nothing_missing() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = cas.put_bytes(b"present").await.unwrap();

        let result = result_with_output(digest);
        assert!(missing_blobs(&cas, &result).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn evicted_output_blob_is_reported() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = Digest::of_bytes(b"never stored");

        let result = result_with_output(digest.clone());
        assert_eq!(missing_blobs(&cas, &result).await.unwrap(), vec![digest]);
    }

    #[tokio::test]
    async fn missing_stdout_and_stderr_are_reported() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let present = cas.put_bytes(b"out").await.unwrap();

        let mut result = result_with_output(present);
        result.stdout_digest = Some(Digest::of_bytes(b"gone-stdout"));
        result.stderr_digest = Some(Digest::of_bytes(b"gone-stderr"));

        let missing = missing_blobs(&cas, &result).await.unwrap();
        assert_eq!(missing.len(), 2);
    }

    #[tokio::test]
    async fn duplicate_digests_are_reported_once() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let gone = Digest::of_bytes(b"gone");

        let mut result = result_with_output(gone.clone());
        result.output_files.push(OutputFile {
            path: "copy.txt".into(),
            digest: gone.clone(),
            is_executable: false,
        });
        result.stdout_digest = Some(gone.clone());

        assert_eq!(missing_blobs(&cas, &result).await.unwrap(), vec![gone]);
    }

    #[tokio::test]
    async fn nested_output_tree_leaf_is_reported() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();

        // A child directory referencing a blob that was never stored.
        let child = Directory {
            files: vec![FileNode {
                name: "leaf.txt".into(),
                digest: Digest::of_bytes(b"absent leaf"),
                is_executable: false,
            }],
            directories: vec![],
            symlinks: vec![],
        };
        let child_digest = digest_of(&child).unwrap();
        let root = Directory {
            files: vec![],
            directories: vec![DirectoryNode {
                name: "sub".into(),
                digest: child_digest,
            }],
            symlinks: vec![],
        };
        let tree = Tree {
            root,
            children: vec![child],
        };
        let tree_digest = cas
            .put_bytes(&tree.to_canonical_bytes().unwrap())
            .await
            .unwrap();

        let mut result = result_with_output(cas.put_bytes(b"fine").await.unwrap());
        result.output_directories = vec![OutputDirectory {
            path: "dist".into(),
            tree_digest,
        }];

        let missing = missing_blobs(&cas, &result).await.unwrap();
        assert_eq!(missing, vec![Digest::of_bytes(b"absent leaf")]);
    }

    #[tokio::test]
    async fn absent_tree_root_does_not_abort_the_check() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();

        let unstored_tree = Tree::default();
        let tree_digest = digest_of(&unstored_tree).unwrap();

        let mut result = result_with_output(Digest::of_bytes(b"also absent"));
        result.output_directories = vec![OutputDirectory {
            path: "dist".into(),
            tree_digest: tree_digest.clone(),
        }];

        let missing = missing_blobs(&cas, &result).await.unwrap();
        assert!(missing.contains(&tree_digest));
        assert!(missing.contains(&Digest::of_bytes(b"also absent")));
    }
}
