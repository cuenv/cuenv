//! Content-Addressable Storage (CAS) for task outputs
//!
//! Provides efficient storage and retrieval of task outputs using content addressing.
//! Files are stored by their SHA-256 hash in a two-level directory structure to
//! avoid filesystem limitations with large numbers of files in a single directory.
//!
//! ## Directory Structure
//!
//! ```text
//! ~/.cache/cuenv/cas/
//!   ab/
//!     cd/
//!       abcdef123456... (actual blob)
//! ```
//!
//! ## Benefits
//!
//! - Deduplication: identical outputs stored once
//! - Integrity: SHA-256 verification on read
//! - Cross-branch sharing: content-addressed, not branch-specific
//! - Efficient lookup: two-level directory structure

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A blob identifier (SHA-256 hash as hex string)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(String);

impl BlobId {
    /// Compute blob ID from data
    pub fn from_data(data: &[u8]) -> Self {
        let hash = Sha256::digest(data);
        Self(hex::encode(hash))
    }

    /// Create from hex string (validation)
    ///
    /// # Errors
    ///
    /// Returns error if the hex string is invalid or wrong length
    pub fn from_hex(hex: impl Into<String>) -> Result<Self> {
        let s = hex.into();
        if s.len() != 64 {
            return Err(Error::validation(format!(
                "BlobId must be 64 hex characters, got {}",
                s.len()
            )));
        }
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::validation("BlobId must contain only hex digits"));
        }
        Ok(Self(s))
    }

    /// Get the hex representation
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Content-addressable storage backend
#[derive(Debug, Clone)]
pub struct CasStore {
    root: PathBuf,
}

impl CasStore {
    /// Create a new CAS store at the given root directory
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Get the path for a blob based on its ID
    ///
    /// Uses a two-level directory structure: `{root}/{id[0:2]}/{id[2:4]}/{id}`
    fn blob_path(&self, id: &BlobId) -> PathBuf {
        let hex = id.as_hex();
        self.root
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex)
    }

    /// Store a blob and return its ID
    ///
    /// # Errors
    ///
    /// Returns error if IO operations fail
    pub fn store(&self, data: &[u8]) -> Result<BlobId> {
        let id = BlobId::from_data(data);
        let path = self.blob_path(&id);

        // Check if blob already exists
        if path.exists() {
            return Ok(id);
        }

        // Create parent directories
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Io {
                source: e,
                path: Some(parent.into()),
                operation: "create_dir_all".into(),
            })?;
        }

        // Write blob atomically using a temporary file
        let tmp_path = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp_path).map_err(|e| Error::Io {
            source: e,
            path: Some(tmp_path.clone().into()),
            operation: "create".into(),
        })?;
        file.write_all(data).map_err(|e| Error::Io {
            source: e,
            path: Some(tmp_path.clone().into()),
            operation: "write".into(),
        })?;
        file.sync_all().map_err(|e| Error::Io {
            source: e,
            path: Some(tmp_path.clone().into()),
            operation: "sync".into(),
        })?;
        drop(file);

        // Atomic rename to final location
        fs::rename(&tmp_path, &path).map_err(|e| Error::Io {
            source: e,
            path: Some(path.clone().into()),
            operation: "rename".into(),
        })?;

        Ok(id)
    }

    /// Load a blob by its ID
    ///
    /// # Errors
    ///
    /// Returns error if the blob doesn't exist or IO operations fail
    pub fn load(&self, id: &BlobId) -> Result<Vec<u8>> {
        let path = self.blob_path(id);
        let data = fs::read(&path).map_err(|e| Error::Io {
            source: e,
            path: Some(path.into()),
            operation: "read".into(),
        })?;

        // Verify integrity
        let computed_id = BlobId::from_data(&data);
        if computed_id != *id {
            return Err(Error::validation(format!(
                "Blob integrity check failed: expected {}, computed {}",
                id, computed_id
            )));
        }

        Ok(data)
    }

    /// Check if a blob exists
    #[must_use]
    pub fn exists(&self, id: &BlobId) -> bool {
        self.blob_path(id).exists()
    }

    /// Get the size of a blob without loading it
    ///
    /// # Errors
    ///
    /// Returns error if the blob doesn't exist or metadata cannot be read
    pub fn size(&self, id: &BlobId) -> Result<u64> {
        let path = self.blob_path(id);
        let metadata = fs::metadata(&path).map_err(|e| Error::Io {
            source: e,
            path: Some(path.into()),
            operation: "metadata".into(),
        })?;
        Ok(metadata.len())
    }

    /// Delete a blob
    ///
    /// # Errors
    ///
    /// Returns error if IO operations fail
    pub fn delete(&self, id: &BlobId) -> Result<()> {
        let path = self.blob_path(id);
        if !path.exists() {
            return Ok(());
        }
        fs::remove_file(&path).map_err(|e| Error::Io {
            source: e,
            path: Some(path.into()),
            operation: "remove_file".into(),
        })?;
        Ok(())
    }

    /// List all blob IDs in the store
    ///
    /// # Errors
    ///
    /// Returns error if directory traversal fails
    pub fn list(&self) -> Result<Vec<BlobId>> {
        let mut blobs = Vec::new();

        if !self.root.exists() {
            return Ok(blobs);
        }

        // Walk the two-level directory structure
        for entry1 in fs::read_dir(&self.root).map_err(|e| Error::Io {
            source: e,
            path: Some(self.root.clone().into()),
            operation: "read_dir".into(),
        })? {
            let entry1 = entry1.map_err(|e| Error::Io {
                source: e,
                path: Some(self.root.clone().into()),
                operation: "read_dir_entry".into(),
            })?;
            let path1 = entry1.path();
            if !path1.is_dir() {
                continue;
            }

            for entry2 in fs::read_dir(&path1).map_err(|e| Error::Io {
                source: e,
                path: Some(path1.clone().into()),
                operation: "read_dir".into(),
            })? {
                let entry2 = entry2.map_err(|e| Error::Io {
                    source: e,
                    path: Some(path1.clone().into()),
                    operation: "read_dir_entry".into(),
                })?;
                let path2 = entry2.path();
                if !path2.is_dir() {
                    continue;
                }

                for entry3 in fs::read_dir(&path2).map_err(|e| Error::Io {
                    source: e,
                    path: Some(path2.clone().into()),
                    operation: "read_dir".into(),
                })? {
                    let entry3 = entry3.map_err(|e| Error::Io {
                        source: e,
                        path: Some(path2.clone().into()),
                        operation: "read_dir_entry".into(),
                    })?;
                    if entry3.path().is_file() {
                        if let Some(filename) = entry3.file_name().to_str() {
                            if let Ok(id) = BlobId::from_hex(filename) {
                                blobs.push(id);
                            }
                        }
                    }
                }
            }
        }

        Ok(blobs)
    }

    /// Get total size of all blobs in the store
    ///
    /// # Errors
    ///
    /// Returns error if directory traversal or metadata reading fails
    pub fn total_size(&self) -> Result<u64> {
        let blobs = self.list()?;
        let mut total = 0u64;
        for blob in &blobs {
            total += self.size(blob)?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_blob_id_from_data() {
        let data = b"hello world";
        let id = BlobId::from_data(data);
        // SHA-256 of "hello world"
        assert_eq!(
            id.as_hex(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_blob_id_validation() {
        // Valid
        assert!(BlobId::from_hex(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_ok());

        // Too short
        assert!(BlobId::from_hex("abc").is_err());

        // Invalid characters
        assert!(BlobId::from_hex(
            "xyz3456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_err());
    }

    #[test]
    fn test_store_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data = b"test data";
        let id = store.store(data).unwrap();

        assert!(store.exists(&id));

        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_store_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data = b"test data";
        let id1 = store.store(data).unwrap();
        let id2 = store.store(data).unwrap();

        assert_eq!(id1, id2);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn test_load_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let id = BlobId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert!(store.load(&id).is_err());
    }

    #[test]
    fn test_integrity_check() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        // Store data
        let data = b"test data";
        let id = store.store(data).unwrap();

        // Corrupt the blob on disk
        let path = store.blob_path(&id);
        fs::write(&path, b"corrupted").unwrap();

        // Load should fail integrity check
        assert!(store.load(&id).is_err());
    }

    #[test]
    fn test_delete() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data = b"test data";
        let id = store.store(data).unwrap();
        assert!(store.exists(&id));

        store.delete(&id).unwrap();
        assert!(!store.exists(&id));
    }

    #[test]
    fn test_list() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let id1 = store.store(b"data1").unwrap();
        let id2 = store.store(b"data2").unwrap();
        let id3 = store.store(b"data3").unwrap();

        let mut blobs = store.list().unwrap();
        blobs.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));

        let mut expected = vec![id1, id2, id3];
        expected.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));

        assert_eq!(blobs, expected);
    }

    #[test]
    fn test_size() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data = b"test data with some length";
        let id = store.store(data).unwrap();

        assert_eq!(store.size(&id).unwrap(), data.len() as u64);
    }

    #[test]
    fn test_total_size() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data1 = b"data1";
        let data2 = b"data22";
        let data3 = b"data333";

        store.store(data1).unwrap();
        store.store(data2).unwrap();
        store.store(data3).unwrap();

        let expected_size = (data1.len() + data2.len() + data3.len()) as u64;
        assert_eq!(store.total_size().unwrap(), expected_size);
    }

    #[test]
    fn test_two_level_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let store = CasStore::new(tmp.path());

        let data = b"test";
        let id = store.store(data).unwrap();

        let path = store.blob_path(&id);
        let hex = id.as_hex();

        // Check that path has correct structure: root/ab/cd/abcd...
        assert!(path.to_str().unwrap().contains(&format!("/{}/", &hex[0..2])));
        assert!(path
            .to_str()
            .unwrap()
            .contains(&format!("/{}/", &hex[2..4])));
        assert!(path.to_str().unwrap().ends_with(hex));
    }
}
