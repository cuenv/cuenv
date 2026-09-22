//! [`ActionCache`] trait and a local on-disk implementation.
//!
//! The action cache maps an [`Action`](crate::message::Action) digest to the
//! [`ActionResult`](crate::message::ActionResult) of a previous execution.

use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::message::ActionResult;
use crate::reapi::CanonicalMessage;
use async_trait::async_trait;
use bazel_remote_apis::build::bazel::remote::execution::v2::ActionResult as Pb;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::trace;

/// A key/value store mapping action digests to [`ActionResult`] records.
#[async_trait]
pub trait ActionCache: Send + Sync {
    /// Look up the result recorded for `action_digest`, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying storage fails or the stored
    /// [`ActionResult`] cannot be decoded.
    async fn lookup(&self, action_digest: &Digest) -> Result<Option<ActionResult>>;

    /// Record `result` as the outcome of `action_digest`. Overwrites any
    /// existing entry (last writer wins).
    ///
    /// # Errors
    ///
    /// Returns an error if the result cannot be encoded or persisted.
    async fn update(&self, action_digest: &Digest, result: &ActionResult) -> Result<()>;

    /// Persist a result only after its referenced blobs have been fetched,
    /// verified, and committed successfully.
    ///
    /// Most caches store it like a normal update. A layered remote cache
    /// overrides this to promote the verified candidate into its local action
    /// cache without publishing the same result back to the remote.
    async fn commit_verified(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        self.update(action_digest, result).await
    }
}

/// Filesystem-backed action cache, laid out as:
///
/// ```text
/// root/ac/sha256/<ab>/<cdef...>    protobuf-encoded REAPI ActionResult
/// root/tmp/                         staging for atomic writes
/// ```
///
/// Entries are stored in the same wire format a REAPI server exchanges, so
/// the local cache and a remote one hold byte-identical records.
#[derive(Debug, Clone)]
pub struct LocalActionCache {
    root: PathBuf,
}

impl LocalActionCache {
    /// Open or create an action cache rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the required directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let ac_dir = root.join("ac").join("sha256");
        let tmp_dir = root.join("tmp");
        fs::create_dir_all(&ac_dir).map_err(|e| Error::io(e, &ac_dir, "create_dir_all"))?;
        fs::create_dir_all(&tmp_dir).map_err(|e| Error::io(e, &tmp_dir, "create_dir_all"))?;
        Ok(Self { root })
    }

    /// Root directory of this action cache.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// On-disk path for a given action digest.
    ///
    /// # Errors
    ///
    /// Returns an error when an untrusted digest is not canonical SHA-256.
    pub fn entry_path(&self, action_digest: &Digest) -> Result<PathBuf> {
        action_digest.validate()?;
        let (prefix, rest) = action_digest.hash.split_at(2);
        Ok(self.root.join("ac").join("sha256").join(prefix).join(rest))
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }
}

#[async_trait]
impl ActionCache for LocalActionCache {
    async fn lookup(&self, action_digest: &Digest) -> Result<Option<ActionResult>> {
        let path = self.entry_path(action_digest)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let proto = <Pb as prost::Message>::decode(bytes.as_slice()).map_err(|e| {
                    Error::serialization(format!(
                        "failed to decode ActionResult at {}: {e}",
                        path.display()
                    ))
                })?;
                let result = ActionResult::from_proto(&proto)?;
                trace!(action = %action_digest, "action cache hit");
                Ok(Some(result))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                trace!(action = %action_digest, "action cache miss");
                Ok(None)
            }
            Err(e) => Err(Error::io(e, &path, "read")),
        }
    }

    async fn update(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        let path = self.entry_path(action_digest)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(e, parent, "create_dir_all"))?;
        }
        let bytes = result.to_canonical_bytes()?;
        let tmp_dir = self.tmp_dir();
        let mut tmp = tempfile::NamedTempFile::new_in(&tmp_dir)
            .map_err(|e| Error::io(e, &tmp_dir, "tempfile"))?;
        tmp.write_all(&bytes)
            .map_err(|e| Error::io(e, tmp.path(), "write"))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| Error::io(e, tmp.path(), "fsync"))?;
        tmp.persist(&path)
            .map_err(|e| Error::io(e.error, &path, "persist"))?;
        trace!(action = %action_digest, "action cache update");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Digest;
    use crate::message::{ExecutionMetadata, OutputFile};
    use chrono::Utc;
    use tempfile::TempDir;

    fn sample_result() -> ActionResult {
        ActionResult {
            output_files: vec![OutputFile {
                path: "out/a.txt".into(),
                digest: Digest::of_bytes(b"a"),
                is_executable: false,
            }],
            output_directories: vec![],
            exit_code: 0,
            stdout_digest: Some(Digest::of_bytes(b"hello\n")),
            stderr_digest: None,
            execution_metadata: ExecutionMetadata {
                worker: "local".into(),
                duration_ms: 42,
                created_at: Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn lookup_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        let ac = LocalActionCache::open(tmp.path()).unwrap();
        let d = Digest::of_bytes(b"no-such-action");
        assert!(ac.lookup(&d).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn update_then_lookup_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let ac = LocalActionCache::open(tmp.path()).unwrap();
        let d = Digest::of_bytes(b"action-1");
        let result = sample_result();
        ac.update(&d, &result).await.unwrap();
        let got = ac.lookup(&d).await.unwrap().unwrap();
        assert_eq!(got, result);
    }

    #[tokio::test]
    async fn update_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let ac = LocalActionCache::open(tmp.path()).unwrap();
        let d = Digest::of_bytes(b"action-2");

        let mut first = sample_result();
        first.exit_code = 1;
        ac.update(&d, &first).await.unwrap();

        let mut second = sample_result();
        second.exit_code = 0;
        ac.update(&d, &second).await.unwrap();

        let got = ac.lookup(&d).await.unwrap().unwrap();
        assert_eq!(got.exit_code, 0);
    }

    #[tokio::test]
    async fn malformed_action_digest_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let cache = LocalActionCache::open(tmp.path()).unwrap();
        let malformed = Digest {
            hash: "a".to_string(),
            size_bytes: 0,
        };

        assert!(cache.lookup(&malformed).await.is_err());
        assert!(cache.update(&malformed, &sample_result()).await.is_err());
    }
}
