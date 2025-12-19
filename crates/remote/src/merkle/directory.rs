//! Merkle tree builder for REAPI Directory structures

use super::Digest;
use crate::error::{RemoteError, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Builder for constructing Merkle trees from file inputs
///
/// In Phase 2, this will build actual REAPI Directory protos.
/// For now, it's a placeholder that will hold the structure.
pub struct DirectoryBuilder {
    /// Root directory path
    root: PathBuf,

    /// Files to include (path -> digest)
    files: HashMap<PathBuf, Digest>,

    /// Subdirectories (path -> DirectoryBuilder)
    subdirs: HashMap<PathBuf, DirectoryBuilder>,
}

impl DirectoryBuilder {
    /// Create a new directory builder
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            files: HashMap::new(),
            subdirs: HashMap::new(),
        }
    }

    /// Add a file to the directory
    pub fn add_file(&mut self, path: impl AsRef<Path>, digest: Digest) -> Result<()> {
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

        // If file is in a subdirectory, delegate to subdir builder
        if let Some(parent) = rel_path.parent() {
            if parent != Path::new("") {
                let subdir = self
                    .subdirs
                    .entry(parent.to_path_buf())
                    .or_insert_with(|| DirectoryBuilder::new(self.root.join(parent)));
                return subdir.add_file(rel_path.file_name().unwrap(), digest);
            }
        }

        // File is directly in this directory
        self.files.insert(rel_path, digest);
        Ok(())
    }

    /// Build the Merkle tree and return the root digest
    ///
    /// In Phase 2, this will serialize to REAPI Directory proto and compute digest.
    /// For now, it returns a placeholder.
    pub fn build(&self) -> Result<Digest> {
        // TODO: In Phase 2, implement actual proto serialization:
        // 1. Sort files and directories by name (CRITICAL for determinism)
        // 2. Build FileNode and DirectoryNode messages
        // 3. Recursively build subdirectories
        // 4. Serialize Directory proto to bytes
        // 5. Compute SHA256 digest

        // Placeholder: just return empty digest
        Ok(Digest::default())
    }

    /// Get all file digests (recursively)
    pub fn get_all_files(&self) -> Vec<(PathBuf, Digest)> {
        let mut files = Vec::new();

        // Add files from this directory
        for (path, digest) in &self.files {
            files.push((path.clone(), digest.clone()));
        }

        // Add files from subdirectories
        for (subdir_path, subdir) in &self.subdirs {
            for (file_path, digest) in subdir.get_all_files() {
                files.push((subdir_path.join(file_path), digest));
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

        builder
            .add_file("subdir/file.txt", digest.clone())
            .unwrap();
        assert_eq!(builder.file_count(), 1);

        let files = builder.get_all_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, PathBuf::from("subdir/file.txt"));
    }

    #[test]
    fn test_build_placeholder() {
        let mut builder = DirectoryBuilder::new("/tmp/test");
        builder
            .add_file("file.txt", Digest::from_bytes(b"hello"))
            .unwrap();

        let digest = builder.build().unwrap();
        // For now, just check it returns something
        assert!(digest.hash.len() == 64);
    }
}
