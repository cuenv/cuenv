//! [`Cas`] trait and local on-disk implementation.

use crate::digest::Digest;
use crate::error::{Error, Result};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tracing::trace;

/// A content-addressed blob store.
///
/// Implementations must be safe to use from multiple threads concurrently.
pub trait Cas: Send + Sync {
    /// True if the store holds a blob with `digest`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying storage cannot be queried.
    fn contains(&self, digest: &Digest) -> Result<bool>;

    /// Load the blob at `digest` into memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the digest is not present, or an
    /// I/O error if the blob cannot be read.
    fn get(&self, digest: &Digest) -> Result<Vec<u8>>;

    /// Copy the blob at `digest` to `destination`. The parent directory must
    /// already exist.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the digest is not present, or an
    /// I/O error if the copy/link fails.
    fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()>;

    /// Store `bytes` and return its digest.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the blob cannot be written to the store.
    fn put_bytes(&self, bytes: &[u8]) -> Result<Digest>;

    /// Stream a file into the store and return its digest. The source file
    /// is read but not modified.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the source cannot be read or the blob
    /// cannot be written to the store.
    fn put_file(&self, source: &Path) -> Result<Digest>;
}

/// A blob store rooted at a local directory.
///
/// Layout:
///
/// ```text
/// root/
///   cas/sha256/<ab>/<cdef...>    blob files (name = rest of hex digest)
///   tmp/                          staging area for atomic writes
/// ```
#[derive(Debug, Clone)]
pub struct LocalCas {
    root: PathBuf,
}

impl LocalCas {
    /// Open or create a local CAS rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the required directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let cas_dir = root.join("cas").join("sha256");
        let tmp_dir = root.join("tmp");
        fs::create_dir_all(&cas_dir).map_err(|e| Error::io(e, &cas_dir, "create_dir_all"))?;
        fs::create_dir_all(&tmp_dir).map_err(|e| Error::io(e, &tmp_dir, "create_dir_all"))?;
        Ok(Self { root })
    }

    /// Root directory of this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compute the on-disk path for `digest`.
    #[must_use]
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        let (prefix, rest) = digest.hash.split_at(2);
        self.root.join("cas").join("sha256").join(prefix).join(rest)
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn verify_bytes(digest: &Digest, bytes: &[u8]) -> Result<()> {
        let actual = Digest::of_bytes(bytes);
        if &actual != digest {
            return Err(Error::digest_mismatch(
                digest.to_resource(),
                actual.to_resource(),
            ));
        }
        Ok(())
    }

    fn verify_file(path: &Path, digest: &Digest) -> Result<()> {
        let mut file = fs::File::open(path).map_err(|e| Error::io(e, path, "open"))?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buffer: Box<[u8]> = vec![0u8; 64 * 1024].into_boxed_slice();

        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|e| Error::io(e, path, "read"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            size += count as u64;
        }

        let actual = Digest {
            hash: hex::encode(hasher.finalize()),
            size_bytes: size,
        };
        if &actual != digest {
            return Err(Error::digest_mismatch(
                digest.to_resource(),
                actual.to_resource(),
            ));
        }
        Ok(())
    }

    /// Atomically rename `src` into `dst`, tolerating the case where another
    /// writer populated the same digest concurrently.
    fn install(src: &Path, dst: &Path) -> Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(e, parent, "create_dir_all"))?;
        }
        if dst.exists() {
            // Content-addressed: same path ⇒ same content. Drop the temp.
            let _ = fs::remove_file(src);
            return Ok(());
        }
        match fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(src);
                Ok(())
            }
            Err(e) if e.raw_os_error() == Some(EXDEV) => {
                // Cross-device rename isn't supported; copy then drop temp.
                fs::copy(src, dst).map_err(|e2| Error::io(e2, dst, "copy"))?;
                let _ = fs::remove_file(src);
                Ok(())
            }
            Err(e) => Err(Error::io(e, dst, "rename")),
        }
    }
}

impl Cas for LocalCas {
    fn contains(&self, digest: &Digest) -> Result<bool> {
        Ok(self.blob_path(digest).exists())
    }

    fn get(&self, digest: &Digest) -> Result<Vec<u8>> {
        let path = self.blob_path(digest);
        match fs::read(&path) {
            Ok(bytes) => {
                Self::verify_bytes(digest, &bytes)?;
                Ok(bytes)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(Error::not_found(digest.hash.clone()))
            }
            Err(e) => Err(Error::io(e, &path, "read")),
        }
    }

    fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
        let src = self.blob_path(digest);
        if !src.exists() {
            return Err(Error::not_found(digest.hash.clone()));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(e, parent, "create_dir_all"))?;
        }
        fs::copy(&src, destination).map_err(|e| Error::io(e, destination, "copy"))?;
        Self::verify_file(destination, digest)
    }

    fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = Digest::of_bytes(bytes);
        let dst = self.blob_path(&digest);
        if dst.exists() {
            trace!(digest = %digest, "CAS put_bytes: already present");
            return Ok(digest);
        }
        let tmp_dir = self.tmp_dir();
        let mut tmp = tempfile::NamedTempFile::new_in(&tmp_dir)
            .map_err(|e| Error::io(e, &tmp_dir, "tempfile"))?;
        tmp.write_all(bytes)
            .map_err(|e| Error::io(e, tmp.path(), "write"))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| Error::io(e, tmp.path(), "fsync"))?;
        let (_, tmp_path) = tmp
            .keep()
            .map_err(|e| Error::io(e.error, &tmp_dir, "keep"))?;
        Self::install(&tmp_path, &dst)?;
        trace!(digest = %digest, "CAS put_bytes: installed");
        Ok(digest)
    }

    fn put_file(&self, source: &Path) -> Result<Digest> {
        // Pass 1: streaming sha256 + size, no copy yet.
        let mut file = fs::File::open(source).map_err(|e| Error::io(e, source, "open"))?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buf: Box<[u8]> = vec![0u8; 64 * 1024].into_boxed_slice();
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| Error::io(e, source, "read"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let digest = Digest {
            hash: hex::encode(hasher.finalize()),
            size_bytes: size,
        };
        let dst = self.blob_path(&digest);
        if dst.exists() {
            trace!(digest = %digest, source = %source.display(), "CAS put_file: already present");
            return Ok(digest);
        }

        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(e, parent, "create_dir_all"))?;
        }
        let tmp_dir = self.tmp_dir();
        let tmp = tempfile::NamedTempFile::new_in(&tmp_dir)
            .map_err(|e| Error::io(e, &tmp_dir, "tempfile"))?;
        fs::copy(source, tmp.path()).map_err(|e| Error::io(e, tmp.path(), "copy"))?;
        let (_, tmp_path) = tmp
            .keep()
            .map_err(|e| Error::io(e.error, &tmp_dir, "keep"))?;
        Self::install(&tmp_path, &dst)?;
        trace!(digest = %digest, "CAS put_file: copied");
        Ok(digest)
    }
}

#[cfg(target_family = "unix")]
const EXDEV: i32 = 18;

#[cfg(not(target_family = "unix"))]
const EXDEV: i32 = -1;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn put_and_get_bytes() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = cas.put_bytes(b"hello cas").unwrap();
        assert!(cas.contains(&digest).unwrap());
        assert_eq!(cas.get(&digest).unwrap(), b"hello cas");
    }

    #[test]
    fn put_bytes_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let a = cas.put_bytes(b"same").unwrap();
        let b = cas.put_bytes(b"same").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn put_file_matches_put_bytes() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let src = tmp.path().join("src.txt");
        fs::write(&src, b"from disk").unwrap();
        let d_file = cas.put_file(&src).unwrap();
        let d_bytes = Digest::of_bytes(b"from disk");
        assert_eq!(d_file, d_bytes);
        assert!(cas.contains(&d_file).unwrap());
    }

    #[test]
    fn get_to_file_materializes_content() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = cas.put_bytes(b"materialize me").unwrap();
        let dst = tmp.path().join("out/file.bin");
        cas.get_to_file(&digest, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"materialize me");
    }

    #[test]
    fn get_detects_corrupted_blob() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = cas.put_bytes(b"immutable").unwrap();
        fs::write(cas.blob_path(&digest), b"mutated").unwrap();

        let err = cas.get(&digest).unwrap_err();
        assert!(matches!(err, Error::DigestMismatch { .. }));
    }

    #[test]
    fn mutating_materialized_file_does_not_corrupt_cas_blob() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let digest = cas.put_bytes(b"original").unwrap();
        let dst = tmp.path().join("out/file.bin");

        cas.get_to_file(&digest, &dst).unwrap();
        fs::write(&dst, b"modified").unwrap();

        assert_eq!(cas.get(&digest).unwrap(), b"original");
    }

    #[test]
    fn mutating_source_after_put_file_does_not_corrupt_cas_blob() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let src = tmp.path().join("src.txt");
        fs::write(&src, b"from disk").unwrap();

        let digest = cas.put_file(&src).unwrap();
        fs::write(&src, b"changed later").unwrap();

        assert_eq!(cas.get(&digest).unwrap(), b"from disk");
    }

    #[test]
    fn get_missing_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let bogus = Digest::of_bytes(b"never written");
        let err = cas.get(&bogus).unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }));
    }

    #[test]
    fn contains_reflects_state() {
        let tmp = TempDir::new().unwrap();
        let cas = LocalCas::open(tmp.path()).unwrap();
        let d = Digest::of_bytes(b"x");
        assert!(!cas.contains(&d).unwrap());
        cas.put_bytes(b"x").unwrap();
        assert!(cas.contains(&d).unwrap());
    }
}
