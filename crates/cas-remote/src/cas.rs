//! [`Cas`] over the REAPI `ContentAddressableStorage` and `ByteStream`
//! services.

use crate::client::{RemoteClient, ServerCapabilities};
use crate::error::{Error, Result};
use async_trait::async_trait;
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::build::bazel::remote::execution::v2::content_addressable_storage_client::ContentAddressableStorageClient as CasClient;
use bazel_remote_apis::google::bytestream::byte_stream_client::ByteStreamClient;
use bazel_remote_apis::google::bytestream::{ReadRequest, WriteRequest};
use cuenv_cas::{Cas, Digest};
use futures::StreamExt;
use prost::Message as _;
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, trace};
use uuid::Uuid;

/// Payload size below which a blob is exchanged with `BatchUpdateBlobs` /
/// `BatchReadBlobs` rather than streamed.
///
/// Used when the server does not advertise a limit of its own. It matches the
/// conventional gRPC 4 MiB message ceiling with room for framing, which is
/// what Bazel itself assumes in the same situation.
const DEFAULT_MAX_BATCH_SIZE: i64 = 4 * 1024 * 1024 - 64 * 1024;
/// Upper bound on a single `FindMissingBlobsRequest` encoded size (well under
/// the 4 MiB gRPC frame limit).
const MAX_FIND_MISSING_ENCODED_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on digest count in a single `FindMissingBlobsRequest`.
const MAX_FIND_MISSING_DIGESTS: usize = 4096;
/// Minimum blob size before applying Zstandard compression.
const MIN_COMPRESS_SIZE: u64 = 4 * 1024;
/// The in-memory [`Cas`] API cannot safely represent unbounded hostile
/// responses. Larger artifacts must use a future file-streaming API.
const MAX_IN_MEMORY_BLOB_SIZE: u64 = 512 * 1024 * 1024;
const MAX_INITIAL_ALLOCATION: usize = 8 * 1024 * 1024;

/// A content-addressed store backed by a REAPI server.
#[derive(Debug)]
pub struct RemoteCas {
    client: RemoteClient,
    /// `max_batch_total_size_bytes` from the server's capabilities.
    max_batch_size: i64,
    /// Negotiated server capabilities (compression & batching limits).
    capabilities: ServerCapabilities,
}

impl RemoteCas {
    /// Build a store on an existing client, asking the server for its limits.
    ///
    /// # Errors
    ///
    /// Returns an error if the capabilities handshake fails or the server is
    /// not compatible with cuenv.
    pub async fn connect(client: RemoteClient) -> Result<Self> {
        let capabilities = client.capabilities().await?;
        let advertised_max_batch_size = capabilities
            .max_batch_total_size_bytes
            .unwrap_or(DEFAULT_MAX_BATCH_SIZE);
        let max_batch_size = advertised_max_batch_size.clamp(0, DEFAULT_MAX_BATCH_SIZE);
        debug!(
            max_batch_size,
            bytestream_zstd = capabilities.supports_bytestream_zstd(),
            batch_update_zstd = capabilities.supports_batch_update_zstd(),
            batch_read_zstd = capabilities.supports_batch_read_zstd(),
            "connected to remote CAS"
        );
        Ok(Self {
            client,
            max_batch_size,
            capabilities,
        })
    }

    /// Build a store without a capabilities handshake, assuming defaults.
    #[must_use]
    pub fn new_unchecked(client: RemoteClient) -> Self {
        Self {
            client,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            capabilities: ServerCapabilities::default(),
        }
    }

    /// Build a store with explicit capabilities (useful in tests).
    #[must_use]
    pub fn with_capabilities(client: RemoteClient, capabilities: ServerCapabilities) -> Self {
        let max_batch_size = capabilities
            .max_batch_total_size_bytes
            .unwrap_or(DEFAULT_MAX_BATCH_SIZE)
            .clamp(0, DEFAULT_MAX_BATCH_SIZE);
        Self {
            client,
            max_batch_size,
            capabilities,
        }
    }

    /// Negotiated server capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    /// Maximum total bytes in a single batch RPC.
    #[must_use]
    pub fn max_batch_size(&self) -> i64 {
        self.max_batch_size
    }

    fn cas_client(&self) -> CasClient<tonic::transport::Channel> {
        CasClient::new(self.client.channel())
    }

    fn bytestream_client(&self) -> ByteStreamClient<tonic::transport::Channel> {
        ByteStreamClient::new(self.client.channel())
    }

    /// Whether a blob of `size_bytes` fits inside a single batch RPC.
    #[must_use]
    pub fn fits_in_a_batch(&self, size_bytes: u64) -> bool {
        i64::try_from(size_bytes).is_ok_and(|size| size <= self.max_batch_size)
    }

    /// `{instance}/blobs/{hash}/{size}` — the REAPI read resource name.
    fn read_resource_name(&self, digest: &Digest, compressor: crate::client::Compressor) -> String {
        let instance = self.client.instance_name();
        let segment = match compressor {
            crate::client::Compressor::Identity => "blobs",
            crate::client::Compressor::Zstd => "compressed-blobs/zstd",
        };
        if instance.is_empty() {
            format!("{segment}/{}/{}", digest.hash, digest.size_bytes)
        } else {
            format!("{instance}/{segment}/{}/{}", digest.hash, digest.size_bytes)
        }
    }

    /// `{instance}/uploads/{uuid}/blobs/{hash}/{size}` (or `compressed-blobs/zstd`)
    /// — the REAPI write resource name. The uuid segment only has to be unique
    /// per upload attempt; it is not a content identifier.
    fn write_resource_name(
        &self,
        digest: &Digest,
        compressor: crate::client::Compressor,
    ) -> String {
        let unique = Uuid::new_v4();
        let instance = self.client.instance_name();
        let segment = match compressor {
            crate::client::Compressor::Identity => "blobs",
            crate::client::Compressor::Zstd => "compressed-blobs/zstd",
        };
        let tail = format!(
            "uploads/{unique}/{segment}/{}/{}",
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
    /// Automatically splits large requests by both digest count and encoded
    /// protobuf size so requests never exceed gRPC message limits.
    ///
    /// # Errors
    ///
    /// Returns an error if any RPC fails or a digest is invalid.
    pub async fn find_missing(&self, digests: &[Digest]) -> Result<Vec<Digest>> {
        if digests.is_empty() {
            return Ok(Vec::new());
        }

        let instance_name = self.client.instance_name();
        let base_overhead = instance_name.len().saturating_add(64);
        let mut missing = Vec::new();
        let mut chunk = Vec::new();
        let mut chunk_bytes = base_overhead;

        for digest in digests {
            digest.validate()?;
            let proto = digest.to_proto()?;
            // Account for protobuf field tag + length prefix + message payload.
            let entry_bytes = proto.encoded_len().saturating_add(8);

            if !chunk.is_empty()
                && (chunk.len() >= MAX_FIND_MISSING_DIGESTS
                    || chunk_bytes.saturating_add(entry_bytes) > MAX_FIND_MISSING_ENCODED_BYTES)
            {
                let batch = std::mem::take(&mut chunk);
                missing.extend(self.find_missing_chunk(batch).await?);
                chunk_bytes = base_overhead;
            }

            chunk_bytes = chunk_bytes.saturating_add(entry_bytes);
            chunk.push(proto);
        }

        if !chunk.is_empty() {
            missing.extend(self.find_missing_chunk(chunk).await?);
        }

        Ok(missing)
    }

    async fn find_missing_chunk(&self, blob_digests: Vec<pb::Digest>) -> Result<Vec<Digest>> {
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

    /// Upload multiple small blobs using packed `BatchUpdateBlobs` RPCs.
    ///
    /// Validates every per-item status and digest in each server response
    /// before returning `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns an error if the client is read-only, a blob exceeds
    /// `max_batch_size`, or any per-item status/digest check fails.
    pub async fn batch_update_blobs(&self, blobs: &[(Digest, Vec<u8>)]) -> Result<()> {
        self.client.ensure_writable("batch upload blobs")?;
        if blobs.is_empty() {
            return Ok(());
        }

        let max_bytes = usize::try_from(self.max_batch_size).unwrap_or(0);
        let use_zstd = self.capabilities.supports_batch_update_zstd();
        let mut current_requests = Vec::new();
        let mut current_expected = HashSet::new();
        let mut current_bytes = 0_usize;

        for (digest, raw) in blobs {
            digest.validate()?;
            let (compressor, payload) = if use_zstd && digest.size_bytes >= MIN_COMPRESS_SIZE {
                let compressed = compress_zstd(raw)?;
                if compressed.len() < raw.len() {
                    (pb::compressor::Value::Zstd as i32, compressed)
                } else {
                    (pb::compressor::Value::Identity as i32, raw.clone())
                }
            } else {
                (pb::compressor::Value::Identity as i32, raw.clone())
            };

            let item_bytes = payload.len().saturating_add(128);
            if item_bytes > max_bytes && max_bytes > 0 {
                return Err(Error::protocol(
                    "BatchUpdateBlobs",
                    format!(
                        "blob {} ({} bytes) exceeds max_batch_total_size_bytes ({max_bytes})",
                        digest.to_resource(),
                        payload.len()
                    ),
                ));
            }

            if !current_requests.is_empty() && current_bytes.saturating_add(item_bytes) > max_bytes
            {
                let reqs = std::mem::take(&mut current_requests);
                let expected = std::mem::take(&mut current_expected);
                self.send_batch_update(reqs, &expected).await?;
                current_bytes = 0;
            }

            current_bytes = current_bytes.saturating_add(item_bytes);
            current_expected.insert(digest.clone());
            current_requests.push(pb::batch_update_blobs_request::Request {
                digest: Some(digest.to_proto()?),
                data: payload,
                compressor,
            });
        }

        if !current_requests.is_empty() {
            self.send_batch_update(current_requests, &current_expected)
                .await?;
        }
        Ok(())
    }

    async fn send_batch_update(
        &self,
        requests: Vec<pb::batch_update_blobs_request::Request>,
        expected: &HashSet<Digest>,
    ) -> Result<()> {
        let request = self.client.request(pb::BatchUpdateBlobsRequest {
            instance_name: self.client.instance_name(),
            requests,
            ..Default::default()
        })?;
        let response = self
            .cas_client()
            .batch_update_blobs(request)
            .await
            .map_err(|status| Error::rpc("BatchUpdateBlobs", &status))?
            .into_inner();

        let mut seen = HashSet::with_capacity(expected.len());
        for entry in response.responses {
            let proto_digest = entry.digest.as_ref().ok_or_else(|| {
                Error::protocol("BatchUpdateBlobs", "server response omitted blob digest")
            })?;
            let actual_digest = Digest::from_proto(proto_digest)?;
            if !expected.contains(&actual_digest) {
                return Err(Error::protocol(
                    "BatchUpdateBlobs",
                    format!(
                        "server responded for unexpected digest {}",
                        actual_digest.to_resource()
                    ),
                ));
            }
            if let Some(status) = entry.status
                && status.code != 0
            {
                return Err(Error::protocol(
                    "BatchUpdateBlobs",
                    format!(
                        "blob {} failed with status {}: {}",
                        actual_digest.to_resource(),
                        status.code,
                        status.message
                    ),
                ));
            }
            seen.insert(actual_digest);
        }

        if seen.len() != expected.len() {
            return Err(Error::protocol(
                "BatchUpdateBlobs",
                format!(
                    "server acknowledged {} of {} uploaded blobs",
                    seen.len(),
                    expected.len()
                ),
            ));
        }
        Ok(())
    }

    /// Download multiple small blobs using packed `BatchReadBlobs` RPCs.
    ///
    /// Validates every per-item status, digest, and SHA-256 content hash.
    ///
    /// # Errors
    ///
    /// Returns an error if any RPC fails, a blob is missing, decompression
    /// fails, or any content digest mismatches.
    pub async fn batch_read_blobs(&self, digests: &[Digest]) -> Result<HashMap<Digest, Vec<u8>>> {
        if digests.is_empty() {
            return Ok(HashMap::new());
        }

        let max_bytes = u64::try_from(self.max_batch_size).unwrap_or(0);
        let mut results = HashMap::with_capacity(digests.len());
        let mut current = Vec::new();
        let mut current_bytes = 0_u64;

        for digest in digests {
            digest.validate()?;
            let item_bytes = digest.size_bytes.saturating_add(128);
            if !current.is_empty() && current_bytes.saturating_add(item_bytes) > max_bytes {
                let batch = std::mem::take(&mut current);
                results.extend(self.send_batch_read(&batch).await?);
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(item_bytes);
            current.push(digest.clone());
        }

        if !current.is_empty() {
            results.extend(self.send_batch_read(&current).await?);
        }

        Ok(results)
    }

    async fn send_batch_read(&self, batch: &[Digest]) -> Result<HashMap<Digest, Vec<u8>>> {
        let mut proto_digests = Vec::with_capacity(batch.len());
        let mut expected = HashSet::with_capacity(batch.len());
        for digest in batch {
            proto_digests.push(digest.to_proto()?);
            expected.insert(digest.clone());
        }

        let acceptable_compressors = if self.capabilities.supports_batch_read_zstd() {
            vec![
                pb::compressor::Value::Zstd as i32,
                pb::compressor::Value::Identity as i32,
            ]
        } else {
            Vec::new()
        };

        let request = self.client.request(pb::BatchReadBlobsRequest {
            instance_name: self.client.instance_name(),
            digests: proto_digests,
            acceptable_compressors,
            ..Default::default()
        })?;
        let response = self
            .cas_client()
            .batch_read_blobs(request)
            .await
            .map_err(|status| Error::rpc("BatchReadBlobs", &status))?
            .into_inner();

        let mut out = HashMap::with_capacity(expected.len());
        for entry in response.responses {
            let proto_digest = entry.digest.as_ref().ok_or_else(|| {
                Error::protocol("BatchReadBlobs", "server response omitted blob digest")
            })?;
            let digest = Digest::from_proto(proto_digest)?;
            if !expected.contains(&digest) {
                return Err(Error::protocol(
                    "BatchReadBlobs",
                    format!(
                        "server responded for unexpected digest {}",
                        digest.to_resource()
                    ),
                ));
            }
            if let Some(status) = entry.status
                && status.code != 0
            {
                return Err(Error::protocol(
                    "BatchReadBlobs",
                    format!(
                        "blob {} failed with status {}: {}",
                        digest.to_resource(),
                        status.code,
                        status.message
                    ),
                ));
            }
            let data = decode_compressed_blob(
                "BatchReadBlobs",
                entry.compressor,
                &entry.data,
                digest.size_bytes,
            )?;
            let actual = Digest::of_bytes(&data);
            if actual != digest {
                return Err(Error::Cas(cuenv_cas::Error::digest_mismatch(
                    digest.to_resource(),
                    actual.to_resource(),
                )));
            }
            out.insert(digest, data);
        }

        if out.len() != expected.len() {
            return Err(Error::protocol(
                "BatchReadBlobs",
                format!(
                    "server returned {} of {} requested blobs",
                    out.len(),
                    expected.len()
                ),
            ));
        }
        Ok(out)
    }

    async fn read_batched(&self, digest: &Digest) -> Result<Vec<u8>> {
        let mut results = self.send_batch_read(std::slice::from_ref(digest)).await?;
        results.remove(digest).ok_or_else(|| {
            Error::protocol(
                "BatchReadBlobs",
                format!("missing expected digest {}", digest.to_resource()),
            )
        })
    }

    async fn read_streamed(&self, digest: &Digest) -> Result<Vec<u8>> {
        digest.validate()?;
        if digest.size_bytes > MAX_IN_MEMORY_BLOB_SIZE {
            return Err(Error::protocol(
                "ByteStream.Read",
                format!(
                    "blob size {} exceeds cuenv's in-memory limit of {MAX_IN_MEMORY_BLOB_SIZE}",
                    digest.size_bytes
                ),
            ));
        }
        let use_zstd =
            self.capabilities.supports_bytestream_zstd() && digest.size_bytes >= MIN_COMPRESS_SIZE;
        // REAPI v2.1: compressed ByteStream reads MUST set read_limit = 0.
        let read_limit = if use_zstd {
            0
        } else {
            i64::try_from(digest.size_bytes)
                .map_err(|_| Error::protocol("ByteStream.Read", "blob size exceeds i64"))?
        };
        let compressor = if use_zstd {
            crate::client::Compressor::Zstd
        } else {
            crate::client::Compressor::Identity
        };
        let request = self.client.request(ReadRequest {
            resource_name: self.read_resource_name(digest, compressor),
            read_offset: 0,
            read_limit,
        })?;
        let mut stream = self
            .bytestream_client()
            .read(request)
            .await
            .map_err(|status| Error::rpc("ByteStream.Read", &status))?
            .into_inner();

        let initial_capacity = usize::try_from(digest.size_bytes)
            .unwrap_or(MAX_INITIAL_ALLOCATION)
            .min(MAX_INITIAL_ALLOCATION);
        if use_zstd {
            let mut bounded = BoundedVecWriter::new(initial_capacity, digest.size_bytes);
            let mut decoder = zstd::stream::write::Decoder::new(&mut bounded).map_err(|e| {
                Error::protocol("ByteStream.Read", format!("init zstd decoder: {e}"))
            })?;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|status| Error::rpc("ByteStream.Read", &status))?;
                decoder.write_all(&chunk.data).map_err(|e| {
                    Error::protocol("ByteStream.Read", format!("zstd decompress failed: {e}"))
                })?;
            }
            decoder.flush().map_err(|e| {
                Error::protocol("ByteStream.Read", format!("zstd flush failed: {e}"))
            })?;
            drop(decoder);
            let bytes = bounded.into_inner();
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != digest.size_bytes {
                return Err(Error::protocol(
                    "ByteStream.Read",
                    format!(
                        "decompressed {} of {} declared bytes",
                        bytes.len(),
                        digest.size_bytes
                    ),
                ));
            }
            return Ok(bytes);
        }

        let mut bytes = Vec::with_capacity(initial_capacity);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|status| Error::rpc("ByteStream.Read", &status))?;
            let next_size = bytes
                .len()
                .checked_add(chunk.data.len())
                .ok_or_else(|| Error::protocol("ByteStream.Read", "response size overflow"))?;
            if u64::try_from(next_size).unwrap_or(u64::MAX) > digest.size_bytes {
                return Err(Error::protocol(
                    "ByteStream.Read",
                    format!(
                        "server sent more than the declared {} bytes",
                        digest.size_bytes
                    ),
                ));
            }
            bytes.extend_from_slice(&chunk.data);
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != digest.size_bytes {
            return Err(Error::protocol(
                "ByteStream.Read",
                format!(
                    "server sent {} of {} declared bytes",
                    bytes.len(),
                    digest.size_bytes
                ),
            ));
        }
        Ok(bytes)
    }

    async fn write_batched(&self, digest: &Digest, bytes: &[u8]) -> Result<()> {
        self.batch_update_blobs(&[(digest.clone(), bytes.to_vec())])
            .await
    }

    async fn write_streamed(&self, digest: &Digest, bytes: &[u8]) -> Result<()> {
        // Chunk well under the gRPC message ceiling; the resource name and
        // framing share the budget with the payload.
        const CHUNK: usize = 1024 * 1024;

        let use_zstd =
            self.capabilities.supports_bytestream_zstd() && digest.size_bytes >= MIN_COMPRESS_SIZE;
        let payload_storage;
        let wire_bytes: &[u8] = if use_zstd {
            payload_storage = compress_zstd(bytes)?;
            &payload_storage
        } else {
            bytes
        };

        let compressor = if use_zstd {
            crate::client::Compressor::Zstd
        } else {
            crate::client::Compressor::Identity
        };
        let resource_name = self.write_resource_name(digest, compressor);
        let wire_total = wire_bytes.len();
        let chunks: Vec<WriteRequest> = wire_bytes
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
                    finish_write: offset + chunk.len() >= wire_total,
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

        // REAPI v2.1: committed_size == -1 means the server already had the
        // complete blob (e.g., another concurrent writer finished first).
        if response.committed_size == -1 {
            return Ok(());
        }
        let committed = u64::try_from(response.committed_size).unwrap_or(u64::MAX);
        if committed != digest.size_bytes {
            return Err(Error::protocol(
                "ByteStream.Write",
                format!(
                    "server committed {committed} of {} bytes",
                    digest.size_bytes
                ),
            ));
        }
        Ok(())
    }

    async fn digest_file(source: &Path) -> cuenv_cas::Result<Digest> {
        let mut file = tokio::fs::File::open(source)
            .await
            .map_err(|error| cuenv_cas::Error::io(error, source, "open"))?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, source, "read"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            size += count as u64;
        }
        Digest::try_new(hex::encode(hasher.finalize()), size)
    }

    async fn write_file_streamed(&self, digest: &Digest, source: &Path) -> cuenv_cas::Result<()> {
        const CHUNK: usize = 1024 * 1024;

        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let source = source.to_path_buf();
        let resource_name = self.write_resource_name(digest, crate::client::Compressor::Identity);
        let expected_size = digest.size_bytes;
        let expected_digest = digest.clone();
        let producer = tokio::spawn(async move {
            let mut file = tokio::fs::File::open(&source)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, &source, "open"))?;
            let mut offset = 0_u64;
            let mut first = true;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0_u8; CHUNK];
            loop {
                let count = file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| cuenv_cas::Error::io(error, &source, "read"))?;
                if count == 0 {
                    break;
                }
                let next_offset = offset
                    .checked_add(count as u64)
                    .ok_or_else(|| cuenv_cas::Error::serialization("upload offset overflow"))?;
                let request = WriteRequest {
                    resource_name: if first {
                        resource_name.clone()
                    } else {
                        String::new()
                    },
                    write_offset: i64::try_from(offset).map_err(|_| {
                        cuenv_cas::Error::serialization("upload offset exceeds REAPI limit")
                    })?,
                    finish_write: next_offset == expected_size,
                    data: buffer[..count].to_vec(),
                };
                hasher.update(&buffer[..count]);
                sender.send(request).await.map_err(|_| {
                    cuenv_cas::Error::serialization("ByteStream upload closed early")
                })?;
                first = false;
                offset = next_offset;
            }
            if offset != expected_size {
                return Err(cuenv_cas::Error::serialization(format!(
                    "source changed while uploading: expected {expected_size} bytes, read {offset}"
                )));
            }
            let actual = Digest::try_new(hex::encode(hasher.finalize()), offset)?;
            if actual != expected_digest {
                return Err(cuenv_cas::Error::digest_mismatch(
                    expected_digest.to_resource(),
                    actual.to_resource(),
                ));
            }
            Ok(())
        });

        let request = self
            .client
            .request(tokio_stream::wrappers::ReceiverStream::new(receiver))
            .map_err(cuenv_cas::Error::from)?;
        let response = self
            .bytestream_client()
            .write(request)
            .await
            .map_err(|status| cuenv_cas::Error::from(Error::rpc("ByteStream.Write", &status)))?
            .into_inner();
        producer.await.map_err(|error| {
            cuenv_cas::Error::serialization(format!("upload producer failed: {error}"))
        })??;
        if response.committed_size == -1 {
            return Ok(());
        }
        let committed = u64::try_from(response.committed_size).unwrap_or(u64::MAX);
        if committed != expected_size {
            return Err(cuenv_cas::Error::serialization(format!(
                "ByteStream.Write committed {committed} of {expected_size} bytes"
            )));
        }
        Ok(())
    }

    async fn read_streamed_to_file(
        &self,
        digest: &Digest,
        destination: &Path,
    ) -> cuenv_cas::Result<()> {
        digest.validate()?;
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, parent, "create_dir_all"))?;
        }
        let use_zstd =
            self.capabilities.supports_bytestream_zstd() && digest.size_bytes >= MIN_COMPRESS_SIZE;
        let read_limit = if use_zstd {
            0
        } else {
            i64::try_from(digest.size_bytes).map_err(|_| {
                cuenv_cas::Error::serialization("download size exceeds the REAPI signed limit")
            })?
        };
        let compressor = if use_zstd {
            crate::client::Compressor::Zstd
        } else {
            crate::client::Compressor::Identity
        };
        let request = self
            .client
            .request(ReadRequest {
                resource_name: self.read_resource_name(digest, compressor),
                read_offset: 0,
                read_limit,
            })
            .map_err(cuenv_cas::Error::from)?;
        let mut stream = self
            .bytestream_client()
            .read(request)
            .await
            .map_err(|status| cuenv_cas::Error::from(Error::rpc("ByteStream.Read", &status)))?
            .into_inner();

        let result = async {
            if use_zstd {
                let mut file = std::fs::File::create(destination)
                    .map_err(|error| cuenv_cas::Error::io(error, destination, "create"))?;
                let mut writer = HashingBoundedWriter::new(&mut file, digest.size_bytes);
                let mut decoder = zstd::stream::write::Decoder::new(&mut writer).map_err(|e| {
                    cuenv_cas::Error::serialization(format!("init zstd decoder: {e}"))
                })?;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|status| {
                        cuenv_cas::Error::from(Error::rpc("ByteStream.Read", &status))
                    })?;
                    decoder.write_all(&chunk.data).map_err(|e| {
                        cuenv_cas::Error::serialization(format!("zstd decompress failed: {e}"))
                    })?;
                }
                decoder.flush().map_err(|e| {
                    cuenv_cas::Error::serialization(format!("zstd flush failed: {e}"))
                })?;
                drop(decoder);
                let (hash, size) = writer.finish();
                file.sync_all()
                    .map_err(|error| cuenv_cas::Error::io(error, destination, "fsync"))?;
                let actual = Digest::try_new(hash, size)?;
                if &actual != digest {
                    return Err(cuenv_cas::Error::digest_mismatch(
                        digest.to_resource(),
                        actual.to_resource(),
                    ));
                }
                return Ok(());
            }

            let mut file = tokio::fs::File::create(destination)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, destination, "create"))?;
            let mut hasher = Sha256::new();
            let mut size = 0_u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|status| {
                    cuenv_cas::Error::from(Error::rpc("ByteStream.Read", &status))
                })?;
                size = size
                    .checked_add(chunk.data.len() as u64)
                    .ok_or_else(|| cuenv_cas::Error::serialization("download size overflow"))?;
                if size > digest.size_bytes {
                    return Err(cuenv_cas::Error::serialization(format!(
                        "server sent more than the declared {} bytes",
                        digest.size_bytes
                    )));
                }
                hasher.update(&chunk.data);
                file.write_all(&chunk.data)
                    .await
                    .map_err(|error| cuenv_cas::Error::io(error, destination, "write"))?;
            }
            file.sync_all()
                .await
                .map_err(|error| cuenv_cas::Error::io(error, destination, "fsync"))?;
            let actual = Digest::try_new(hex::encode(hasher.finalize()), size)?;
            if &actual != digest {
                return Err(cuenv_cas::Error::digest_mismatch(
                    digest.to_resource(),
                    actual.to_resource(),
                ));
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(destination).await;
        }
        result
    }

    async fn put(&self, bytes: &[u8]) -> Result<Digest> {
        self.client.ensure_writable("upload a blob")?;
        let digest = Digest::of_bytes(bytes);

        // Skip the upload when the server already holds the blob. Content
        // addressing makes this safe and it is the common case in a warm
        // cache.
        if self
            .find_missing(std::slice::from_ref(&digest))
            .await?
            .is_empty()
        {
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

fn compress_zstd(bytes: &[u8]) -> Result<Vec<u8>> {
    zstd::bulk::compress(bytes, 3)
        .map_err(|e| Error::protocol("zstd.compress", format!("zstd compression failed: {e}")))
}

fn decode_compressed_blob(
    operation: &'static str,
    compressor: i32,
    data: &[u8],
    expected_uncompressed_size: u64,
) -> Result<Vec<u8>> {
    if compressor == pb::compressor::Value::Identity as i32 || compressor == 0 {
        return Ok(data.to_vec());
    }
    if compressor == pb::compressor::Value::Zstd as i32 {
        let initial_capacity = usize::try_from(expected_uncompressed_size)
            .unwrap_or(MAX_INITIAL_ALLOCATION)
            .min(MAX_INITIAL_ALLOCATION);
        let mut bounded = BoundedVecWriter::new(initial_capacity, expected_uncompressed_size);
        let mut decoder = zstd::stream::write::Decoder::new(&mut bounded)
            .map_err(|e| Error::protocol(operation, format!("init zstd decoder: {e}")))?;
        decoder
            .write_all(data)
            .map_err(|e| Error::protocol(operation, format!("zstd decompress failed: {e}")))?;
        decoder
            .flush()
            .map_err(|e| Error::protocol(operation, format!("zstd flush failed: {e}")))?;
        drop(decoder);
        return Ok(bounded.into_inner());
    }
    Err(Error::protocol(
        operation,
        format!("unsupported REAPI compressor value {compressor}"),
    ))
}

/// Sink that caps decompressed output at `limit` bytes to guard against
/// decompression bombs.
struct BoundedVecWriter {
    buf: Vec<u8>,
    limit: u64,
}

impl BoundedVecWriter {
    fn new(initial_capacity: usize, limit: u64) -> Self {
        Self {
            buf: Vec::with_capacity(initial_capacity),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

impl std::io::Write for BoundedVecWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let next = u64::try_from(self.buf.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("decompressed output exceeds declared {} bytes", self.limit),
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// File sink that simultaneously caps decompressed output and computes SHA-256.
struct HashingBoundedWriter<'a> {
    file: &'a mut std::fs::File,
    hasher: Sha256,
    written: u64,
    limit: u64,
}

impl<'a> HashingBoundedWriter<'a> {
    fn new(file: &'a mut std::fs::File, limit: u64) -> Self {
        Self {
            file,
            hasher: Sha256::new(),
            written: 0,
            limit,
        }
    }

    fn finish(self) -> (String, u64) {
        (hex::encode(self.hasher.finalize()), self.written)
    }
}

impl std::io::Write for HashingBoundedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let next = self.written.saturating_add(len);
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("decompressed output exceeds declared {} bytes", self.limit),
            ));
        }
        self.file.write_all(data)?;
        self.hasher.update(data);
        self.written = next;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
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
        if self.fits_in_a_batch(digest.size_bytes) {
            let bytes = self.get(digest).await?;
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| cuenv_cas::Error::io(error, parent, "create_dir_all"))?;
            }
            tokio::fs::write(destination, &bytes)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, destination, "write"))?;
            Ok(())
        } else {
            self.read_streamed_to_file(digest, destination).await
        }
    }

    async fn put_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        Ok(self.put(bytes).await?)
    }

    async fn put_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        self.client
            .ensure_writable("upload a file")
            .map_err(cuenv_cas::Error::from)?;
        let digest = Self::digest_file(source).await?;
        if self
            .find_missing(std::slice::from_ref(&digest))
            .await
            .map_err(cuenv_cas::Error::from)?
            .is_empty()
        {
            return Ok(digest);
        }
        if self.fits_in_a_batch(digest.size_bytes) {
            let bytes = tokio::fs::read(source)
                .await
                .map_err(|error| cuenv_cas::Error::io(error, source, "read"))?;
            let actual = Digest::of_bytes(&bytes);
            if actual != digest {
                return Err(cuenv_cas::Error::digest_mismatch(
                    digest.to_resource(),
                    actual.to_resource(),
                ));
            }
            self.write_batched(&digest, &bytes)
                .await
                .map_err(cuenv_cas::Error::from)?;
        } else {
            self.write_file_streamed(&digest, source).await?;
        }
        Ok(digest)
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
        let name = cas("").read_resource_name(&digest, crate::client::Compressor::Identity);
        assert_eq!(name, format!("blobs/{}/1", digest.hash));
        let compressed = cas("").read_resource_name(&digest, crate::client::Compressor::Zstd);
        assert_eq!(
            compressed,
            format!("compressed-blobs/zstd/{}/1", digest.hash)
        );
    }

    #[tokio::test]
    async fn read_resource_name_includes_the_instance() {
        let digest = Digest::of_bytes(b"x");
        let name =
            cas("my-instance").read_resource_name(&digest, crate::client::Compressor::Identity);
        assert_eq!(name, format!("my-instance/blobs/{}/1", digest.hash));
    }

    #[tokio::test]
    async fn write_resource_names_are_unique_per_attempt() {
        let store = cas("");
        let digest = Digest::of_bytes(b"x");
        assert_ne!(
            store.write_resource_name(&digest, crate::client::Compressor::Identity),
            store.write_resource_name(&digest, crate::client::Compressor::Identity)
        );
    }

    #[tokio::test]
    async fn write_resource_name_has_the_reapi_shape() {
        let digest = Digest::of_bytes(b"x");
        let name = cas("inst").write_resource_name(&digest, crate::client::Compressor::Identity);
        assert!(name.starts_with("inst/uploads/"), "{name}");
        assert!(
            name.ends_with(&format!("/blobs/{}/1", digest.hash)),
            "{name}"
        );
        let zstd_name = cas("inst").write_resource_name(&digest, crate::client::Compressor::Zstd);
        assert!(
            zstd_name.ends_with(&format!("/compressed-blobs/zstd/{}/1", digest.hash)),
            "{zstd_name}"
        );
    }

    #[tokio::test]
    async fn batch_threshold_follows_the_advertised_limit() {
        let store = cas("");
        assert!(store.fits_in_a_batch(1024));
        assert!(!store.fits_in_a_batch(u64::try_from(DEFAULT_MAX_BATCH_SIZE).unwrap() + 1));
    }

    #[test]
    fn zstd_roundtrip_and_bounded_decompression_guard() {
        let payload = b"cuenv remote cache payload ".repeat(512);
        let compressed = compress_zstd(&payload).unwrap();
        assert!(compressed.len() < payload.len());
        let decoded = decode_compressed_blob(
            "test",
            pb::compressor::Value::Zstd as i32,
            &compressed,
            payload.len() as u64,
        )
        .unwrap();
        assert_eq!(decoded, payload);

        // Declaring a smaller expected size must reject the oversized stream.
        let err =
            decode_compressed_blob("test", pb::compressor::Value::Zstd as i32, &compressed, 16)
                .expect_err("oversized decompressed payload must be rejected");
        assert!(err.to_string().contains("exceeds declared"));
    }
}
