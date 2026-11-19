//! Post-run remote cache publisher (`ActionPublisher` implementation).
//!
//! Enforces the REAPI v2 publication contract:
//! 1. Upload the canonical `Command` and `Action` blobs.
//! 2. Upload stdout, stderr, declared output files, output `Tree` messages,
//!    and every nested file leaf inside each `Tree`.
//! 3. Only after every blob succeeds, publish the `ActionResult` last via
//!    `UpdateActionResult` (gRPC) or `PUT /ac/<sha256>` (HTTP).
//!
//! Ordinary CAS writes during action construction and local recording stay
//! strictly local; remote writes flow exclusively through [`RemotePublisher`].

use crate::action_cache::RemoteActionCache;
use crate::cas::RemoteCas;
use crate::http::{HttpActionCache, HttpCas};
use async_trait::async_trait;
use cuenv_cas::{
    ActionCache, ActionPublisher, CanonicalMessage, Cas, Digest, PublishActionRequest,
    referenced_blobs,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::debug;
use uuid::Uuid;

/// Upper bound for loading a blob into memory during HTTP publication;
/// larger blobs are staged to a temporary file and streamed.
const HTTP_IN_MEMORY_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Publishes verified local actions to a remote cache over gRPC or HTTP.
#[derive(Debug, Clone)]
pub struct RemotePublisher {
    backend: PublisherBackend,
}

#[derive(Debug, Clone)]
enum PublisherBackend {
    Grpc {
        cas: Arc<RemoteCas>,
        action_cache: Arc<RemoteActionCache>,
    },
    Http {
        cas: Arc<HttpCas>,
        action_cache: Arc<HttpActionCache>,
    },
}

struct ActionPublication<'request> {
    request: PublishActionRequest<'request>,
    inline_blobs: HashMap<Digest, Vec<u8>>,
    all_digests: Vec<Digest>,
}

impl RemotePublisher {
    /// Create a publisher targeting a REAPI v2 gRPC cache.
    #[must_use]
    pub fn grpc(cas: Arc<RemoteCas>, action_cache: Arc<RemoteActionCache>) -> Self {
        Self {
            backend: PublisherBackend::Grpc { cas, action_cache },
        }
    }

    /// Create a publisher targeting a Bazel HTTP/REST cache.
    #[must_use]
    pub fn http(cas: Arc<HttpCas>, action_cache: Arc<HttpActionCache>) -> Self {
        Self {
            backend: PublisherBackend::Http { cas, action_cache },
        }
    }

    async fn publish_grpc(
        cas: &RemoteCas,
        action_cache: &RemoteActionCache,
        publication: &ActionPublication<'_>,
    ) -> cuenv_cas::Result<()> {
        let ActionPublication {
            request,
            inline_blobs,
            all_digests,
        } = publication;
        let missing = cas.find_missing(all_digests).await?;
        debug!(
            action = %request.action_digest,
            total = all_digests.len(),
            missing = missing.len(),
            "publishing action to remote gRPC cache"
        );

        let mut small_batch: Vec<(Digest, Vec<u8>)> = Vec::new();
        let mut batch_bytes = 0_u64;

        for digest in missing {
            if cas.fits_in_a_batch(digest.size_bytes) {
                let item_bytes = digest.size_bytes + crate::cas::BATCH_ITEM_OVERHEAD;
                if batch_bytes.saturating_add(item_bytes) > cas.max_batch_size().cast_unsigned() {
                    cas.batch_update_blobs(&small_batch).await?;
                    small_batch.clear();
                    batch_bytes = 0;
                }
                let bytes = if let Some(inline) = inline_blobs.get(&digest) {
                    inline.clone()
                } else {
                    request.local_cas.get(&digest).await?
                };
                small_batch.push((digest, bytes));
                batch_bytes += item_bytes;
            } else if let Some(inline) = inline_blobs.get(&digest) {
                cas.put_bytes(inline).await?;
            } else {
                upload_large_blob_via_file(request.local_cas, cas, &digest).await?;
            }
        }

        if !small_batch.is_empty() {
            cas.batch_update_blobs(&small_batch).await?;
        }

        // Publish the ActionResult LAST, only after Action, Command, logs,
        // outputs, and all nested Tree leaves have succeeded.
        action_cache
            .update(request.action_digest, request.result)
            .await?;
        Ok(())
    }

    async fn publish_http(
        cas: &HttpCas,
        action_cache: &HttpActionCache,
        publication: &ActionPublication<'_>,
    ) -> cuenv_cas::Result<()> {
        let ActionPublication {
            request,
            inline_blobs,
            all_digests,
        } = publication;
        for digest in all_digests {
            if cas.contains(digest).await? {
                continue;
            }
            if let Some(inline) = inline_blobs.get(digest) {
                cas.put_bytes(inline).await?;
            } else if digest.size_bytes <= HTTP_IN_MEMORY_THRESHOLD {
                let bytes = request.local_cas.get(digest).await?;
                cas.put_bytes(&bytes).await?;
            } else {
                upload_large_blob_via_file(request.local_cas, cas, digest).await?;
            }
        }

        // Publish the ActionResult LAST.
        action_cache
            .update(request.action_digest, request.result)
            .await?;
        Ok(())
    }
}

async fn upload_large_blob_via_file(
    local_cas: &dyn Cas,
    remote_cas: &dyn Cas,
    digest: &Digest,
) -> cuenv_cas::Result<()> {
    let temp_path = std::env::temp_dir().join(format!("cuenv-upload-{}.blob", Uuid::new_v4()));
    let result = async {
        local_cas.get_to_file(digest, &temp_path).await?;
        let uploaded = remote_cas.put_file(&temp_path).await?;
        if &uploaded != digest {
            return Err(cuenv_cas::Error::digest_mismatch(
                digest.to_resource(),
                uploaded.to_resource(),
            ));
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(&temp_path).await;
    result
}

#[async_trait]
impl ActionPublisher for RemotePublisher {
    async fn publish(&self, request: PublishActionRequest<'_>) -> cuenv_cas::Result<()> {
        let command_bytes = request.command.to_canonical_bytes()?;
        let action_bytes = request.action.to_canonical_bytes()?;

        let mut inline_blobs = HashMap::with_capacity(2);
        inline_blobs.insert(request.command_digest.clone(), command_bytes);
        inline_blobs.insert(request.action_digest.clone(), action_bytes);

        let mut seen = HashSet::new();
        let mut all_digests = Vec::new();
        for digest in [request.command_digest, request.action_digest] {
            if seen.insert(digest.clone()) {
                all_digests.push(digest.clone());
            }
        }
        for digest in referenced_blobs(request.local_cas, request.result).await? {
            if seen.insert(digest.clone()) {
                all_digests.push(digest);
            }
        }

        let publication = ActionPublication {
            request,
            inline_blobs,
            all_digests,
        };
        match &self.backend {
            PublisherBackend::Grpc { cas, action_cache } => {
                Self::publish_grpc(cas, action_cache, &publication).await
            }
            PublisherBackend::Http { cas, action_cache } => {
                Self::publish_http(cas, action_cache, &publication).await
            }
        }
    }
}
