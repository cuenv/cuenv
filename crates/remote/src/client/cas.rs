//! Content Addressable Storage (CAS) client

use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use crate::reapi::{
    batch_update_blobs_request, content_addressable_storage_client::ContentAddressableStorageClient,
    BatchReadBlobsRequest, BatchUpdateBlobsRequest, FindMissingBlobsRequest,
};
use std::sync::Arc;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, instrument, warn};

/// Maximum batch size for blob operations (4MB per REAPI spec recommendation)
const MAX_BATCH_SIZE_BYTES: usize = 4 * 1024 * 1024;

/// Client for REAPI ContentAddressableStorage service
pub struct CasClient {
    client: ContentAddressableStorageClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
}

impl CasClient {
    /// Create a new CAS client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client = ContentAddressableStorageClient::with_interceptor(
            channel.channel(),
            interceptor,
        );
        Self {
            client,
            config: Arc::new(config),
        }
    }

    /// Find which blobs are missing from the CAS
    #[instrument(skip(self, digests), fields(digest_count = digests.len()))]
    pub async fn find_missing_blobs(&self, digests: &[Digest]) -> Result<Vec<Digest>> {
        if digests.is_empty() {
            return Ok(vec![]);
        }

        let proto_digests: Vec<_> = digests.iter().map(digest_to_proto).collect();

        let request = FindMissingBlobsRequest {
            instance_name: self.config.instance_name.clone(),
            blob_digests: proto_digests,
            digest_function: 0, // SHA256
        };

        let mut client = self.client.clone();

        let response = client
            .find_missing_blobs(request)
            .await
            .map_err(|e| RemoteError::grpc_error("FindMissingBlobs", e))?;

        let missing: Vec<Digest> = response
            .into_inner()
            .missing_blob_digests
            .into_iter()
            .filter_map(|d| proto_to_digest(&d).ok())
            .collect();

        debug!(missing_count = missing.len(), "Found missing blobs");
        Ok(missing)
    }

    /// Upload multiple blobs to the CAS
    ///
    /// Automatically batches uploads to respect size limits
    #[instrument(skip(self, blobs), fields(blob_count = blobs.len()))]
    pub async fn batch_upload_blobs(&self, blobs: &[(Digest, Vec<u8>)]) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }

        // Split into batches based on size
        let mut current_batch: Vec<&(Digest, Vec<u8>)> = Vec::new();
        let mut current_size = 0usize;

        for blob in blobs {
            let blob_size = blob.1.len();

            // If this blob alone exceeds max size, upload it individually
            if blob_size > MAX_BATCH_SIZE_BYTES {
                // Flush current batch first
                if !current_batch.is_empty() {
                    self.upload_batch(&current_batch).await?;
                    current_batch.clear();
                    current_size = 0;
                }
                // Upload large blob individually
                self.upload_batch(&[blob]).await?;
                continue;
            }

            // If adding this blob would exceed limit, flush current batch
            if current_size + blob_size > MAX_BATCH_SIZE_BYTES && !current_batch.is_empty() {
                self.upload_batch(&current_batch).await?;
                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(blob);
            current_size += blob_size;
        }

        // Upload remaining batch
        if !current_batch.is_empty() {
            self.upload_batch(&current_batch).await?;
        }

        debug!(blob_count = blobs.len(), "Uploaded all blobs");
        Ok(())
    }

    /// Upload a single batch of blobs
    async fn upload_batch(&self, blobs: &[&(Digest, Vec<u8>)]) -> Result<()> {
        let requests: Vec<batch_update_blobs_request::Request> = blobs
            .iter()
            .map(|(digest, data)| batch_update_blobs_request::Request {
                digest: Some(digest_to_proto(digest)),
                data: data.clone(),
                compressor: 0, // No compression
            })
            .collect();

        let request = BatchUpdateBlobsRequest {
            instance_name: self.config.instance_name.clone(),
            requests,
            digest_function: 0, // SHA256
        };

        let mut client = self.client.clone();

        let response = client
            .batch_update_blobs(request)
            .await
            .map_err(|e| RemoteError::grpc_error("BatchUpdateBlobs", e))?;

        // Check individual responses for errors
        for resp in response.into_inner().responses {
            if let Some(status) = resp.status {
                if status.code != 0 {
                    let digest_str = resp
                        .digest
                        .map(|d| format!("{}/{}", d.hash, d.size_bytes))
                        .unwrap_or_else(|| "unknown".to_string());
                    warn!(
                        digest = %digest_str,
                        code = status.code,
                        message = %status.message,
                        "Blob upload failed"
                    );
                    return Err(RemoteError::upload_failed(digest_str, status.message));
                }
            }
        }

        debug!(batch_size = blobs.len(), "Uploaded blob batch");
        Ok(())
    }

    /// Download multiple blobs from the CAS
    #[instrument(skip(self, digests), fields(digest_count = digests.len()))]
    pub async fn batch_read_blobs(&self, digests: &[Digest]) -> Result<Vec<Vec<u8>>> {
        if digests.is_empty() {
            return Ok(vec![]);
        }

        let proto_digests: Vec<_> = digests.iter().map(digest_to_proto).collect();

        let request = BatchReadBlobsRequest {
            instance_name: self.config.instance_name.clone(),
            digests: proto_digests,
            acceptable_compressors: vec![],
            digest_function: 0,
        };

        let mut client = self.client.clone();

        let response = client
            .batch_read_blobs(request)
            .await
            .map_err(|e| RemoteError::grpc_error("BatchReadBlobs", e))?;

        let mut blobs = Vec::with_capacity(digests.len());
        for resp in response.into_inner().responses {
            if let Some(status) = &resp.status {
                if status.code != 0 {
                    let digest_str = resp
                        .digest
                        .map(|d| format!("{}/{}", d.hash, d.size_bytes))
                        .unwrap_or_else(|| "unknown".to_string());
                    return Err(RemoteError::content_not_found(format!(
                        "{}: {}",
                        digest_str, status.message
                    )));
                }
            }
            blobs.push(resp.data);
        }

        debug!(blob_count = blobs.len(), "Downloaded blobs");
        Ok(blobs)
    }

    /// Upload a single blob to the CAS
    pub async fn upload_blob(&self, digest: &Digest, data: Vec<u8>) -> Result<()> {
        self.batch_upload_blobs(&[(digest.clone(), data)]).await
    }

    /// Download a single blob from the CAS
    pub async fn read_blob(&self, digest: &Digest) -> Result<Vec<u8>> {
        let blobs = self.batch_read_blobs(&[digest.clone()]).await?;
        blobs.into_iter().next().ok_or_else(|| {
            RemoteError::content_not_found(digest.to_string())
        })
    }
}

/// Convert our Digest to proto Digest
fn digest_to_proto(digest: &Digest) -> crate::reapi::Digest {
    crate::reapi::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}

/// Convert proto Digest to our Digest
fn proto_to_digest(proto: &crate::reapi::Digest) -> Result<Digest> {
    Digest::new(&proto.hash, proto.size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_conversion_roundtrip() {
        let original = Digest::from_bytes(b"hello world");
        let proto = digest_to_proto(&original);
        let back = proto_to_digest(&proto).unwrap();

        assert_eq!(original.hash, back.hash);
        assert_eq!(original.size_bytes, back.size_bytes);
    }
}
