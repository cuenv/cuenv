//! A remote [`Cas`] that remembers which blobs it served corrupt.
//!
//! `FindMissingBlobs` answers from the server's index, not from its bytes. A
//! blob whose stored bytes no longer hash to their digest therefore stays
//! "present" for good: every read fails verification and falls back to
//! running the task, every upload is skipped because the server already
//! "has" the blob, and every result naming it is published and missed
//! again. Nothing in that loop is an error, so nothing repairs it.
//!
//! [`CorruptionAwareCas`] breaks the loop for the life of the process:
//!
//! - A read that fails digest verification records the digest and warns,
//!   naming it.
//! - [`Cas::contains`] reports a recorded digest as absent. A lookup then
//!   misses without fetching it again, and `LayeredActionCache` does not
//!   publish a result that names it.
//! - An upload of a recorded digest is sent even though the server reports
//!   it present ([`Cas::replace_bytes`] / [`Cas::replace_file`]), then read
//!   back. Only a verified read-back clears the record.
//!
//! A server that ignores writes for digests it already indexes cannot be
//! repaired from the client. The warning then says so, and names the digest
//! for an operator to evict.

use crate::temporary::TemporaryFile;
use async_trait::async_trait;
use cuenv_cas::{Cas, Digest, Error, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use tracing::{debug, warn};

/// Wraps a remote [`Cas`]; see the module documentation.
pub struct CorruptionAwareCas {
    inner: Arc<dyn Cas>,
    corrupt: Mutex<HashSet<Digest>>,
}

impl std::fmt::Debug for CorruptionAwareCas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CorruptionAwareCas")
            .field("corrupt", &self.corrupt_count())
            .finish_non_exhaustive()
    }
}

impl CorruptionAwareCas {
    /// Wrap `inner`, the remote store.
    #[must_use]
    pub fn new(inner: Arc<dyn Cas>) -> Self {
        Self {
            inner,
            corrupt: Mutex::new(HashSet::new()),
        }
    }

    /// Whether a read of `digest` failed verification in this process and
    /// no upload has repaired it since.
    #[must_use]
    pub fn is_known_corrupt(&self, digest: &Digest) -> bool {
        self.corrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(digest)
    }

    fn corrupt_count(&self) -> usize {
        self.corrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn any_known_corrupt(&self) -> bool {
        self.corrupt_count() > 0
    }

    /// Record `digest` when `error` says its bytes did not verify.
    fn note(&self, digest: &Digest, error: &Error) {
        if matches!(error, Error::DigestMismatch { .. }) {
            warn!(
                digest = %digest,
                %error,
                "remote CAS served bytes that do not match their digest; \
                 treating the blob as absent for the rest of this run (it is replaced \
                 only when this run uploads to the remote cache)"
            );
            self.corrupt
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(digest.clone());
        }
    }

    /// Read `digest` back after replacing it and clear the record only if
    /// the server now serves the right bytes.
    async fn confirm_repair(&self, digest: &Digest) {
        let readback = match TemporaryFile::new() {
            Ok(readback) => readback,
            Err(error) => {
                debug!(digest = %digest, %error, "cannot read back a replaced blob");
                return;
            }
        };
        match self.inner.get_to_file(digest, readback.path()).await {
            Ok(()) => {
                self.corrupt
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(digest);
                warn!(digest = %digest, "replaced a corrupt blob in the remote CAS");
            }
            Err(error) => warn!(
                digest = %digest,
                %error,
                "the remote CAS still serves a corrupt blob after it was re-uploaded; \
                 the server must evict or repair it"
            ),
        }
    }
}

#[async_trait]
impl Cas for CorruptionAwareCas {
    async fn contains(&self, digest: &Digest) -> Result<bool> {
        if self.is_known_corrupt(digest) {
            return Ok(false);
        }
        self.inner.contains(digest).await
    }

    async fn get(&self, digest: &Digest) -> Result<Vec<u8>> {
        self.inner
            .get(digest)
            .await
            .inspect_err(|error| self.note(digest, error))
    }

    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
        self.inner
            .get_to_file(digest, destination)
            .await
            .inspect_err(|error| self.note(digest, error))
    }

    async fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = Digest::of_bytes(bytes);
        if self.is_known_corrupt(&digest) {
            return self.replace_bytes(bytes).await;
        }
        self.inner.put_bytes(bytes).await
    }

    async fn put_file(&self, source: &Path) -> Result<Digest> {
        // Hashing the file first costs a second read; pay it only when there
        // is something to repair.
        if self.any_known_corrupt() {
            let digest = crate::cas::digest_file(source).await?;
            if self.is_known_corrupt(&digest) {
                return self.replace_file(source).await;
            }
        }
        self.inner.put_file(source).await
    }

    async fn replace_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = self.inner.replace_bytes(bytes).await?;
        self.confirm_repair(&digest).await;
        Ok(digest)
    }

    async fn replace_file(&self, source: &Path) -> Result<Digest> {
        let digest = self.inner.replace_file(source).await?;
        self.confirm_repair(&digest).await;
        Ok(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A server whose index can say "present" for bytes that are wrong, and
    /// that skips `put_*` for any digest it indexes, like `FindMissingBlobs`
    /// followed by an upload. `honour_replace` decides whether a forced
    /// upload overwrites the stored bytes.
    struct IndexedServer {
        blobs: Mutex<HashMap<Digest, Vec<u8>>>,
        honour_replace: bool,
        uploads: AtomicUsize,
    }

    impl IndexedServer {
        fn new(honour_replace: bool) -> Self {
            Self {
                blobs: Mutex::new(HashMap::new()),
                honour_replace,
                uploads: AtomicUsize::new(0),
            }
        }

        fn corrupt(&self, digest: &Digest) {
            self.blobs
                .lock()
                .unwrap()
                .insert(digest.clone(), b"rotten".to_vec());
        }

        fn store(&self, digest: Digest, bytes: &[u8], overwrite: bool) {
            self.uploads.fetch_add(1, Ordering::Relaxed);
            let mut blobs = self.blobs.lock().unwrap();
            if overwrite || !blobs.contains_key(&digest) {
                blobs.insert(digest, bytes.to_vec());
            }
        }
    }

    #[async_trait]
    impl Cas for IndexedServer {
        async fn contains(&self, digest: &Digest) -> Result<bool> {
            Ok(self.blobs.lock().unwrap().contains_key(digest))
        }
        async fn get(&self, digest: &Digest) -> Result<Vec<u8>> {
            let bytes = self
                .blobs
                .lock()
                .unwrap()
                .get(digest)
                .cloned()
                .ok_or_else(|| Error::not_found(digest.hash.clone()))?;
            let actual = Digest::of_bytes(&bytes);
            if &actual != digest {
                return Err(Error::digest_mismatch(
                    digest.to_resource(),
                    actual.to_resource(),
                ));
            }
            Ok(bytes)
        }
        async fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
            let bytes = self.get(digest).await?;
            std::fs::write(destination, bytes)
                .map_err(|error| Error::io(error, destination, "write"))
        }
        async fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
            let digest = Digest::of_bytes(bytes);
            if !self.blobs.lock().unwrap().contains_key(&digest) {
                self.store(digest.clone(), bytes, false);
            }
            Ok(digest)
        }
        async fn put_file(&self, source: &Path) -> Result<Digest> {
            let bytes = std::fs::read(source).map_err(|error| Error::io(error, source, "read"))?;
            self.put_bytes(&bytes).await
        }
        async fn replace_bytes(&self, bytes: &[u8]) -> Result<Digest> {
            let digest = Digest::of_bytes(bytes);
            self.store(digest.clone(), bytes, self.honour_replace);
            Ok(digest)
        }
        async fn replace_file(&self, source: &Path) -> Result<Digest> {
            let bytes = std::fs::read(source).map_err(|error| Error::io(error, source, "read"))?;
            self.replace_bytes(&bytes).await
        }
    }

    fn poisoned(honour_replace: bool) -> (Arc<IndexedServer>, CorruptionAwareCas, Digest) {
        let server = Arc::new(IndexedServer::new(honour_replace));
        let digest = Digest::of_bytes(b"output");
        server.corrupt(&digest);
        let cas = CorruptionAwareCas::new(server.clone());
        (server, cas, digest)
    }

    #[tokio::test]
    async fn a_corrupt_read_hides_the_blob_from_contains() {
        let (_, cas, digest) = poisoned(true);
        assert!(
            cas.contains(&digest).await.unwrap(),
            "the index says present"
        );
        assert!(matches!(
            cas.get(&digest).await,
            Err(Error::DigestMismatch { .. })
        ));
        assert!(cas.is_known_corrupt(&digest));
        assert!(!cas.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn uploading_a_known_corrupt_blob_replaces_it_and_clears_the_record() {
        let (server, cas, digest) = poisoned(true);
        let _ = cas.get(&digest).await;
        assert_eq!(cas.put_bytes(b"output").await.unwrap(), digest);
        assert_eq!(server.uploads.load(Ordering::Relaxed), 1);
        assert_eq!(cas.get(&digest).await.unwrap(), b"output");
        assert!(!cas.is_known_corrupt(&digest));
        assert!(cas.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn a_file_upload_of_a_known_corrupt_blob_replaces_it() {
        let (server, cas, digest) = poisoned(true);
        let _ = cas.get(&digest).await;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("output");
        std::fs::write(&source, b"output").unwrap();
        assert_eq!(cas.put_file(&source).await.unwrap(), digest);
        assert_eq!(server.uploads.load(Ordering::Relaxed), 1);
        assert!(!cas.is_known_corrupt(&digest));
    }

    #[tokio::test]
    async fn a_server_that_ignores_the_replacement_stays_recorded() {
        let (_, cas, digest) = poisoned(false);
        let _ = cas.get(&digest).await;
        cas.put_bytes(b"output").await.unwrap();
        assert!(cas.is_known_corrupt(&digest));
        assert!(!cas.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn without_a_corrupt_read_uploads_are_skipped_as_before() {
        let (server, cas, digest) = poisoned(true);
        cas.put_bytes(b"output").await.unwrap();
        assert_eq!(server.uploads.load(Ordering::Relaxed), 0);
        assert!(cas.contains(&digest).await.unwrap());
    }
}
