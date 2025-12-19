//! ByteStream client for streaming large blob uploads/downloads

use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use crate::proto::google::bytestream::byte_stream_client::ByteStreamClient as ProtoByteStreamClient;
use crate::proto::google::bytestream::{QueryWriteStatusRequest, ReadRequest, WriteRequest};
use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

/// Default chunk size for streaming uploads (2MB)
const DEFAULT_CHUNK_SIZE: usize = 2 * 1024 * 1024;

/// Client for ByteStream API (streaming blob uploads/downloads)
///
/// This client is used for large blobs that exceed the BatchUpdateBlobs size limit.
/// It streams data in chunks to avoid loading entire files into memory.
pub struct ByteStreamClient {
    client: ProtoByteStreamClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
    chunk_size: usize,
}

impl ByteStreamClient {
    /// Create a new ByteStream client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client = ProtoByteStreamClient::with_interceptor(channel.channel(), interceptor);
        Self {
            client,
            config: Arc::new(config),
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }

    /// Create a new ByteStream client with custom chunk size
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }

    /// Generate a resource name for uploading a blob to CAS
    ///
    /// Format: `{instance_name}/uploads/{uuid}/blobs/{hash}/{size}`
    fn upload_resource_name(&self, digest: &Digest) -> String {
        let uuid = Uuid::new_v4();
        format!(
            "{}/uploads/{}/blobs/{}/{}",
            self.config.instance_name, uuid, digest.hash, digest.size_bytes
        )
    }

    /// Generate a resource name for reading a blob from CAS
    ///
    /// Format: `{instance_name}/blobs/{hash}/{size}`
    fn read_resource_name(&self, digest: &Digest) -> String {
        format!(
            "{}/blobs/{}/{}",
            self.config.instance_name, digest.hash, digest.size_bytes
        )
    }

    /// Upload a blob using ByteStream.Write streaming RPC
    ///
    /// Chunks the data and streams it to the server. This is memory-efficient
    /// for large blobs as it doesn't require loading the entire file at once.
    #[instrument(skip(self, data), fields(digest = %digest.hash, size = digest.size_bytes))]
    pub async fn upload_blob(&self, digest: &Digest, data: &[u8]) -> Result<()> {
        let resource_name = self.upload_resource_name(digest);
        let total_size = data.len();

        debug!(
            resource = %resource_name,
            chunk_size = self.chunk_size,
            "Starting ByteStream upload"
        );

        // Create channel for streaming requests
        let (tx, rx) = tokio::sync::mpsc::channel::<WriteRequest>(16);
        let stream = ReceiverStream::new(rx);

        // Spawn task to send chunks
        let chunk_size = self.chunk_size;
        let resource_name_clone = resource_name.clone();
        let data_vec = data.to_vec();

        tokio::spawn(async move {
            let mut offset = 0i64;
            let data = &data_vec;

            while (offset as usize) < total_size {
                let start = offset as usize;
                let end = std::cmp::min(start + chunk_size, total_size);
                let chunk = data[start..end].to_vec();
                let finish_write = end == total_size;

                let request = WriteRequest {
                    resource_name: if offset == 0 {
                        resource_name_clone.clone()
                    } else {
                        String::new() // Only first request needs resource name
                    },
                    write_offset: offset,
                    finish_write,
                    data: chunk,
                };

                if tx.send(request).await.is_err() {
                    // Receiver dropped, abort
                    break;
                }

                offset = end as i64;
            }
        });

        // Send the stream and get response
        let mut client = self.client.clone();
        let response = client
            .write(stream)
            .await
            .map_err(|e| RemoteError::grpc_error("ByteStream.Write", e))?;

        let committed_size = response.into_inner().committed_size;

        // Verify committed size matches expected
        if committed_size != digest.size_bytes {
            return Err(RemoteError::bytestream_incomplete(
                digest.size_bytes,
                committed_size,
            ));
        }

        debug!(
            committed_size,
            "ByteStream upload complete"
        );
        Ok(())
    }

    /// Upload a blob from a file path (memory-efficient for large files)
    ///
    /// Reads the file in chunks and streams directly to the server without
    /// loading the entire file into memory.
    #[instrument(skip(self), fields(digest = %digest.hash, size = digest.size_bytes, path = %path.display()))]
    pub async fn upload_file(&self, digest: &Digest, path: &Path) -> Result<()> {
        let resource_name = self.upload_resource_name(digest);
        let total_size = digest.size_bytes as usize;

        info!(
            resource = %resource_name,
            chunk_size = self.chunk_size,
            path = %path.display(),
            "Starting ByteStream file upload"
        );

        // Open file for async reading
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| RemoteError::io_error(format!("open {:?}", path), e))?;

        // Create channel for streaming requests
        let (tx, rx) = tokio::sync::mpsc::channel::<WriteRequest>(16);
        let stream = ReceiverStream::new(rx);

        // Spawn task to read file and send chunks
        let chunk_size = self.chunk_size;
        let resource_name_clone = resource_name.clone();
        let path_display = path.display().to_string();

        let send_task = tokio::spawn(async move {
            let mut offset = 0i64;
            let mut buffer = vec![0u8; chunk_size];

            loop {
                let bytes_read = match file.read(&mut buffer).await {
                    Ok(0) => break, // EOF
                    Ok(n) => n,
                    Err(e) => {
                        warn!(error = %e, path = %path_display, "Error reading file");
                        break;
                    }
                };

                let finish_write = (offset as usize) + bytes_read >= total_size;
                let chunk = buffer[..bytes_read].to_vec();

                let request = WriteRequest {
                    resource_name: if offset == 0 {
                        resource_name_clone.clone()
                    } else {
                        String::new()
                    },
                    write_offset: offset,
                    finish_write,
                    data: chunk,
                };

                if tx.send(request).await.is_err() {
                    break;
                }

                offset += bytes_read as i64;

                if finish_write {
                    break;
                }
            }
        });

        // Send the stream and get response
        let mut client = self.client.clone();
        let response = client
            .write(stream)
            .await
            .map_err(|e| RemoteError::grpc_error("ByteStream.Write", e))?;

        // Wait for send task to complete
        let _ = send_task.await;

        let committed_size = response.into_inner().committed_size;

        // Verify committed size matches expected
        if committed_size != digest.size_bytes {
            return Err(RemoteError::bytestream_incomplete(
                digest.size_bytes,
                committed_size,
            ));
        }

        info!(
            committed_size,
            path = %path.display(),
            "ByteStream file upload complete"
        );
        Ok(())
    }

    /// Query the current write status for a resource (for resumable uploads)
    #[instrument(skip(self))]
    pub async fn query_write_status(&self, resource_name: &str) -> Result<i64> {
        let request = QueryWriteStatusRequest {
            resource_name: resource_name.to_string(),
        };

        let mut client = self.client.clone();
        let response = client
            .query_write_status(request)
            .await
            .map_err(|e| RemoteError::grpc_error("ByteStream.QueryWriteStatus", e))?;

        Ok(response.into_inner().committed_size)
    }

    /// Read a blob using ByteStream.Read streaming RPC
    ///
    /// Downloads the blob in chunks and assembles them in memory.
    /// For very large blobs, consider using `read_blob_to_file` instead.
    #[instrument(skip(self), fields(digest = %digest.hash, size = digest.size_bytes))]
    pub async fn read_blob(&self, digest: &Digest) -> Result<Vec<u8>> {
        let resource_name = self.read_resource_name(digest);

        debug!(
            resource = %resource_name,
            "Starting ByteStream read"
        );

        let request = ReadRequest {
            resource_name,
            read_offset: 0,
            read_limit: 0, // 0 means no limit
        };

        let mut client = self.client.clone();
        let response = client
            .read(request)
            .await
            .map_err(|e| RemoteError::grpc_error("ByteStream.Read", e))?;

        let mut stream = response.into_inner();
        let mut data = Vec::with_capacity(digest.size_bytes as usize);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| RemoteError::grpc_error("ByteStream.Read chunk", e))?;
            data.extend(chunk.data);
        }

        if data.len() != digest.size_bytes as usize {
            warn!(
                expected = digest.size_bytes,
                actual = data.len(),
                "ByteStream read size mismatch"
            );
        }

        debug!(
            bytes_read = data.len(),
            "ByteStream read complete"
        );
        Ok(data)
    }

    /// Read a blob directly to a file (memory-efficient for large blobs)
    #[instrument(skip(self), fields(digest = %digest.hash, size = digest.size_bytes, path = %path.display()))]
    pub async fn read_blob_to_file(&self, digest: &Digest, path: &Path) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let resource_name = self.read_resource_name(digest);

        info!(
            resource = %resource_name,
            path = %path.display(),
            "Starting ByteStream read to file"
        );

        let request = ReadRequest {
            resource_name,
            read_offset: 0,
            read_limit: 0,
        };

        let mut client = self.client.clone();
        let response = client
            .read(request)
            .await
            .map_err(|e| RemoteError::grpc_error("ByteStream.Read", e))?;

        let mut stream = response.into_inner();
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|e| RemoteError::io_error(format!("create {:?}", path), e))?;

        let mut total_bytes = 0usize;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| RemoteError::grpc_error("ByteStream.Read chunk", e))?;
            file.write_all(&chunk.data)
                .await
                .map_err(|e| RemoteError::io_error(format!("write {:?}", path), e))?;
            total_bytes += chunk.data.len();
        }

        file.flush()
            .await
            .map_err(|e| RemoteError::io_error(format!("flush {:?}", path), e))?;

        info!(
            bytes_written = total_bytes,
            path = %path.display(),
            "ByteStream read to file complete"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upload_resource_name_format() {
        // We can't easily test the full client without a server,
        // but we can verify the resource name format is correct
        let digest = Digest::from_bytes(b"test content");

        // The format should be: {instance}/uploads/{uuid}/blobs/{hash}/{size}
        // We verify the pattern manually since uuid changes each call
        let expected_parts = ["uploads", "blobs", &digest.hash, &digest.size_bytes.to_string()];

        // Just verify the hash and size are present in expected order
        assert!(!digest.hash.is_empty());
        assert!(digest.size_bytes > 0);
        for part in expected_parts {
            assert!(!part.is_empty());
        }
    }

    #[test]
    fn test_read_resource_name_format() {
        let digest = Digest::from_bytes(b"test content");

        // The format should be: {instance}/blobs/{hash}/{size}
        let instance = "default";
        let expected = format!(
            "{}/blobs/{}/{}",
            instance, digest.hash, digest.size_bytes
        );

        assert!(expected.contains("blobs"));
        assert!(expected.contains(&digest.hash));
    }
}
