//! Content Addressable Storage (CAS) client

use crate::client::bytestream::ByteStreamClient;
use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use crate::reapi::{
    BatchReadBlobsRequest, BatchUpdateBlobsRequest, FindMissingBlobsRequest,
    batch_update_blobs_request,
    content_addressable_storage_client::ContentAddressableStorageClient,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, info, instrument, warn};

/// Maximum TOTAL batch size for BatchUpdateBlobs (stay well under 75MB gRPC limit)
/// Account for protobuf overhead by using 30MB limit
const MAX_BATCH_TOTAL_BYTES: usize = 30 * 1024 * 1024; // 30MB

/// Maximum single blob size before using individual upload
const MAX_SINGLE_BLOB_BYTES: usize = 25 * 1024 * 1024; // 25MB

/// Maximum number of digests per FindMissingBlobs request
const MAX_FIND_MISSING_BATCH_SIZE: usize = 500;

/// Maximum number of blobs per BatchUpdateBlobs request
const MAX_UPLOAD_BATCH_COUNT: usize = 100;

/// Threshold for using ByteStream instead of BatchUpdateBlobs
/// Blobs larger than this are uploaded via streaming
const BYTESTREAM_THRESHOLD: usize = 4 * 1024 * 1024; // 4MB

/// Client for REAPI ContentAddressableStorage service
pub struct CasClient {
    client: ContentAddressableStorageClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
}

impl CasClient {
    /// Create a new CAS client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client =
            ContentAddressableStorageClient::with_interceptor(channel.channel(), interceptor);
        Self {
            client,
            config: Arc::new(config),
        }
    }

    /// Find which blobs are missing from the CAS
    ///
    /// Automatically batches requests to avoid exceeding server limits
    #[instrument(skip(self, digests), fields(digest_count = digests.len()))]
    pub async fn find_missing_blobs(&self, digests: &[Digest]) -> Result<Vec<Digest>> {
        if digests.is_empty() {
            return Ok(vec![]);
        }

        let mut all_missing = Vec::new();
        let mut client = self.client.clone();

        // Process in batches
        for chunk in digests.chunks(MAX_FIND_MISSING_BATCH_SIZE) {
            let proto_digests: Vec<_> = chunk.iter().map(digest_to_proto).collect();

            let request = FindMissingBlobsRequest {
                instance_name: self.config.instance_name.clone(),
                blob_digests: proto_digests,
                digest_function: 1, // SHA256 (REAPI enum value)
            };

            let response = client
                .find_missing_blobs(request)
                .await
                .map_err(|e| RemoteError::grpc_error("FindMissingBlobs", e))?;

            let batch_missing: Result<Vec<Digest>> = response
                .into_inner()
                .missing_blob_digests
                .into_iter()
                .map(|d| proto_to_digest(&d))
                .collect();

            all_missing.extend(batch_missing?);
        }

        debug!(missing_count = all_missing.len(), "Found missing blobs");
        Ok(all_missing)
    }

    /// Upload multiple blobs to the CAS
    ///
    /// Automatically batches uploads to respect size and count limits
    #[instrument(skip(self, blobs), fields(blob_count = blobs.len()))]
    pub async fn batch_upload_blobs(&self, blobs: &[(Digest, Vec<u8>)]) -> Result<()> {
        use tracing::info;

        if blobs.is_empty() {
            return Ok(());
        }

        // Calculate total size for progress reporting
        let total_size: usize = blobs.iter().map(|(_, data)| data.len()).sum();
        let total_size_mb = total_size / (1024 * 1024);

        info!(
            blob_count = blobs.len(),
            total_size_mb,
            "Starting blob upload to CAS"
        );

        // Split into batches based on TOTAL size and count
        let mut current_batch: Vec<&(Digest, Vec<u8>)> = Vec::new();
        let mut current_size = 0usize;
        let mut uploaded_count = 0usize;
        let mut batch_count = 0usize;

        // Maximum blob size we can upload via BatchUpdateBlobs
        // Larger blobs would need ByteStream API (not yet implemented)
        const MAX_UPLOADABLE_BLOB_BYTES: usize = 60 * 1024 * 1024; // 60MB

        let mut skipped_large_blobs = 0usize;

        for blob in blobs {
            let blob_size = blob.1.len();

            // If blob is too large for BatchUpdateBlobs, skip it for now
            // TODO: Implement ByteStream API for large blobs
            if blob_size > MAX_UPLOADABLE_BLOB_BYTES {
                warn!(
                    digest = %blob.0.hash,
                    size_mb = blob_size / (1024 * 1024),
                    "Skipping large blob (ByteStream upload not yet implemented)"
                );
                skipped_large_blobs += 1;
                continue;
            }

            // If this blob alone exceeds batch size, upload it individually
            if blob_size > MAX_SINGLE_BLOB_BYTES {
                // Flush current batch first
                if !current_batch.is_empty() {
                    self.upload_batch(&current_batch).await?;
                    uploaded_count += current_batch.len();
                    batch_count += 1;
                    current_batch.clear();
                    current_size = 0;
                }
                // Upload large blob individually
                self.upload_batch(&[blob]).await?;
                uploaded_count += 1;
                batch_count += 1;
                continue;
            }

            // If adding this blob would exceed TOTAL batch size or count limit, flush first
            let would_exceed_total_size = current_size + blob_size > MAX_BATCH_TOTAL_BYTES;
            let would_exceed_count = current_batch.len() >= MAX_UPLOAD_BATCH_COUNT;

            if (would_exceed_total_size || would_exceed_count) && !current_batch.is_empty() {
                self.upload_batch(&current_batch).await?;
                uploaded_count += current_batch.len();
                batch_count += 1;

                // Log progress every 10 batches
                if batch_count % 10 == 0 {
                    info!(
                        uploaded = uploaded_count,
                        total = blobs.len(),
                        batches = batch_count,
                        "Upload progress"
                    );
                }

                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(blob);
            current_size += blob_size;
        }

        // Upload remaining batch
        if !current_batch.is_empty() {
            self.upload_batch(&current_batch).await?;
            batch_count += 1;
        }

        if skipped_large_blobs > 0 {
            warn!(
                skipped = skipped_large_blobs,
                "Some large blobs were skipped (ByteStream API not yet implemented)"
            );
        }

        info!(
            uploaded = uploaded_count,
            skipped = skipped_large_blobs,
            batches = batch_count,
            "Completed blob upload"
        );
        Ok(())
    }

    /// Upload a single batch of blobs
    #[instrument(skip(self, blobs), fields(batch_size = blobs.len()))]
    async fn upload_batch(&self, blobs: &[&(Digest, Vec<u8>)]) -> Result<()> {
        // Calculate total data size for this batch
        let total_data_bytes: usize = blobs.iter().map(|(_, data)| data.len()).sum();
        debug!(
            blob_count = blobs.len(),
            total_bytes = total_data_bytes,
            "Uploading batch"
        );

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
            digest_function: 1, // SHA256 (REAPI enum value)
        };

        // Log the actual protobuf message size
        use prost::Message;
        let encoded_size = request.encoded_len();
        debug!(
            request_count = request.requests.len(),
            encoded_size_bytes = encoded_size,
            encoded_size_mb = encoded_size / (1024 * 1024),
            "BatchUpdateBlobs request size"
        );

        // Fail fast if the message is too large
        const MAX_GRPC_MESSAGE_SIZE: usize = 70 * 1024 * 1024; // 70MB to be safe
        if encoded_size > MAX_GRPC_MESSAGE_SIZE {
            return Err(RemoteError::upload_failed(
                "batch".to_string(),
                format!(
                    "Batch message size {} bytes exceeds limit {} bytes",
                    encoded_size, MAX_GRPC_MESSAGE_SIZE
                ),
            ));
        }

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
        blobs
            .into_iter()
            .next()
            .ok_or_else(|| RemoteError::content_not_found(digest.to_string()))
    }

    /// Upload blobs using a combination of batch and streaming uploads
    ///
    /// - Small blobs (<4MB) use BatchUpdateBlobs for efficiency
    /// - Large blobs (>=4MB) use ByteStream for streaming uploads
    ///
    /// This method accepts both in-memory blobs and file paths for large files,
    /// which avoids loading large files entirely into memory.
    #[instrument(skip(self, bytestream, blobs, file_sources), fields(blob_count = blobs.len(), file_count = file_sources.len()))]
    pub async fn upload_with_bytestream(
        &self,
        bytestream: &ByteStreamClient,
        blobs: &[(Digest, Vec<u8>)],
        file_sources: &HashMap<Digest, PathBuf>,
    ) -> Result<()> {
        if blobs.is_empty() && file_sources.is_empty() {
            return Ok(());
        }

        // Separate blobs into small (batch) and large (bytestream)
        let (small_blobs, large_blobs): (Vec<_>, Vec<_>) = blobs
            .iter()
            .partition(|(_, data)| data.len() < BYTESTREAM_THRESHOLD);

        // Count large files that need streaming
        let large_files: Vec<_> = file_sources
            .iter()
            .filter(|(d, _)| d.size_bytes as usize >= BYTESTREAM_THRESHOLD)
            .collect();

        let small_files: Vec<_> = file_sources
            .iter()
            .filter(|(d, _)| (d.size_bytes as usize) < BYTESTREAM_THRESHOLD)
            .collect();

        info!(
            small_blobs = small_blobs.len() + small_files.len(),
            large_blobs = large_blobs.len(),
            large_files = large_files.len(),
            "Uploading blobs (batch + streaming)"
        );

        // Upload small blobs via BatchUpdateBlobs
        if !small_blobs.is_empty() {
            self.batch_upload_blobs(
                &small_blobs.into_iter().cloned().collect::<Vec<_>>()
            ).await?;
        }

        // Upload small files by reading and batching
        if !small_files.is_empty() {
            let mut file_blobs = Vec::with_capacity(small_files.len());
            for (digest, path) in small_files {
                let data = std::fs::read(path).map_err(|e| {
                    RemoteError::io_error(format!("reading file {:?}", path), e)
                })?;
                file_blobs.push(((*digest).clone(), data));
            }
            self.batch_upload_blobs(&file_blobs).await?;
        }

        // Upload large in-memory blobs via ByteStream
        for (digest, data) in &large_blobs {
            info!(
                digest = %digest.hash,
                size_mb = data.len() / (1024 * 1024),
                "Streaming large blob upload"
            );
            bytestream.upload_blob(digest, data).await?;
        }

        // Upload large files via ByteStream (streaming from disk)
        for (digest, path) in large_files {
            info!(
                digest = %digest.hash,
                size_mb = digest.size_bytes / (1024 * 1024),
                path = %path.display(),
                "Streaming large file upload"
            );
            bytestream.upload_file(digest, path).await?;
        }

        info!("Upload complete");
        Ok(())
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
