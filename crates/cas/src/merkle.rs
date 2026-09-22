//! Merkle-tree construction over a directory.
//!
//! [`build_input_tree`] walks a directory, inserts every file into a [`Cas`]
//! and builds the corresponding [`Directory`] messages bottom-up. The root
//! digest identifies the entire input tree and is what an
//! [`Action`](crate::message::Action) references as its `input_root_digest`.

use crate::cas::Cas;
use crate::digest::{Digest, digest_of};
use crate::error::{Error, Result};
use crate::message::{Directory, DirectoryNode, FileNode, Tree};
use crate::reapi::CanonicalMessage;
use async_recursion::async_recursion;
use bazel_remote_apis::build::bazel::remote::execution::v2::{
    Directory as PbDirectory, Tree as PbTree,
};
use std::collections::{BTreeMap, HashMap};
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
/// Returns an error if any filesystem operation fails, if a child name is not
/// valid UTF-8, if a child blob cannot be stored in `cas`, or if a
/// [`Directory`] message cannot be serialized.
pub async fn build_input_tree(root: &Path, cas: &dyn Cas) -> Result<Digest> {
    let tree = build_directory(root, cas).await?;
    cas.put_bytes(&tree.to_canonical_bytes()?).await
}

/// Materialize a previously-built input tree to `destination`. The destination
/// directory is created if it does not exist.
///
/// # Errors
///
/// Returns an error if any blob is missing from `cas`, if a [`Directory`]
/// message cannot be decoded, or if any filesystem operation fails.
pub async fn materialize_input_tree(
    cas: &dyn Cas,
    root_digest: &Digest,
    destination: &Path,
) -> Result<()> {
    let dir = decode_directory(&cas.get(root_digest).await?)?;
    fs::create_dir_all(destination).map_err(|e| Error::io(e, destination, "create_dir_all"))?;
    materialize_directory(cas, &dir, destination).await
}

/// Build a REAPI [`Tree`] for an output directory and store it in `cas`.
///
/// File contents are stored as ordinary CAS blobs. Descendant directories
/// are embedded in the `Tree` message as REAPI requires, preserving empty
/// directories without relying on separate `Directory` blobs.
///
/// # Errors
///
/// Returns an error for filesystem failures, non-UTF-8 names, symlinks or
/// special files, and CAS persistence failures.
pub async fn build_output_tree(root: &Path, cas: &dyn Cas) -> Result<Digest> {
    let (root, children) = build_tree_directory(root, cas).await?;
    let tree = Tree { root, children };
    cas.put_bytes(&tree.to_canonical_bytes()?).await
}

/// Materialize a REAPI output [`Tree`] at `destination`.
///
/// # Errors
///
/// Returns an error if the tree is malformed, references a missing child,
/// contains unsafe names or symlinks, or any file blob cannot be restored.
pub async fn materialize_output_tree(
    cas: &dyn Cas,
    tree_digest: &Digest,
    destination: &Path,
) -> Result<()> {
    let tree = decode_tree(&cas.get(tree_digest).await?)?;
    let mut children = HashMap::with_capacity(tree.children.len());
    for child in tree.children {
        children.insert(directory_digest(&child)?, child);
    }
    fs::create_dir_all(destination).map_err(|e| Error::io(e, destination, "create_dir_all"))?;
    materialize_tree_directory(cas, &tree.root, &children, destination).await
}

#[async_recursion]
async fn build_directory(dir: &Path, cas: &dyn Cas) -> Result<Directory> {
    // Collect children first so we can sort deterministically.
    let mut files: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();
    let mut subdirs: BTreeMap<String, PathBuf> = BTreeMap::new();

    let entries = fs::read_dir(dir).map_err(|e| Error::io(e, dir, "read_dir"))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(e, dir, "read_dir_entry"))?;
        let name = reapi_name(&entry)?;
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
        let digest = cas.put_file(&path).await?;
        file_nodes.push(FileNode {
            name,
            digest,
            is_executable,
        });
    }

    let mut dir_nodes: Vec<DirectoryNode> = Vec::with_capacity(subdirs.len());
    for (name, path) in subdirs {
        let child = build_directory(&path, cas).await?;
        let digest = cas.put_bytes(&child.to_canonical_bytes()?).await?;
        dir_nodes.push(DirectoryNode { name, digest });
    }

    Ok(Directory {
        files: file_nodes,
        directories: dir_nodes,
        symlinks: Vec::new(),
    })
}

#[async_recursion]
async fn build_tree_directory(dir: &Path, cas: &dyn Cas) -> Result<(Directory, Vec<Directory>)> {
    let mut files: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();
    let mut subdirs: BTreeMap<String, PathBuf> = BTreeMap::new();

    let entries = fs::read_dir(dir).map_err(|e| Error::io(e, dir, "read_dir"))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(e, dir, "read_dir_entry"))?;
        let name = reapi_name(&entry)?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| Error::io(e, &path, "file_type"))?;
        if file_type.is_dir() {
            subdirs.insert(name, path);
        } else if file_type.is_file() {
            files.insert(name, (path.clone(), is_executable(&path)?));
        } else {
            return Err(Error::configuration(format!(
                "output tree contains unsupported symlink or special file: {}",
                path.display()
            )));
        }
    }

    let mut file_nodes = Vec::with_capacity(files.len());
    for (name, (path, is_executable)) in files {
        file_nodes.push(FileNode {
            name,
            digest: cas.put_file(&path).await?,
            is_executable,
        });
    }

    let mut directory_nodes = Vec::with_capacity(subdirs.len());
    let mut children = Vec::new();
    for (name, path) in subdirs {
        let (child, descendants) = build_tree_directory(&path, cas).await?;
        let digest = directory_digest(&child)?;
        directory_nodes.push(DirectoryNode { name, digest });
        children.push(child);
        children.extend(descendants);
    }

    Ok((
        Directory {
            files: file_nodes,
            directories: directory_nodes,
            symlinks: Vec::new(),
        },
        children,
    ))
}

fn reapi_name(entry: &fs::DirEntry) -> Result<String> {
    let path = entry.path();
    entry.file_name().into_string().map_err(|_| {
        Error::configuration(format!(
            "REAPI path names must be valid UTF-8: {}",
            path.display()
        ))
    })
}

#[async_recursion]
async fn materialize_directory(cas: &dyn Cas, dir: &Directory, destination: &Path) -> Result<()> {
    for file in &dir.files {
        let dst = destination.join(&file.name);
        cas.get_to_file(&file.digest, &dst).await?;
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
        let sub = decode_directory(&cas.get(&child.digest).await?)?;
        materialize_directory(cas, &sub, &dst).await?;
    }
    Ok(())
}

#[async_recursion]
async fn materialize_tree_directory(
    cas: &dyn Cas,
    directory: &Directory,
    children: &HashMap<Digest, Directory>,
    destination: &Path,
) -> Result<()> {
    if !directory.symlinks.is_empty() {
        return Err(Error::serialization(
            "output Tree symlinks are not supported".to_string(),
        ));
    }
    for file in &directory.files {
        let name = safe_node_name(&file.name)?;
        let destination = destination.join(name);
        cas.get_to_file(&file.digest, &destination).await?;
        #[cfg(unix)]
        if file.is_executable {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&destination)
                .map_err(|e| Error::io(e, &destination, "metadata"))?
                .permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            fs::set_permissions(&destination, permissions)
                .map_err(|e| Error::io(e, &destination, "set_permissions"))?;
        }
    }
    for child in &directory.directories {
        let name = safe_node_name(&child.name)?;
        let child_directory = children.get(&child.digest).ok_or_else(|| {
            Error::serialization(format!(
                "output Tree is missing child directory {} ({})",
                child.name, child.digest
            ))
        })?;
        let destination = destination.join(name);
        fs::create_dir_all(&destination)
            .map_err(|e| Error::io(e, &destination, "create_dir_all"))?;
        materialize_tree_directory(cas, child_directory, children, &destination).await?;
    }
    Ok(())
}

fn safe_node_name(name: &str) -> Result<&str> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(name),
        _ => Err(Error::serialization(format!(
            "unsafe output Tree node name: {name:?}"
        ))),
    }
}

/// Decode a `Directory` blob from its REAPI protobuf encoding.
///
/// # Errors
///
/// Returns an error if the bytes are not a valid `Directory` message or a
/// child digest is missing.
pub fn decode_directory(bytes: &[u8]) -> Result<Directory> {
    let proto = <PbDirectory as prost::Message>::decode(bytes)
        .map_err(|e| Error::serialization(format!("decode Directory: {e}")))?;
    Directory::from_proto(&proto)
}

/// Decode a `Tree` blob from its REAPI protobuf encoding.
///
/// # Errors
///
/// Returns an error if the bytes are not a valid `Tree`, the root is absent,
/// or a child digest is malformed.
pub fn decode_tree(bytes: &[u8]) -> Result<Tree> {
    let proto = <PbTree as prost::Message>::decode(bytes)
        .map_err(|e| Error::serialization(format!("decode Tree: {e}")))?;
    Tree::from_proto(&proto)
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

    #[tokio::test]
    async fn build_flat_directory() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join("a.txt"), b"A");
        write(&src.join("b.txt"), b"B");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let root_digest = build_input_tree(&src, &cas).await.unwrap();

        // Materialize into a fresh directory and assert content matches.
        let out = TempDir::new().unwrap();
        materialize_input_tree(&cas, &root_digest, out.path())
            .await
            .unwrap();
        assert_eq!(fs::read(out.path().join("a.txt")).unwrap(), b"A");
        assert_eq!(fs::read(out.path().join("b.txt")).unwrap(), b"B");
    }

    #[tokio::test]
    async fn build_nested_directory() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join("top.txt"), b"top");
        write(&src.join("sub/one.txt"), b"one");
        write(&src.join("sub/nested/two.txt"), b"two");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let root_digest = build_input_tree(&src, &cas).await.unwrap();

        let out = TempDir::new().unwrap();
        materialize_input_tree(&cas, &root_digest, out.path())
            .await
            .unwrap();
        assert_eq!(fs::read(out.path().join("top.txt")).unwrap(), b"top");
        assert_eq!(fs::read(out.path().join("sub/one.txt")).unwrap(), b"one");
        assert_eq!(
            fs::read(out.path().join("sub/nested/two.txt")).unwrap(),
            b"two"
        );
    }

    #[tokio::test]
    async fn same_content_yields_same_root_digest() {
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
        let d1 = build_input_tree(&src1, &cas).await.unwrap();
        let d2 = build_input_tree(&src2, &cas).await.unwrap();
        assert_eq!(d1, d2, "identical trees must hash the same");
    }

    #[tokio::test]
    async fn differing_content_yields_different_root_digest() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write(&a.join("x.txt"), b"one");
        write(&b.join("x.txt"), b"two");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let da = build_input_tree(&a, &cas).await.unwrap();
        let db = build_input_tree(&b, &cas).await.unwrap();
        assert_ne!(da, db);
    }

    // APFS refuses to create a name that is not valid UTF-8, so the setup
    // itself fails on macOS; the check under test is exercised on Linux.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn input_tree_rejects_non_utf8_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join(OsStr::from_bytes(b"invalid-\xff")), b"content");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let error = build_input_tree(&src, &cas).await.unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }

    // APFS refuses to create a name that is not valid UTF-8, so the setup
    // itself fails on macOS; the check under test is exercised on Linux.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn output_tree_rejects_non_utf8_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        write(&src.join(OsStr::from_bytes(b"invalid-\xff")), b"content");

        let cas_root = TempDir::new().unwrap();
        let cas = LocalCas::open(cas_root.path()).unwrap();
        let error = build_output_tree(&src, &cas).await.unwrap_err();

        assert!(error.to_string().contains("must be valid UTF-8"));
    }
}
