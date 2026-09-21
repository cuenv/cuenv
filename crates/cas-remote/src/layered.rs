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
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

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
}

#[async_trait]
impl Cas for LayeredCas {
    async fn contains(&self, digest: &Digest) -> Result<bool> {
        if self.local.contains(digest).await? {
            return Ok(true);
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
        if self.local.contains(digest).await? {
            return self.local.get_to_file(digest, destination).await;
        }
        // Populate the local store first so the write to `destination` is a
        // local copy and the blob stays for next time.
        if self.fetch_through(digest).await.is_some() {
            return self.local.get_to_file(digest, destination).await;
        }
        self.local.get_to_file(digest, destination).await
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
            && let Err(e) = self.remote.put_file(source).await
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
            push: false,
        }
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
        if self.push
            && let Err(e) = self.remote.update(action_digest, result).await
        {
            warn!(action = %action_digest, error = %e, "remote action cache update failed; keeping local entry");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_cas::{LocalActionCache, LocalCas};
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        let remote_dir = TempDir::new().unwrap();
        let out_dir = TempDir::new().unwrap();
        let remote = local_cas(&remote_dir);
        let digest = remote.put_bytes(b"materialize me").await.unwrap();

        let layered = LayeredCas::new(local_cas(&local_dir), remote);
        let destination = out_dir.path().join("nested/out.bin");
        layered.get_to_file(&digest, &destination).await.unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"materialize me");
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
