//! [`Cas`] over the REAPI `ContentAddressableStorage` and `ByteStream`
//! services.

use crate::client::RemoteClient;
use crate::error::{Error, Result};
use async_trait::async_trait;
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::build::bazel::remote::execution::v2::content_addressable_storage_client::ContentAddressableStorageClient as CasClient;
use bazel_remote_apis::google::bytestream::byte_stream_client::ByteStreamClient;
use bazel_remote_apis::google::bytestream::{ReadRequest, WriteRequest};
use cuenv_cas::{Cas, Digest};
use futures::StreamExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace};

/// Payload size below which a blob is exchanged with `BatchUpdateBlobs` /
/// `BatchReadBlobs` rather than streamed.
///
/// Used when the server does not advertise a limit of its own. It matches the
/// conventional gRPC 4 MiB message ceiling with room for framing, which is
/// what Bazel itself assumes in the same situation.
const DEFAULT_MAX_BATCH_SIZE: i64 = 4 * 1024 * 1024 - 64 * 1024;

/// A content-addressed store backed by a REAPI server.
#[derive(Debug)]
pub struct RemoteCas {
    client: RemoteClient,
    /// `max_batch_total_size_bytes` from the server's capabilities.
    max_batch_size: i64,
    /// Counter making each `ByteStream` upload resource name unique.
    upload_counter: AtomicU64,
}

impl RemoteCas {
    /// Build a store on an existing client, asking the server for its limits.
    ///
    /// # Errors
    ///
    /// Returns an error if the capabilities handshake fails or the server is
    /// not compatible with cuenv.
    pub async fn connect(client: RemoteClient) -> Result<Self> {
        let max_batch_size = client
            .check_capabilities()
            .await?
            .unwrap_or(DEFAULT_MAX_BATCH_SIZE);
        debug!(max_batch_size, "connected to remote CAS");
        Ok(Self {
            client,
            max_batch_size,
            upload_counter: AtomicU64::new(0),
        })
    }

    /// Build a store without a capabilities handshake, assuming defaults.
    #[must_use]
    pub fn new_unchecked(client: RemoteClient) -> Self {
        Self {
            client,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            upload_counter: AtomicU64::new(0),
        }
    }

    fn cas_client(&self) -> CasClient<tonic::transport::Channel> {
        CasClient::new(self.client.channel())
    }

    fn bytestream_client(&self) -> ByteStreamClient<tonic::transport::Channel> {
        ByteStreamClient::new(self.client.channel())
    }

    fn fits_in_a_batch(&self, size_bytes: u64) -> bool {
        i64::try_from(size_bytes).is_ok_and(|size| size <= self.max_batch_size)
    }

    /// `{instance}/blobs/{hash}/{size}` — the REAPI read resource name.
    fn read_resource_name(&self, digest: &Digest) -> String {
        let instance = self.client.instance_name();
        if instance.is_empty() {
            format!("blobs/{}/{}", digest.hash, digest.size_bytes)
        } else {
            format!("{instance}/blobs/{}/{}", digest.hash, digest.size_bytes)
        }
    }

    /// `{instance}/uploads/{uuid}/blobs/{hash}/{size}` — the REAPI write
    /// resource name. The uuid segment only has to be unique per upload
    /// attempt; it is not a content identifier.
    fn write_resource_name(&self, digest: &Digest) -> String {
        let unique = format!(
            "{:016x}-{:08x}",
            std::process::id(),
            self.upload_counter.fetch_add(1, Ordering::Relaxed)
        );
        let instance = self.client.instance_name();
        let tail = format!(
            "uploads/{unique}/blobs/{}/{}",
            digest.hash, digest.size_bytes
        );
        if instance.is_empty() {
            tail
        } else {
            format!("{instance}/{tail}")
        }
    }

    /// Digests from `digests` the server does not hold.
    ///
    /// This is the call that makes uploads cheap: ask once which blobs are
    /// missing, then send only those.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn find_missing(&self, digests: &[Digest]) -> Result<Vec<Digest>> {
        if digests.is_empty() {
            return Ok(Vec::new());
        }
        let mut blob_digests = Vec::with_capacity(digests.len());
        for digest in digests {
            blob_digests.push(digest.to_proto()?);
        }

        let request = self.client.request(pb::FindMissingBlobsRequest {
            instance_name: self.client.instance_name(),
            blob_digests,
            ..Default::default()
        })?;
        let response = self
            .cas_client()
            .find_missing_blobs(request)
            .await
            .map_err(|status| Error::rpc("FindMissingBlobs", &status))?
            .into_inner();

        response
            .missing_blob_digests
            .iter()
            .map(|proto| Digest::from_proto(proto).map_err(Error::from))
            .collect()
    }

    async fn read_batched(&self, digest: &Digest) -> Result<Vec<u8>> {
        let request = self.client.request(pb::BatchReadBlobsRequest {
            instance_name: self.client.instance_name(),
            digests: vec![digest.to_proto()?],
            ..Default::default()
        })?;
        let response = self
            .cas_client()
            .batch_read_blobs(request)
            .await
            .map_err(|status| Error::rpc("BatchReadBlobs", &status))?
            .into_inner();

        let entry = response.responses.into_iter().next().ok_or_else(|| {
            Error::protocol("BatchReadBlobs", "server returned no response for the blob")
        })?;
        if let Some(status) = entry.status
            && status.code != 0
        {
            return Err(Error::protocol(
                "BatchReadBlobs",
                format!("status {}: {}", status.code, status.message),
            ));
        }
        Ok(entry.data)
    }

    async fn read_streamed(&self, digest: &Digest) -> Result<Vec<u8>> {
        let request = self.client.request(ReadRequest {
            resource_name: self.read_resource_name(digest),
            read_offset: 0,
            read_limit: 0,
        })?;
        let mut stream = self
            .bytestream_client()
            .read(request)
            .await
            .map_err(|status| Error::rpc("ByteStream.Read", &status))?
            .into_inner();

        let mut bytes = Vec::with_capacity(usize::try_from(digest.size_bytes).unwrap_or(0));
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|status| Error::rpc("ByteStream.Read", &status))?;
            bytes.extend_from_slice(&chunk.data);
        }
        Ok(bytes)
    }

    async fn write_batched(&self, digest: &Digest, bytes: &[u8]) -> Result<()> {
        let request = self.client.request(pb::BatchUpdateBlobsRequest {
            instance_name: self.client.instance_name(),
            requests: vec![pb::batch_update_blobs_request::Request {
                digest: Some(digest.to_proto()?),
                data: bytes.to_vec(),
                ..Default::default()
            }],
            ..Default::default()
        })?;
        let response = self
            .cas_client()
            .batch_update_blobs(request)
            .await
            .map_err(|status| Error::rpc("BatchUpdateBlobs", &status))?
            .into_inner();

        for entry in response.responses {
            if let Some(status) = entry.status
                && status.code != 0
            {
                return Err(Error::protocol(
                    "BatchUpdateBlobs",
                    format!("status {}: {}", status.code, status.message),
                ));
            }
        }
        Ok(())
    }

    async fn write_streamed(&self, digest: &Digest, bytes: &[u8]) -> Result<()> {
        // Chunk well under the gRPC message ceiling; the resource name and
        // framing share the budget with the payload.
        const CHUNK: usize = 1024 * 1024;

        let resource_name = self.write_resource_name(digest);
        let total = bytes.len();
        let chunks: Vec<WriteRequest> = bytes
            .chunks(CHUNK)
            .enumerate()
            .map(|(index, chunk)| {
                let offset = index * CHUNK;
                WriteRequest {
                    // REAPI wants the resource name on the first request only.
                    resource_name: if index == 0 {
                        resource_name.clone()
                    } else {
                        String::new()
                    },
                    write_offset: i64::try_from(offset).unwrap_or(i64::MAX),
                    finish_write: offset + chunk.len() >= total,
                    data: chunk.to_vec(),
                }
            })
            .collect();

        // An empty blob still needs one request, with `finish_write` set, or
        // the server never commits it.
        let chunks = if chunks.is_empty() {
            vec![WriteRequest {
                resource_name,
                write_offset: 0,
                finish_write: true,
                data: Vec::new(),
            }]
        } else {
            chunks
        };

        let request = self.client.request(tokio_stream::iter(chunks))?;
        let response = self
            .bytestream_client()
            .write(request)
            .await
            .map_err(|status| Error::rpc("ByteStream.Write", &status))?
            .into_inner();

        let committed = usize::try_from(response.committed_size).unwrap_or(0);
        if committed != total {
            return Err(Error::protocol(
                "ByteStream.Write",
                format!("server committed {committed} of {total} bytes"),
            ));
        }
        Ok(())
    }

    async fn put(&self, bytes: &[u8]) -> Result<Digest> {
        self.client.ensure_writable("upload a blob")?;
        let digest = Digest::of_bytes(bytes);

        // Skip the upload when the server already holds the blob. Content
        // addressing makes this safe and it is the common case in a warm
        // cache.
        if self.find_missing(std::slice::from_ref(&digest)).await?.is_empty() {
            trace!(digest = %digest, "remote CAS already holds the blob");
            return Ok(digest);
        }

        if self.fits_in_a_batch(digest.size_bytes) {
            self.write_batched(&digest, bytes).await?;
        } else {
            self.write_streamed(&digest, bytes).await?;
        }
        trace!(digest = %digest, "uploaded blob to remote CAS");
        Ok(digest)
    }
}

#[async_trait]
impl Cas for RemoteCas {
    async fn contains(&self, digest: &Digest) -> cuenv_cas::Result<bool> {
        let missing = self
            .find_missing(std::slice::from_ref(digest))
            .await
            .map_err(cuenv_cas::Error::from)?;
        Ok(missing.is_empty())
    }

    async fn get(&self, digest: &Digest) -> cuenv_cas::Result<Vec<u8>> {
        let bytes = if self.fits_in_a_batch(digest.size_bytes) {
            self.read_batched(digest).await
        } else {
            self.read_streamed(digest).await
        }
        .map_err(cuenv_cas::Error::from)?;

        // The server is not part of cuenv's trust boundary: a wrong blob here
        // would be installed into the workspace as if the task had produced
        // it. Verify before anyone sees it.
        let actual = Digest::of_bytes(&bytes);
        if &actual != digest {
            return Err(cuenv_cas::Error::digest_mismatch(
                digest.to_resource(),
                actual.to_resource(),
            ));
        }
        Ok(bytes)
    }

    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> cuenv_cas::Result<()> {
        let bytes = self.get(digest).await?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| cuenv_cas::Error::io(e, parent, "create_dir_all"))?;
        }
        std::fs::write(destination, &bytes)
            .map_err(|e| cuenv_cas::Error::io(e, destination, "write"))?;
        Ok(())
    }

    async fn put_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        Ok(self.put(bytes).await?)
    }

    async fn put_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        let bytes =
            std::fs::read(source).map_err(|e| cuenv_cas::Error::io(e, source, "read"))?;
        Ok(self.put(&bytes).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RemoteConfig;
    use tonic::transport::Channel;

    fn cas(instance: &str) -> RemoteCas {
        let channel = Channel::builder("http://127.0.0.1:1".parse().unwrap()).connect_lazy();
        RemoteCas::new_unchecked(RemoteClient::from_channel(
            channel,
            RemoteConfig::new("grpc://127.0.0.1:1").with_instance_name(instance),
        ))
    }

    #[tokio::test]
    async fn read_resource_name_omits_an_empty_instance() {
        let digest = Digest::of_bytes(b"x");
        let name = cas("").read_resource_name(&digest);
        assert_eq!(name, format!("blobs/{}/1", digest.hash));
    }

    #[tokio::test]
    async fn read_resource_name_includes_the_instance() {
        let digest = Digest::of_bytes(b"x");
        let name = cas("my-instance").read_resource_name(&digest);
        assert_eq!(name, format!("my-instance/blobs/{}/1", digest.hash));
    }

    #[tokio::test]
    async fn write_resource_names_are_unique_per_attempt() {
        let store = cas("");
        let digest = Digest::of_bytes(b"x");
        assert_ne!(
            store.write_resource_name(&digest),
            store.write_resource_name(&digest)
        );
    }

    #[tokio::test]
    async fn write_resource_name_has_the_reapi_shape() {
        let digest = Digest::of_bytes(b"x");
        let name = cas("inst").write_resource_name(&digest);
        assert!(name.starts_with("inst/uploads/"), "{name}");
        assert!(name.ends_with(&format!("/blobs/{}/1", digest.hash)), "{name}");
    }

    #[tokio::test]
    async fn batch_threshold_follows_the_advertised_limit() {
        let store = cas("");
        assert!(store.fits_in_a_batch(1024));
        assert!(!store.fits_in_a_batch(u64::try_from(DEFAULT_MAX_BATCH_SIZE).unwrap() + 1));
    }
}
