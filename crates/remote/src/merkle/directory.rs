//! Merkle tree builder for REAPI Directory structures

use super::Digest;
use crate::error::{RemoteError, Result};
use crate::reapi::{self, DirectoryNode, FileNode, SymlinkNode};
use prost::Message;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entry representing a file with its metadata
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Content digest of the file
    pub digest: Digest,
    /// Whether the file is executable
    pub is_executable: bool,
}

/// Entry representing a symlink
#[derive(Debug, Clone)]
pub struct SymlinkEntry {
    /// Target path of the symlink
    pub target: PathBuf,
}

/// Builder for constructing Merkle trees from file inputs
///
/// Builds REAPI Directory protos with deterministic ordering for content-addressed caching.
pub struct DirectoryBuilder {
    /// Root directory path
    root: PathBuf,

    /// Files to include (name -> entry)
    files: HashMap<String, FileEntry>,

    /// Symlinks to include (name -> entry)
    symlinks: HashMap<String, SymlinkEntry>,

    /// Subdirectories (name -> DirectoryBuilder)
    subdirs: HashMap<String, DirectoryBuilder>,
}

/// Result of building a directory tree
pub struct DirectoryTree {
    /// Root digest of the tree
    pub root_digest: Digest,
    /// All directories with their serialized bytes (digest -> bytes)
    pub directories: Vec<(Digest, Vec<u8>)>,
}

impl DirectoryBuilder {
    /// Create a new directory builder
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            files: HashMap::new(),
            symlinks: HashMap::new(),
            subdirs: HashMap::new(),
        }
    }

    /// Add a file to the directory
    pub fn add_file(&mut self, path: impl AsRef<Path>, digest: Digest) -> Result<()> {
        self.add_file_with_permissions(path, digest, false)
    }

    /// Add a symlink to the directory
    pub fn add_symlink(&mut self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let target = target.as_ref();

        // Ensure path is relative to root
        let rel_path = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| {
                    RemoteError::merkle_error(format!(
                        "Symlink path {:?} is not under root {:?}",
                        path, self.root
                    ))
                })?
                .to_path_buf()
        } else {
            path.to_path_buf()
        };

        // Split into components
        let components: Vec<_> = rel_path.components().collect();

        if components.is_empty() {
            return Err(RemoteError::merkle_error("Empty symlink path"));
        }

        if components.len() == 1 {
            // Symlink is directly in this directory
            let name = components[0].as_os_str().to_string_lossy().to_string();
            self.symlinks.insert(
                name,
                SymlinkEntry {
                    target: target.to_path_buf(),
                },
            );
            Ok(())
        } else {
            // Symlink is in a subdirectory - delegate
            let subdir_name = components[0].as_os_str().to_string_lossy().to_string();
            let remaining: PathBuf = components[1..].iter().collect();

            let subdir = self
                .subdirs
                .entry(subdir_name.clone())
                .or_insert_with(|| DirectoryBuilder::new(self.root.join(&subdir_name)));
            subdir.add_symlink(&remaining, target)
        }
    }

    /// Add a file with executable permission to the directory
    pub fn add_file_with_permissions(
        &mut self,
        path: impl AsRef<Path>,
        digest: Digest,
        is_executable: bool,
    ) -> Result<()> {
        let path = path.as_ref();

        // Ensure path is relative to root
        let rel_path = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| {
                    RemoteError::merkle_error(format!(
                        "File path {:?} is not under root {:?}",
                        path, self.root
                    ))
                })?
                .to_path_buf()
        } else {
            path.to_path_buf()
        };

        // Split into components
        let components: Vec<_> = rel_path.components().collect();

        if components.is_empty() {
            return Err(RemoteError::merkle_error("Empty file path"));
        }

        if components.len() == 1 {
            // File is directly in this directory
            let name = components[0].as_os_str().to_string_lossy().to_string();
            self.files.insert(
                name,
                FileEntry {
                    digest,
                    is_executable,
                },
            );
            Ok(())
        } else {
            // File is in a subdirectory - delegate
            let subdir_name = components[0].as_os_str().to_string_lossy().to_string();
            let remaining: PathBuf = components[1..].iter().collect();

            let subdir = self
                .subdirs
                .entry(subdir_name.clone())
                .or_insert_with(|| DirectoryBuilder::new(self.root.join(&subdir_name)));
            subdir.add_file_with_permissions(&remaining, digest, is_executable)
        }
    }

    /// Build the Merkle tree and return the root digest with all directory blobs
    ///
    /// This serializes to REAPI Directory protos with sorted entries for determinism.
    pub fn build(&self) -> Result<DirectoryTree> {
        let mut all_directories = Vec::new();
        let root_digest = self.build_recursive(&mut all_directories)?;

        Ok(DirectoryTree {
            root_digest,
            directories: all_directories,
        })
    }

    /// Recursively build directories bottom-up
    fn build_recursive(&self, all_directories: &mut Vec<(Digest, Vec<u8>)>) -> Result<Digest> {
        // First, recursively build all subdirectories
        let mut directory_nodes: Vec<DirectoryNode> = Vec::new();
        let mut subdir_names: Vec<_> = self.subdirs.keys().collect();
        subdir_names.sort(); // Sort for determinism

        for name in subdir_names {
            let subdir = &self.subdirs[name];
            let subdir_digest = subdir.build_recursive(all_directories)?;

            directory_nodes.push(DirectoryNode {
                name: name.clone(),
                digest: Some(digest_to_proto(&subdir_digest)),
            });
        }

        // Build file nodes (sorted by name)
        let mut file_nodes: Vec<FileNode> = Vec::new();
        let mut file_names: Vec<_> = self.files.keys().collect();
        file_names.sort(); // Sort for determinism

        for name in file_names {
            let entry = &self.files[name];
            file_nodes.push(FileNode {
                name: name.clone(),
                digest: Some(digest_to_proto(&entry.digest)),
                is_executable: entry.is_executable,
                node_properties: None,
            });
        }

        // Build symlink nodes (sorted by name)
        let mut symlink_nodes: Vec<SymlinkNode> = Vec::new();
        let mut symlink_names: Vec<_> = self.symlinks.keys().collect();
        symlink_names.sort(); // Sort for determinism

        for name in symlink_names {
            let entry = &self.symlinks[name];
            symlink_nodes.push(SymlinkNode {
                name: name.clone(),
                target: entry.target.to_string_lossy().to_string(),
                node_properties: None,
            });
        }

        // Create the Directory proto
        let directory = reapi::Directory {
            files: file_nodes,
            directories: directory_nodes,
            symlinks: symlink_nodes,
            node_properties: None,
        };

        // Serialize and compute digest
        let bytes = directory.encode_to_vec();
        let digest = Digest::from_bytes(&bytes);

        // Add to collection
        all_directories.push((digest.clone(), bytes));

        Ok(digest)
    }

    /// Get all file digests (recursively)
    pub fn get_all_files(&self) -> Vec<(PathBuf, Digest)> {
        let mut files = Vec::new();

        // Add files from this directory
        for (name, entry) in &self.files {
            files.push((PathBuf::from(name), entry.digest.clone()));
        }

        // Add files from subdirectories
        for (subdir_name, subdir) in &self.subdirs {
            for (file_path, digest) in subdir.get_all_files() {
                files.push((PathBuf::from(subdir_name).join(file_path), digest));
            }
        }

        files
    }

    /// Get the number of files in this tree
    pub fn file_count(&self) -> usize {
        let mut count = self.files.len();
        for subdir in self.subdirs.values() {
            count += subdir.file_count();
        }
        count
    }
}

/// Convert our Digest to proto Digest
fn digest_to_proto(digest: &Digest) -> reapi::Digest {
    reapi::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_builder() {
        let builder = DirectoryBuilder::new("/tmp/test");
        assert_eq!(builder.file_count(), 0);
    }

    #[test]
    fn test_add_file() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        let digest = Digest::from_bytes(b"hello");

        builder.add_file("file.txt", digest.clone()).unwrap();
        assert_eq!(builder.file_count(), 1);

        let files = builder.get_all_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, PathBuf::from("file.txt"));
        assert_eq!(files[0].1, digest);
    }

    #[test]
    fn test_add_file_in_subdir() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        let digest = Digest::from_bytes(b"hello");

        builder.add_file("subdir/file.txt", digest.clone()).unwrap();
        assert_eq!(builder.file_count(), 1);

        let files = builder.get_all_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, PathBuf::from("subdir/file.txt"));
    }

    #[test]
    fn test_build_tree() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        builder
            .add_file("file.txt", Digest::from_bytes(b"hello"))
            .unwrap();

        let tree = builder.build().unwrap();

        // Check root digest is valid
        assert!(tree.root_digest.hash.len() == 64);

        // Should have at least one directory (the root)
        assert!(!tree.directories.is_empty());
    }

    #[test]
    fn test_build_nested_tree() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        builder.add_file("a.txt", Digest::from_bytes(b"a")).unwrap();
        builder
            .add_file("subdir/b.txt", Digest::from_bytes(b"b"))
            .unwrap();

        let tree = builder.build().unwrap();

        // Should have 2 directories: root and subdir
        assert_eq!(tree.directories.len(), 2);
    }

    #[test]
    fn test_deterministic_order() {
        // Add files in different orders, should get same digest
        let mut builder1 = DirectoryBuilder::new("/tmp/test");
        builder1
            .add_file("z.txt", Digest::from_bytes(b"z"))
            .unwrap();
        builder1
            .add_file("a.txt", Digest::from_bytes(b"a"))
            .unwrap();

        let mut builder2 = DirectoryBuilder::new("/tmp/test");
        builder2
            .add_file("a.txt", Digest::from_bytes(b"a"))
            .unwrap();
        builder2
            .add_file("z.txt", Digest::from_bytes(b"z"))
            .unwrap();

        let tree1 = builder1.build().unwrap();
        let tree2 = builder2.build().unwrap();

        assert_eq!(tree1.root_digest.hash, tree2.root_digest.hash);
    }

    #[test]
    fn test_add_symlink() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        builder
            .add_symlink("link", "/nix/store/abc123/bin/cargo")
            .unwrap();
        builder
            .add_file("file.txt", Digest::from_bytes(b"hello"))
            .unwrap();

        let tree = builder.build().unwrap();

        // Should have one directory (root) with symlink and file
        assert_eq!(tree.directories.len(), 1);
        assert!(tree.root_digest.hash.len() == 64);
    }

    #[test]
    fn test_add_symlink_in_subdir() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        builder
            .add_symlink("subdir/link", "/nix/store/xyz/lib/libfoo.so")
            .unwrap();

        let tree = builder.build().unwrap();

        // Should have 2 directories: root and subdir
        assert_eq!(tree.directories.len(), 2);
    }

    #[test]
    fn test_symlinks_deterministic_order() {
        // Add symlinks in different orders, should get same digest
        let mut builder1 = DirectoryBuilder::new("/tmp/test");
        builder1.add_symlink("z-link", "/target/z").unwrap();
        builder1.add_symlink("a-link", "/target/a").unwrap();

        let mut builder2 = DirectoryBuilder::new("/tmp/test");
        builder2.add_symlink("a-link", "/target/a").unwrap();
        builder2.add_symlink("z-link", "/target/z").unwrap();

        let tree1 = builder1.build().unwrap();
        let tree2 = builder2.build().unwrap();

        assert_eq!(tree1.root_digest.hash, tree2.root_digest.hash);
    }
}
