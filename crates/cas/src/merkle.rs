//! Merkle-tree construction over a directory.
//!
//! [`build_input_tree`] walks a directory, inserts every file into a [`Cas`]
//! and builds the corresponding [`Directory`] messages bottom-up. The root
//! digest identifies the entire input tree and is what an
//! [`Action`](crate::message::Action) references as its `input_root_digest`.

use crate::cas::Cas;
use crate::digest::{Digest, digest_of};
use crate::error::{Error, Result};
use crate::message::{Directory, DirectoryNode, FileNode};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Build a Merkle tree from `root` and insert every node into `cas`.
///
/// Returns the digest of the root [`Directory`]. Symlinks and special files
/// are skipped for now; this is the input tree for hermetic task execution,
/// so only regular files and directories matter.
///
/// # Errors
///
/// Returns an error if any filesystem operation fails, if a child blob
/// cannot be stored in `cas`, or if a [`Directory`] message cannot be
/// serialized.
pub fn build_input_tree(root: &Path, cas: &dyn Cas) -> Result<Digest> {
    let tree = build_directory(root, cas)?;
    let bytes = serde_json::to_vec(&tree)
        .map_err(|e| Error::serialization(format!("encode Directory: {e}")))?;
    cas.put_bytes(&bytes)
}

/// Materialize a previously-built input tree to `destination`. The destination
/// directory is created if it does not exist.
///
/// # Errors
///
/// Returns an error if any blob is missing from `cas`, if a [`Directory`]
/// message cannot be decoded, or if any filesystem operation fails.
pub fn materialize_input_tree(
    cas: &dyn Cas,
    root_digest: &Digest,
    destination: &Path,
) -> Result<()> {
    let bytes = cas.get(root_digest)?;
    let dir: Directory = serde_json::from_slice(&bytes)
        .map_err(|e| Error::serialization(format!("decode Directory: {e}")))?;
    fs::create_dir_all(destination).map_err(|e| Error::io(e, destination, "create_dir_all"))?;
    materialize_directory(cas, &dir, destination)
}

fn build_directory(dir: &Path, cas: &dyn Cas) -> Result<Directory> {
    // Collect children first so we can sort deterministically.
    let mut files: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();
    let mut subdirs: BTreeMap<String, PathBuf> = BTreeMap::new();

    let entries = fs::read_dir(dir).map_err(|e| Error::io(e, dir, "read_dir"))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(e, dir, "read_dir_entry"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| Error::io(e, &path, "file_type"))?;
        if ft.is_dir() {
            subdirs.insert(name, path);
        } else if ft.is_file() {
            let is_executable = is_executable(&path)?;
            files.insert(name, (path, is_executable));
        }
        // Symlinks and other kinds are intentionally dropped.
    }

    let mut file_nodes: Vec<FileNode> = Vec::with_capacity(files.len());
    for (name, (path, is_executable)) in files {
        let digest = cas.put_file(&path)?;
        file_nodes.push(FileNode {
            name,
            digest,
            is_executable,
        });
    }

    let mut dir_nodes: Vec<DirectoryNode> = Vec::with_capacity(subdirs.len());
    for (name, path) in subdirs {
        let child = build_directory(&path, cas)?;
        let bytes = serde_json::to_vec(&child)
            .map_err(|e| Error::serialization(format!("encode Directory: {e}")))?;
        let digest = cas.put_bytes(&bytes)?;
        dir_nodes.push(DirectoryNode { name, digest });
    }

    Ok(Directory {
        files: file_nodes,
        directories: dir_nodes,
        symlinks: Vec::new(),
    })
}

fn materialize_directory(cas: &dyn Cas, dir: &Directory, destination: &Path) -> Result<()> {
    for file in &dir.files {
        let dst = destination.join(&file.name);
        cas.get_to_file(&file.digest, &dst)?;
        #[cfg(unix)]
        if file.is_executable {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = fs::metadata(&dst)
                .map_err(|e| Error::io(e, &dst, "metadata"))?
                .permissions();
            perm.set_mode(perm.mode() | 0o111);
            fs::set_permissions(&dst, perm).map_err(|e| Error::io(e, &dst, "set_permissions"))?;
        }
    }
    for child in &dir.directories {
        let dst = destination.join(&child.name);
        fs::create_dir_all(&dst).map_err(|e| Error::io(e, &dst, "create_dir_all"))?;
        let bytes = cas.get(&child.digest)?;
        let sub: Directory = serde_json::from_slice(&bytes)
            .map_err(|e| Error::serialization(format!("decode Directory: {e}")))?;
        materialize_directory(cas, &sub, &dst)?;
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).map_err(|e| Error::io(e, path, "metadata"))?;
    Ok(meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}

/// Convenience: compute the digest of a [`Directory`] without inserting it.
///
/// Useful for tests and for callers that want to compare digests before
/// committing anything to the CAS.
///
/// # Errors
///
/// Returns an error if the [`Directory`] cannot be canonically encoded.
pub fn directory_digest(dir: &Directory) -> Result<Digest> {
    digest_of(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::LocalCas;
    use tempfile::TempDir;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn build_flat_directory() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join("a.txt"), b"A");
        write(&src.join("b.txt"), b"B");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let root_digest = build_input_tree(&src, &cas).unwrap();

        // Materialize into a fresh directory and assert content matches.
        let out = TempDir::new().unwrap();
        materialize_input_tree(&cas, &root_digest, out.path()).unwrap();
        assert_eq!(fs::read(out.path().join("a.txt")).unwrap(), b"A");
        assert_eq!(fs::read(out.path().join("b.txt")).unwrap(), b"B");
    }

    #[test]
    fn build_nested_directory() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join("top.txt"), b"top");
        write(&src.join("sub/one.txt"), b"one");
        write(&src.join("sub/nested/two.txt"), b"two");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let root_digest = build_input_tree(&src, &cas).unwrap();

        let out = TempDir::new().unwrap();
        materialize_input_tree(&cas, &root_digest, out.path()).unwrap();
        assert_eq!(fs::read(out.path().join("top.txt")).unwrap(), b"top");
        assert_eq!(fs::read(out.path().join("sub/one.txt")).unwrap(), b"one");
        assert_eq!(
            fs::read(out.path().join("sub/nested/two.txt")).unwrap(),
            b"two"
        );
    }

    #[test]
    fn same_content_yields_same_root_digest() {
        let mk = || {
            let tmp = TempDir::new().unwrap();
            let src = tmp.path().join("src");
            write(&src.join("a.txt"), b"A");
            write(&src.join("sub/b.txt"), b"B");
            (tmp, src)
        };
        let (_tmp1, src1) = mk();
        let (_tmp2, src2) = mk();

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let d1 = build_input_tree(&src1, &cas).unwrap();
        let d2 = build_input_tree(&src2, &cas).unwrap();
        assert_eq!(d1, d2, "identical trees must hash the same");
    }

    #[test]
    fn differing_content_yields_different_root_digest() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write(&a.join("x.txt"), b"one");
        write(&b.join("x.txt"), b"two");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let da = build_input_tree(&a, &cas).unwrap();
        let db = build_input_tree(&b, &cas).unwrap();
        assert_ne!(da, db);
    }
}
