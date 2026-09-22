//! Read-through, write-behind stacking of a local store over a remote one.
//!
//! The local store is authoritative for latency and the remote for reach. A
//! read consults the local store first and, on a miss, fetches from the
//! remote and keeps a copy so the next read is local. A write goes to the
//! local store synchronously — the task's own outputs must be there before it
//! is reported complete — and to the remote as well.
//!
//! # Why a remote failure is not an error
//!
//! A cache is an optimization. If the network is down, the right outcome is a
//! slower build, not a failed one. Every remote operation here therefore logs
//! and degrades: a failed remote read becomes a miss, and a failed remote
//! write is dropped after being recorded locally.
//!
//! The one thing that is *not* forgiven is a digest mismatch, which
//! [`RemoteCas`](crate::RemoteCas) rejects before the bytes reach this layer.

use async_trait::async_trait;
use cuenv_cas::{ActionCache, ActionResult, Cas, Digest, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};
use uuid::Uuid;

struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("cuenv-layered-cas-{}.tmp", Uuid::new_v4()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| cuenv_cas::Error::io(error, &path, "create temporary file"))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn verify_digest(expected: &Digest, actual: &Digest) -> Result<()> {
    (expected == actual).then_some(()).ok_or_else(|| {
        cuenv_cas::Error::digest_mismatch(expected.to_resource(), actual.to_resource())
    })
}

/// A [`Cas`] that reads through a local store to a remote one.
#[derive(Clone)]
pub struct LayeredCas {
    local: Arc<dyn Cas>,
    remote: Arc<dyn Cas>,
    /// Whether to push locally-written blobs to the remote store.
    push: bool,
}

impl std::fmt::Debug for LayeredCas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredCas")
            .field("push", &self.push)
            .finish_non_exhaustive()
    }
}

impl LayeredCas {
    /// Stack `remote` behind `local`, reading through but not pushing.
    #[must_use]
    pub fn new(local: Arc<dyn Cas>, remote: Arc<dyn Cas>) -> Self {
        Self {
            local,
            remote,
            push: false,
        }
    }

    /// Also push locally-written blobs to the remote store.
    #[must_use]
    pub fn with_push(mut self) -> Self {
        self.push = true;
        self
    }

    /// Fetch from the remote store and keep a local copy.
    ///
    /// Returns `None` when the remote does not have it, or could not be
    /// reached.
    async fn fetch_through(&self, digest: &Digest) -> Option<Vec<u8>> {
        match self.remote.get(digest).await {
            Ok(bytes) => {
                if let Err(e) = self.local.put_bytes(&bytes).await {
                    // The fetch still succeeded; only the local copy failed,
                    // so the caller gets its bytes and the next read repeats
                    // the fetch.
                    warn!(digest = %digest, error = %e, "cannot cache remote blob locally");
                }
                Some(bytes)
            }
            Err(e) => {
                debug!(digest = %digest, error = %e, "remote CAS did not serve the blob");
                None
            }
        }
    }

    async fn fetch_through_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
        let downloaded = TemporaryFile::new()?;
        if let Err(error) = self.remote.get_to_file(digest, downloaded.path()).await {
            debug!(digest = %digest, error = %error, "remote CAS did not serve the blob");
            return self.local.get_to_file(digest, destination).await;
        }

        match self.local.put_file(downloaded.path()).await {
            Ok(local_digest) => {
                verify_digest(digest, &local_digest)?;
                self.local.get_to_file(digest, destination).await
            }
            // The remote verified these bytes against `digest` while
            // downloading them; only keeping a local copy failed. Serve the
            // download, and the next read repeats the fetch.
            Err(e) => {
                warn!(digest = %digest, error = %e, "cannot cache remote blob locally");
                std::fs::copy(downloaded.path(), destination)
                    .map(|_| ())
                    .map_err(|error| cuenv_cas::Error::io(error, destination, "copy remote blob"))
            }
        }
    }

    async fn push_file_snapshot(&self, digest: &Digest) -> Result<()> {
        let snapshot = TemporaryFile::new()?;
        self.local.get_to_file(digest, snapshot.path()).await?;
        let snapshot_digest = self.local.put_file(snapshot.path()).await?;
        verify_digest(digest, &snapshot_digest)?;

        let remote_digest = self.remote.put_file(snapshot.path()).await?;
        verify_digest(digest, &remote_digest)
    }
}

#[async_trait]
impl Cas for LayeredCas {
    async fn contains(&self, digest: &Digest) -> Result<bool> {
        match self.local.contains(digest).await {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            // An unreadable local store is a local miss, not a failure: the
            // remote may still hold the blob.
            Err(e) => {
                debug!(digest = %digest, error = %e, "local CAS unreadable; asking the remote");
            }
        }
        match self.remote.contains(digest).await {
            Ok(found) => Ok(found),
            Err(e) => {
                debug!(digest = %digest, error = %e, "remote CAS unreachable; treating as absent");
                Ok(false)
            }
        }
    }

    async fn get(&self, digest: &Digest) -> Result<Vec<u8>> {
        match self.local.get(digest).await {
            Ok(bytes) => Ok(bytes),
            Err(local_error) => match self.fetch_through(digest).await {
                Some(bytes) => Ok(bytes),
                // Report the local error: it names the digest the caller
                // asked for, and the remote failure has already been logged.
                None => Err(local_error),
            },
        }
    }

    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
        // Mirror `get`: a local copy that is missing, unreadable or fails
        // verification falls through to the remote rather than failing a
        // read the remote could have served.
        match self.local.contains(digest).await {
            Ok(true) => match self.local.get_to_file(digest, destination).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    debug!(digest = %digest, error = %e, "local CAS read failed; fetching from the remote");
                }
            },
            Ok(false) => {}
            Err(e) => {
                debug!(digest = %digest, error = %e, "local CAS unreadable; fetching from the remote");
            }
        }
        self.fetch_through_file(digest, destination).await
    }

    async fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = self.local.put_bytes(bytes).await?;
        if self.push
            && let Err(e) = self.remote.put_bytes(bytes).await
        {
            warn!(digest = %digest, error = %e, "remote CAS upload failed; keeping local copy");
        }
        Ok(digest)
    }

    async fn put_file(&self, source: &Path) -> Result<Digest> {
        let digest = self.local.put_file(source).await?;
        if self.push
            && let Err(e) = self.push_file_snapshot(&digest).await
        {
            warn!(digest = %digest, error = %e, "remote CAS upload failed; keeping local copy");
        }
        Ok(digest)
    }
}

/// An [`ActionCache`] that reads through a local cache to a remote one.
#[derive(Clone)]
pub struct LayeredActionCache {
    local: Arc<dyn ActionCache>,
    remote: Arc<dyn ActionCache>,
    remote_cas: Option<Arc<dyn Cas>>,
    push: bool,
}

impl std::fmt::Debug for LayeredActionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredActionCache")
            .field("push", &self.push)
            .finish_non_exhaustive()
    }
}

impl LayeredActionCache {
    /// Stack `remote` behind `local`, reading through but not pushing.
    #[must_use]
    pub fn new(local: Arc<dyn ActionCache>, remote: Arc<dyn ActionCache>) -> Self {
        Self {
            local,
            remote,
            remote_cas: None,
            push: false,
        }
    }

    /// Supply the remote CAS used to verify that an action result is complete
    /// before publishing it remotely.
    #[must_use]
    pub fn with_remote_cas(mut self, remote_cas: Arc<dyn Cas>) -> Self {
        self.remote_cas = Some(remote_cas);
        self
    }

    /// Also push locally-recorded results to the remote cache.
    #[must_use]
    pub fn with_push(mut self) -> Self {
        self.push = true;
        self
    }
}

#[async_trait]
impl ActionCache for LayeredActionCache {
    async fn lookup(&self, action_digest: &Digest) -> Result<Option<ActionResult>> {
        if let Some(result) = self.local.lookup(action_digest).await? {
            return Ok(Some(result));
        }

        let remote = match self.remote.lookup(action_digest).await {
            Ok(remote) => remote,
            Err(e) => {
                debug!(action = %action_digest, error = %e, "remote action cache unreachable; treating as a miss");
                return Ok(None);
            }
        };

        let Some(result) = remote else {
            return Ok(None);
        };

        // Do not persist a remote result before its referenced blobs have
        // been fetched and verified. A dangling local result would shadow a
        // later repaired remote entry indefinitely.
        Ok(Some(result))
    }

    async fn update(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        self.local.update(action_digest, result).await?;
        if self.push {
            let Some(remote_cas) = &self.remote_cas else {
                warn!(
                    action = %action_digest,
                    "remote action result not published: no remote CAS verifier configured"
                );
                return Ok(());
            };
            let action_present = remote_cas.contains(action_digest).await.unwrap_or(false);
            let missing = cuenv_cas::missing_blobs(remote_cas.as_ref(), result)
                .await
                .unwrap_or_else(|error| {
                    warn!(action = %action_digest, %error, "could not verify remote action result blobs");
                    vec![action_digest.clone()]
                });
            if !action_present || !missing.is_empty() {
                warn!(
                    action = %action_digest,
                    missing = ?missing,
                    action_present,
                    "remote action result not published because referenced CAS blobs are incomplete"
                );
                return Ok(());
            }
            if let Err(e) = self.remote.update(action_digest, result).await {
                warn!(action = %action_digest, error = %e, "remote action cache update failed; keeping local entry");
            }
        }
        Ok(())
    }

    async fn commit_verified(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        self.local.update(action_digest, result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_cas::{LocalActionCache, LocalCas};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tempfile::TempDir;

    /// A store that fails every operation, standing in for an unreachable
    /// server.
    #[derive(Debug, Default)]
    struct BrokenCas {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Cas for BrokenCas {
        async fn contains(&self, _digest: &Digest) -> Result<bool> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization("network is down"))
        }
        async fn get(&self, _digest: &Digest) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization("network is down"))
        }
        async fn get_to_file(&self, _digest: &Digest, _destination: &Path) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization("network is down"))
        }
        async fn put_bytes(&self, _bytes: &[u8]) -> Result<Digest> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization("network is down"))
        }
        async fn put_file(&self, _source: &Path) -> Result<Digest> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization("network is down"))
        }
    }

    struct StreamingOnlyCas {
        bytes: Vec<u8>,
        get_calls: AtomicUsize,
        get_to_file_calls: AtomicUsize,
    }

    impl StreamingOnlyCas {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                get_calls: AtomicUsize::new(0),
                get_to_file_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Cas for StreamingOnlyCas {
        async fn contains(&self, _digest: &Digest) -> Result<bool> {
            Ok(true)
        }

        async fn get(&self, _digest: &Digest) -> Result<Vec<u8>> {
            self.get_calls.fetch_add(1, Ordering::Relaxed);
            Err(cuenv_cas::Error::serialization(
                "in-memory get must not be called",
            ))
        }

        async fn get_to_file(&self, _digest: &Digest, destination: &Path) -> Result<()> {
            self.get_to_file_calls.fetch_add(1, Ordering::Relaxed);
            std::fs::write(destination, &self.bytes)
                .map_err(|error| cuenv_cas::Error::io(error, destination, "write"))
        }

        async fn put_bytes(&self, _bytes: &[u8]) -> Result<Digest> {
            Err(cuenv_cas::Error::serialization("unexpected put_bytes"))
        }

        async fn put_file(&self, _source: &Path) -> Result<Digest> {
            Err(cuenv_cas::Error::serialization("unexpected put_file"))
        }
    }

    struct MutatingLocalCas {
        inner: Arc<dyn Cas>,
        replacement: Vec<u8>,
        mutate_next_put: AtomicBool,
    }

    impl MutatingLocalCas {
        fn new(inner: Arc<dyn Cas>, replacement: &[u8]) -> Self {
            Self {
                inner,
                replacement: replacement.to_vec(),
                mutate_next_put: AtomicBool::new(true),
            }
        }
    }

    #[async_trait]
    impl Cas for MutatingLocalCas {
        async fn contains(&self, digest: &Digest) -> Result<bool> {
            self.inner.contains(digest).await
        }

        async fn get(&self, digest: &Digest) -> Result<Vec<u8>> {
            self.inner.get(digest).await
        }

        async fn get_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
            self.inner.get_to_file(digest, destination).await
        }

        async fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
            self.inner.put_bytes(bytes).await
        }

        async fn put_file(&self, source: &Path) -> Result<Digest> {
            let digest = self.inner.put_file(source).await?;
            if self.mutate_next_put.swap(false, Ordering::Relaxed) {
                std::fs::write(source, &self.replacement)
                    .map_err(|error| cuenv_cas::Error::io(error, source, "mutate test source"))?;
            }
            Ok(digest)
        }
    }

    #[derive(Default)]
    struct RecordingUploadCas {
        uploaded: Mutex<Option<Vec<u8>>>,
    }

    #[async_trait]
    impl Cas for RecordingUploadCas {
        async fn contains(&self, _digest: &Digest) -> Result<bool> {
            Ok(false)
        }

        async fn get(&self, _digest: &Digest) -> Result<Vec<u8>> {
            Err(cuenv_cas::Error::serialization("unexpected get"))
        }

        async fn get_to_file(&self, _digest: &Digest, _destination: &Path) -> Result<()> {
            Err(cuenv_cas::Error::serialization("unexpected get_to_file"))
        }

        async fn put_bytes(&self, _bytes: &[u8]) -> Result<Digest> {
            Err(cuenv_cas::Error::serialization("unexpected put_bytes"))
        }

        async fn put_file(&self, source: &Path) -> Result<Digest> {
            let bytes = std::fs::read(source)
                .map_err(|error| cuenv_cas::Error::io(error, source, "read upload"))?;
            let digest = Digest::of_bytes(&bytes);
            *self.uploaded.lock().unwrap() = Some(bytes);
            Ok(digest)
        }
    }

    #[derive(Debug, Default)]
    struct BrokenActionCache;

    #[async_trait]
    impl ActionCache for BrokenActionCache {
        async fn lookup(&self, _action_digest: &Digest) -> Result<Option<ActionResult>> {
            Err(cuenv_cas::Error::serialization("network is down"))
        }
        async fn update(&self, _action_digest: &Digest, _result: &ActionResult) -> Result<()> {
            Err(cuenv_cas::Error::serialization("network is down"))
        }
    }

    fn local_cas(dir: &TempDir) -> Arc<dyn Cas> {
        Arc::new(LocalCas::open(dir.path()).unwrap())
    }

    #[tokio::test]
    async fn an_unreadable_local_store_falls_back_to_the_remote() {
        let remote_dir = TempDir::new().unwrap();
        let remote = local_cas(&remote_dir);
        let digest = remote.put_bytes(b"remote").await.unwrap();
        let layered = LayeredCas::new(Arc::new(BrokenCas::default()), remote);
        let out_dir = TempDir::new().unwrap();
        let destination = out_dir.path().join("blob");

        assert!(layered.contains(&digest).await.unwrap());
        layered.get_to_file(&digest, &destination).await.unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"remote");
    }

    #[tokio::test]
    async fn a_local_hit_never_touches_the_remote() {
        let dir = TempDir::new().unwrap();
        let local = local_cas(&dir);
        let digest = local.put_bytes(b"local").await.unwrap();

        let remote = Arc::new(BrokenCas::default());
        let layered = LayeredCas::new(local, remote.clone());

        assert_eq!(layered.get(&digest).await.unwrap(), b"local");
        assert!(layered.contains(&digest).await.unwrap());
        assert_eq!(remote.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_remote_blob_is_fetched_and_kept_locally() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let local = local_cas(&local_dir);
        let remote = local_cas(&remote_dir);
        let digest = remote.put_bytes(b"from remote").await.unwrap();

        let layered = LayeredCas::new(local.clone(), remote);
        assert_eq!(layered.get(&digest).await.unwrap(), b"from remote");

        // The point of read-through: the second read is local.
        assert!(local.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn an_unreachable_remote_degrades_to_a_miss() {
        let dir = TempDir::new().unwrap();
        let layered = LayeredCas::new(local_cas(&dir), Arc::new(BrokenCas::default()));
        let absent = Digest::of_bytes(b"nowhere");

        assert!(!layered.contains(&absent).await.unwrap());
        assert!(layered.get(&absent).await.is_err());
    }

    #[tokio::test]
    async fn a_failing_remote_upload_does_not_fail_the_write() {
        let dir = TempDir::new().unwrap();
        let local = local_cas(&dir);
        let layered = LayeredCas::new(local.clone(), Arc::new(BrokenCas::default())).with_push();

        // A cache is an optimization: a dead network must not fail the task.
        let digest = layered.put_bytes(b"payload").await.unwrap();
        assert!(local.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn blobs_are_not_pushed_unless_asked() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let local = local_cas(&local_dir);
        let remote = local_cas(&remote_dir);

        let layered = LayeredCas::new(local, remote.clone());
        let digest = layered.put_bytes(b"payload").await.unwrap();

        assert!(!remote.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn pushing_uploads_to_the_remote() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let remote = local_cas(&remote_dir);

        let layered = LayeredCas::new(local_cas(&local_dir), remote.clone()).with_push();
        let digest = layered.put_bytes(b"payload").await.unwrap();

        assert!(remote.contains(&digest).await.unwrap());
    }

    #[tokio::test]
    async fn get_to_file_populates_from_the_remote() {
        let local_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();
        let local = local_cas(&local_dir);
        let remote = Arc::new(StreamingOnlyCas::new(b"materialize me"));
        let digest = Digest::of_bytes(b"materialize me");

        let layered = LayeredCas::new(local.clone(), remote.clone());
        let destination = out_dir.path().join("nested/out.bin");
        layered.get_to_file(&digest, &destination).await.unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"materialize me");
        assert!(local.contains(&digest).await.unwrap());
        assert_eq!(remote.get_calls.load(Ordering::Relaxed), 0);
        assert_eq!(remote.get_to_file_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn get_to_file_rejects_a_download_with_the_wrong_digest() {
        let local_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();
        let remote = Arc::new(StreamingOnlyCas::new(b"wrong bytes"));
        let digest = Digest::of_bytes(b"requested bytes");

        let layered = LayeredCas::new(local_cas(&local_dir), remote);
        let destination = out_dir.path().join("out.bin");
        let error = layered
            .get_to_file(&digest, &destination)
            .await
            .unwrap_err();

        assert!(matches!(error, cuenv_cas::Error::DigestMismatch { .. }));
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn put_file_uploads_the_local_snapshot_when_the_source_changes() {
        let local_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();
        let source = source_dir.path().join("source.bin");
        std::fs::write(&source, b"original bytes").unwrap();

        let local: Arc<dyn Cas> = Arc::new(MutatingLocalCas::new(
            local_cas(&local_dir),
            b"changed after local put",
        ));
        let remote = Arc::new(RecordingUploadCas::default());
        let layered = LayeredCas::new(local, remote.clone()).with_push();

        let digest = layered.put_file(&source).await.unwrap();

        assert_eq!(digest, Digest::of_bytes(b"original bytes"));
        assert_eq!(std::fs::read(&source).unwrap(), b"changed after local put");
        assert_eq!(
            remote.uploaded.lock().unwrap().as_deref(),
            Some(&b"original bytes"[..])
        );
    }

    fn sample_result() -> ActionResult {
        ActionResult {
            exit_code: 0,
            ..ActionResult::default()
        }
    }

    #[tokio::test]
    async fn a_remote_action_result_is_not_cached_before_blob_verification() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let remote: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(remote_dir.path()).unwrap());

        let digest = Digest::of_bytes(b"action");
        remote.update(&digest, &sample_result()).await.unwrap();

        let layered = LayeredActionCache::new(local.clone(), remote);
        assert!(layered.lookup(&digest).await.unwrap().is_some());
        assert!(local.lookup(&digest).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_verified_remote_action_result_is_promoted_locally_only() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let remote: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(remote_dir.path()).unwrap());

        let digest = Digest::of_bytes(b"verified-action");
        let result = sample_result();
        let layered = LayeredActionCache::new(local.clone(), remote.clone()).with_push();
        layered.commit_verified(&digest, &result).await.unwrap();

        assert_eq!(local.lookup(&digest).await.unwrap(), Some(result));
        assert!(remote.lookup(&digest).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_unreachable_remote_action_cache_is_a_miss() {
        let local_dir = TempDir::new().unwrap();
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let layered = LayeredActionCache::new(local, Arc::new(BrokenActionCache));

        assert!(
            layered
                .lookup(&Digest::of_bytes(b"action"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_failing_remote_update_does_not_fail_the_record() {
        let local_dir = TempDir::new().unwrap();
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let layered =
            LayeredActionCache::new(local.clone(), Arc::new(BrokenActionCache)).with_push();

        let digest = Digest::of_bytes(b"action");
        layered.update(&digest, &sample_result()).await.unwrap();
        assert!(local.lookup(&digest).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_dangling_result_is_not_published_remotely() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let remote_cas = local_cas(&remote_dir);
        let action_digest = remote_cas.put_bytes(b"action").await.unwrap();
        let missing = Digest::of_bytes(b"missing output");
        let result = ActionResult {
            stdout_digest: Some(missing),
            ..sample_result()
        };
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let remote: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(remote_dir.path()).unwrap());
        let layered = LayeredActionCache::new(local.clone(), remote.clone())
            .with_remote_cas(remote_cas)
            .with_push();

        layered.update(&action_digest, &result).await.unwrap();

        assert_eq!(local.lookup(&action_digest).await.unwrap(), Some(result));
        assert!(remote.lookup(&action_digest).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_complete_result_can_be_published_remotely() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let remote_cas = local_cas(&remote_dir);
        let action_digest = remote_cas.put_bytes(b"action").await.unwrap();
        let stdout_digest = remote_cas.put_bytes(b"stdout").await.unwrap();
        let result = ActionResult {
            stdout_digest: Some(stdout_digest),
            ..sample_result()
        };
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let remote: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(remote_dir.path()).unwrap());
        let layered = LayeredActionCache::new(local, remote.clone())
            .with_remote_cas(remote_cas)
            .with_push();

        layered.update(&action_digest, &result).await.unwrap();

        assert_eq!(remote.lookup(&action_digest).await.unwrap(), Some(result));
    }

    #[tokio::test]
    async fn action_results_are_not_pushed_unless_asked() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();
        let local: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(local_dir.path()).unwrap());
        let remote: Arc<dyn ActionCache> =
            Arc::new(LocalActionCache::open(remote_dir.path()).unwrap());

        let layered = LayeredActionCache::new(local, remote.clone());
        let digest = Digest::of_bytes(b"action");
        layered.update(&digest, &sample_result()).await.unwrap();

        assert!(remote.lookup(&digest).await.unwrap().is_none());
    }
}
