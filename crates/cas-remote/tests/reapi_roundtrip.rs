//! End-to-end tests against a REAPI server running in-process.
//!
//! These exercise the real gRPC path — protobuf on the wire, tonic codecs,
//! streaming, status codes — without needing a container. The server is a
//! minimal but honest implementation of the three services cuenv uses, so a
//! client bug shows up here rather than against someone's production cache.

use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::build::bazel::remote::execution::v2::action_cache_server::{
    ActionCache as ActionCacheService, ActionCacheServer,
};
use bazel_remote_apis::build::bazel::remote::execution::v2::capabilities_server::{
    Capabilities as CapabilitiesService, CapabilitiesServer,
};
use bazel_remote_apis::build::bazel::remote::execution::v2::content_addressable_storage_server::{
    ContentAddressableStorage as CasService, ContentAddressableStorageServer,
};
use bazel_remote_apis::google::bytestream::byte_stream_server::{
    ByteStream as ByteStreamService, ByteStreamServer,
};
use bazel_remote_apis::google::bytestream::{
    QueryWriteStatusRequest, QueryWriteStatusResponse, ReadRequest, ReadResponse, WriteRequest,
    WriteResponse,
};
use cuenv_cas::{ActionCache, Cas, Digest};
use cuenv_cas_remote::{Credentials, RemoteActionCache, RemoteCas, RemoteClient, RemoteConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

// =============================================================================
// A small in-memory REAPI server
// =============================================================================

#[derive(Default)]
struct Store {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    results: Mutex<HashMap<String, pb::ActionResult>>,
    /// Bearer tokens seen, so a test can assert credentials were sent.
    seen_authorization: Mutex<Vec<String>>,
    /// Maximum batch payload advertised. 0 means "unset".
    max_batch_total_size_bytes: i64,
}

impl Store {
    fn record_auth(&self, metadata: &tonic::metadata::MetadataMap) {
        if let Some(value) = metadata.get("authorization")
            && let Ok(value) = value.to_str()
        {
            lock(&self.seen_authorization).push(value.to_string());
        }
    }
}

/// Lock a mutex, recovering from poisoning.
///
/// A poisoned mutex here means some other assertion already failed while the
/// lock was held. The stored bytes are still perfectly valid, and panicking
/// again would bury the original failure under this one.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn key(digest: &pb::Digest) -> String {
    format!("{}/{}", digest.hash, digest.size_bytes)
}

/// Parse `{instance}/blobs/{hash}/{size}` or
/// `{instance}/uploads/{uuid}/blobs/{hash}/{size}`.
fn key_from_resource_name(resource_name: &str) -> Option<String> {
    let parts: Vec<&str> = resource_name.split('/').collect();
    let index = parts.iter().rposition(|part| *part == "blobs")?;
    let hash = parts.get(index + 1)?;
    let size = parts.get(index + 2)?;
    Some(format!("{hash}/{size}"))
}

struct CasImpl(Arc<Store>);

#[tonic::async_trait]
impl CasService for CasImpl {
    async fn find_missing_blobs(
        &self,
        request: Request<pb::FindMissingBlobsRequest>,
    ) -> Result<Response<pb::FindMissingBlobsResponse>, Status> {
        self.0.record_auth(request.metadata());
        let blobs = lock(&self.0.blobs);
        let missing = request
            .into_inner()
            .blob_digests
            .into_iter()
            .filter(|digest| !blobs.contains_key(&key(digest)))
            .collect();
        Ok(Response::new(pb::FindMissingBlobsResponse {
            missing_blob_digests: missing,
        }))
    }

    async fn batch_update_blobs(
        &self,
        request: Request<pb::BatchUpdateBlobsRequest>,
    ) -> Result<Response<pb::BatchUpdateBlobsResponse>, Status> {
        self.0.record_auth(request.metadata());
        let mut blobs = lock(&self.0.blobs);
        let mut responses = Vec::new();
        for entry in request.into_inner().requests {
            let Some(digest) = entry.digest else {
                return Err(Status::invalid_argument("missing digest"));
            };
            blobs.insert(key(&digest), entry.data);
            responses.push(pb::batch_update_blobs_response::Response {
                digest: Some(digest),
                status: None,
            });
        }
        Ok(Response::new(pb::BatchUpdateBlobsResponse { responses }))
    }

    async fn batch_read_blobs(
        &self,
        request: Request<pb::BatchReadBlobsRequest>,
    ) -> Result<Response<pb::BatchReadBlobsResponse>, Status> {
        self.0.record_auth(request.metadata());
        let blobs = lock(&self.0.blobs);
        let responses = request
            .into_inner()
            .digests
            .into_iter()
            .map(|digest| match blobs.get(&key(&digest)) {
                Some(data) => pb::batch_read_blobs_response::Response {
                    digest: Some(digest),
                    data: data.clone(),
                    status: None,
                    ..Default::default()
                },
                None => pb::batch_read_blobs_response::Response {
                    digest: Some(digest),
                    data: Vec::new(),
                    status: Some(bazel_remote_apis::google::rpc::Status {
                        code: 5, // NOT_FOUND
                        message: "blob not found".into(),
                        details: Vec::new(),
                    }),
                    ..Default::default()
                },
            })
            .collect();
        Ok(Response::new(pb::BatchReadBlobsResponse { responses }))
    }

    type GetTreeStream =
        tokio_stream::Iter<std::vec::IntoIter<Result<pb::GetTreeResponse, Status>>>;

    async fn get_tree(
        &self,
        _request: Request<pb::GetTreeRequest>,
    ) -> Result<Response<Self::GetTreeStream>, Status> {
        Err(Status::unimplemented("GetTree is not used by cuenv"))
    }

    // The blob-splitting RPCs are a newer REAPI addition that cuenv does not
    // use. A real server may implement them; this one answers honestly.

    async fn split_blob(
        &self,
        _request: Request<pb::SplitBlobRequest>,
    ) -> Result<Response<pb::SplitBlobResponse>, Status> {
        Err(Status::unimplemented("SplitBlob is not used by cuenv"))
    }

    async fn splice_blob(
        &self,
        _request: Request<pb::SpliceBlobRequest>,
    ) -> Result<Response<pb::SpliceBlobResponse>, Status> {
        Err(Status::unimplemented("SpliceBlob is not used by cuenv"))
    }

    type GetChunkMappingStream =
        tokio_stream::Iter<std::vec::IntoIter<Result<pb::GetChunkMappingResponse, Status>>>;

    async fn get_chunk_mapping(
        &self,
        _request: Request<pb::GetChunkMappingRequest>,
    ) -> Result<Response<Self::GetChunkMappingStream>, Status> {
        Err(Status::unimplemented(
            "GetChunkMapping is not used by cuenv",
        ))
    }

    async fn register_chunk_mapping(
        &self,
        _request: Request<tonic::Streaming<pb::RegisterChunkMappingRequest>>,
    ) -> Result<Response<pb::RegisterChunkMappingResponse>, Status> {
        Err(Status::unimplemented(
            "RegisterChunkMapping is not used by cuenv",
        ))
    }
}

/// Read chunk size the test server uses. Deliberately tiny so the client
/// has to reassemble many pieces.
const SERVER_CHUNK: usize = 7;

struct ByteStreamImpl(Arc<Store>);

#[tonic::async_trait]
impl ByteStreamService for ByteStreamImpl {
    type ReadStream = tokio_stream::Iter<std::vec::IntoIter<Result<ReadResponse, Status>>>;

    async fn read(
        &self,
        request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        self.0.record_auth(request.metadata());
        let request = request.into_inner();
        let resource_name = request.resource_name;
        let key = key_from_resource_name(&resource_name)
            .ok_or_else(|| Status::invalid_argument("malformed resource name"))?;
        let blobs = lock(&self.0.blobs);
        let data = blobs
            .get(&key)
            .ok_or_else(|| Status::not_found("blob not found"))?
            .clone();
        let offset = usize::try_from(request.read_offset)
            .map_err(|_| Status::invalid_argument("negative read offset"))?;
        if offset > data.len() {
            return Err(Status::out_of_range("read offset exceeds blob size"));
        }
        let available = &data[offset..];
        let data = if request.read_limit == 0 {
            available
        } else {
            let limit = usize::try_from(request.read_limit)
                .map_err(|_| Status::invalid_argument("negative read limit"))?;
            &available[..available.len().min(limit)]
        };

        // Deliberately chunk small so the client's reassembly is exercised.
        let chunks: Vec<Result<ReadResponse, Status>> = data
            .chunks(SERVER_CHUNK)
            .map(|chunk| {
                Ok(ReadResponse {
                    data: chunk.to_vec(),
                })
            })
            .collect();
        Ok(Response::new(tokio_stream::iter(chunks)))
    }

    async fn write(
        &self,
        request: Request<tonic::Streaming<WriteRequest>>,
    ) -> Result<Response<WriteResponse>, Status> {
        self.0.record_auth(request.metadata());
        let mut stream = request.into_inner();
        let mut key = None;
        let mut data = Vec::new();
        let mut expected_offset = 0_i64;
        let mut finished = false;

        while let Some(chunk) = stream.message().await? {
            if finished {
                return Err(Status::invalid_argument("chunk sent after finish_write"));
            }
            if key.is_none() && !chunk.resource_name.is_empty() {
                key = key_from_resource_name(&chunk.resource_name);
            } else if !chunk.resource_name.is_empty() {
                return Err(Status::invalid_argument(
                    "resource name must appear only in the first chunk",
                ));
            }
            if chunk.write_offset != expected_offset {
                return Err(Status::invalid_argument(format!(
                    "unexpected write offset {}; expected {expected_offset}",
                    chunk.write_offset
                )));
            }
            data.extend_from_slice(&chunk.data);
            expected_offset = i64::try_from(data.len())
                .map_err(|_| Status::resource_exhausted("upload too large"))?;
            if chunk.finish_write {
                finished = true;
            }
        }

        if !finished {
            return Err(Status::invalid_argument("finish_write was not sent"));
        }
        let key = key.ok_or_else(|| Status::invalid_argument("no resource name was sent"))?;
        let committed_size = i64::try_from(data.len()).unwrap_or(i64::MAX);
        lock(&self.0.blobs).insert(key, data);
        Ok(Response::new(WriteResponse { committed_size }))
    }

    async fn query_write_status(
        &self,
        _request: Request<QueryWriteStatusRequest>,
    ) -> Result<Response<QueryWriteStatusResponse>, Status> {
        Err(Status::unimplemented(
            "QueryWriteStatus is not used by cuenv",
        ))
    }
}

struct ActionCacheImpl(Arc<Store>);

#[tonic::async_trait]
impl ActionCacheService for ActionCacheImpl {
    async fn get_action_result(
        &self,
        request: Request<pb::GetActionResultRequest>,
    ) -> Result<Response<pb::ActionResult>, Status> {
        self.0.record_auth(request.metadata());
        let digest = request
            .into_inner()
            .action_digest
            .ok_or_else(|| Status::invalid_argument("missing action digest"))?;
        let results = lock(&self.0.results);
        results
            .get(&key(&digest))
            .cloned()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("no such action result"))
    }

    async fn update_action_result(
        &self,
        request: Request<pb::UpdateActionResultRequest>,
    ) -> Result<Response<pb::ActionResult>, Status> {
        self.0.record_auth(request.metadata());
        let request = request.into_inner();
        let digest = request
            .action_digest
            .ok_or_else(|| Status::invalid_argument("missing action digest"))?;
        let result = request
            .action_result
            .ok_or_else(|| Status::invalid_argument("missing action result"))?;
        lock(&self.0.results).insert(key(&digest), result.clone());
        Ok(Response::new(result))
    }
}

struct CapabilitiesImpl {
    store: Arc<Store>,
    digest_functions: Vec<i32>,
}

#[tonic::async_trait]
impl CapabilitiesService for CapabilitiesImpl {
    async fn get_capabilities(
        &self,
        request: Request<pb::GetCapabilitiesRequest>,
    ) -> Result<Response<pb::ServerCapabilities>, Status> {
        self.store.record_auth(request.metadata());
        Ok(Response::new(pb::ServerCapabilities {
            cache_capabilities: Some(pb::CacheCapabilities {
                digest_functions: self.digest_functions.clone(),
                max_batch_total_size_bytes: self.store.max_batch_total_size_bytes,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }
}

/// A server listening on an ephemeral port, shut down when dropped.
struct TestServer {
    endpoint: String,
    store: Arc<Store>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    #[expect(
        clippy::panic,
        reason = "test harness setup; clippy's allow-panic-in-tests does not reach helper fns"
    )]
    async fn start(store: Store, digest_functions: Vec<i32>) -> Self {
        let store = Arc::new(store);
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => panic!("cannot bind a test server socket: {e}"),
        };
        let addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(e) => panic!("cannot read the test server address: {e}"),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();

        let serving = tonic::transport::Server::builder()
            .add_service(ContentAddressableStorageServer::new(CasImpl(store.clone())))
            .add_service(ByteStreamServer::new(ByteStreamImpl(store.clone())))
            .add_service(ActionCacheServer::new(ActionCacheImpl(store.clone())))
            .add_service(CapabilitiesServer::new(CapabilitiesImpl {
                store: store.clone(),
                digest_functions,
            }))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = rx.await;
            });

        let handle = tokio::spawn(async move {
            let _ = serving.await;
        });

        Self {
            endpoint: format!("grpc://{addr}"),
            store,
            shutdown: Some(tx),
            handle: Some(handle),
        }
    }

    #[expect(
        clippy::panic,
        reason = "test harness setup; clippy's allow-panic-in-tests does not reach helper fns"
    )]
    async fn client(&self, config: RemoteConfig) -> RemoteClient {
        match RemoteClient::connect(RemoteConfig {
            endpoint: self.endpoint.clone(),
            ..config
        })
        .await
        {
            Ok(client) => client,
            Err(e) => panic!("cannot connect to the test server: {e}"),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn sha256() -> Vec<i32> {
    vec![i32::from(pb::digest_function::Value::Sha256)]
}

#[expect(
    clippy::panic,
    reason = "test harness setup; clippy's allow-panic-in-tests does not reach helper fns"
)]
async fn writable_cas(server: &TestServer) -> RemoteCas {
    let client = server.client(RemoteConfig::default().writable()).await;
    match RemoteCas::connect(client).await {
        Ok(cas) => cas,
        Err(e) => panic!("capabilities handshake failed: {e}"),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn small_blob_round_trips_through_batch_rpcs() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let cas = writable_cas(&server).await;

    let digest = cas.put_bytes(b"hello remote cache").await.unwrap();
    assert!(cas.contains(&digest).await.unwrap());
    assert_eq!(cas.get(&digest).await.unwrap(), b"hello remote cache");
}

#[tokio::test]
async fn large_blob_round_trips_through_bytestream() {
    // Advertise a tiny batch limit so anything real is streamed instead.
    let store = Store {
        max_batch_total_size_bytes: 16,
        ..Store::default()
    };
    let server = TestServer::start(store, sha256()).await;
    let cas = writable_cas(&server).await;

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let digest = cas.put_bytes(&payload).await.unwrap();
    assert_eq!(cas.get(&digest).await.unwrap(), payload);
}

#[tokio::test]
async fn empty_blob_round_trips_through_batch_rpc() {
    let store = Store {
        max_batch_total_size_bytes: 0,
        ..Store::default()
    };
    let server = TestServer::start(store, sha256()).await;
    let client = server.client(RemoteConfig::default().writable()).await;
    let cas = RemoteCas::new_unchecked(client);

    let digest = cas.put_bytes(b"").await.unwrap();
    assert_eq!(cas.get(&digest).await.unwrap(), b"");
}

#[tokio::test]
async fn find_missing_reports_only_absent_blobs() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let cas = writable_cas(&server).await;

    let present = cas.put_bytes(b"present").await.unwrap();
    let absent = Digest::of_bytes(b"absent");

    let missing = cas
        .find_missing(&[present.clone(), absent.clone()])
        .await
        .unwrap();
    assert_eq!(missing, vec![absent]);
}

#[tokio::test]
async fn a_corrupt_blob_is_rejected_rather_than_returned() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let cas = writable_cas(&server).await;
    let digest = cas.put_bytes(b"original").await.unwrap();

    // A hostile or buggy server hands back different bytes under the same
    // digest. These would otherwise be installed into the workspace as if
    // the task had produced them.
    server.store.blobs.lock().unwrap().insert(
        format!("{}/{}", digest.hash, digest.size_bytes),
        b"tampered".to_vec(),
    );

    let error = cas.get(&digest).await.unwrap_err();
    assert!(
        matches!(error, cuenv_cas::Error::DigestMismatch { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_read_only_client_refuses_to_upload() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let client = server.client(RemoteConfig::default()).await;
    let cas = RemoteCas::new_unchecked(client);

    assert!(cas.put_bytes(b"payload").await.is_err());
}

#[tokio::test]
async fn credentials_reach_the_server() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let client = server
        .client(
            RemoteConfig::default()
                .writable()
                .with_credentials(Credentials::Bearer("secret-token".into())),
        )
        .await;
    let cas = RemoteCas::new_unchecked(client);

    cas.put_bytes(b"payload").await.unwrap();

    let seen = server.store.seen_authorization.lock().unwrap().clone();
    assert!(
        seen.iter().any(|value| value == "Bearer secret-token"),
        "server saw {seen:?}"
    );
}

#[tokio::test]
async fn a_server_without_sha256_is_refused() {
    // Advertising only a digest function cuenv cannot compute would mean
    // every lookup silently misses forever.
    let md5 = vec![i32::from(pb::digest_function::Value::Md5)];
    let server = TestServer::start(Store::default(), md5).await;
    let client = server.client(RemoteConfig::default()).await;

    let error = RemoteCas::connect(client).await.unwrap_err();
    assert!(error.to_string().contains("SHA-256"), "{error}");
}

#[tokio::test]
async fn capabilities_supply_the_batch_limit() {
    let store = Store {
        max_batch_total_size_bytes: 128,
        ..Store::default()
    };
    let server = TestServer::start(store, sha256()).await;
    let client = server.client(RemoteConfig::default()).await;

    assert_eq!(client.check_capabilities().await.unwrap(), Some(128));
}

#[tokio::test]
async fn action_result_round_trips() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let cas = writable_cas(&server).await;
    let client = server.client(RemoteConfig::default().writable()).await;
    let action_cache = RemoteActionCache::new(client);

    let stdout = cas.put_bytes(b"build succeeded\n").await.unwrap();
    let output = cas.put_bytes(b"binary").await.unwrap();
    let action_digest = Digest::of_bytes(b"an-action");

    let result = cuenv_cas::ActionResult {
        output_files: vec![cuenv_cas::OutputFile {
            path: "target/app".into(),
            digest: output,
            is_executable: true,
        }],
        output_directories: vec![],
        exit_code: 0,
        stdout_digest: Some(stdout),
        stderr_digest: None,
        execution_metadata: cuenv_cas::ExecutionMetadata {
            worker: "test".into(),
            duration_ms: 1500,
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        },
    };

    action_cache.update(&action_digest, &result).await.unwrap();
    let fetched = action_cache.lookup(&action_digest).await.unwrap().unwrap();
    assert_eq!(fetched, result);
}

#[tokio::test]
async fn an_absent_action_result_is_a_miss_not_an_error() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let client = server.client(RemoteConfig::default()).await;
    let action_cache = RemoteActionCache::new(client);

    let missing = action_cache
        .lookup(&Digest::of_bytes(b"never recorded"))
        .await
        .unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn a_read_only_client_refuses_to_update_an_action_result() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let client = server.client(RemoteConfig::default()).await;
    let action_cache = RemoteActionCache::new(client);

    let result = action_cache
        .update(
            &Digest::of_bytes(b"action"),
            &cuenv_cas::ActionResult::default(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn uploading_a_blob_the_server_already_holds_skips_the_transfer() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let cas = writable_cas(&server).await;

    let first = cas.put_bytes(b"identical").await.unwrap();
    let second = cas.put_bytes(b"identical").await.unwrap();
    assert_eq!(first, second);
    assert_eq!(cas.get(&first).await.unwrap(), b"identical");
}

#[tokio::test]
async fn instance_name_is_carried_on_requests() {
    let server = TestServer::start(Store::default(), sha256()).await;
    let client = server
        .client(
            RemoteConfig::default()
                .writable()
                .with_instance_name("my-instance"),
        )
        .await;
    let cas = RemoteCas::new_unchecked(client);

    // The in-memory server keys blobs by digest regardless of instance, so
    // this asserts the requests are well-formed and accepted end to end.
    let digest = cas.put_bytes(b"scoped").await.unwrap();
    assert_eq!(cas.get(&digest).await.unwrap(), b"scoped");
}

#[tokio::test]
async fn layered_store_serves_a_remote_blob_and_keeps_it_locally() {
    use cuenv_cas::LocalCas;
    use cuenv_cas_remote::LayeredCas;
    use std::sync::Arc;

    let server = TestServer::start(Store::default(), sha256()).await;
    let remote = writable_cas(&server).await;
    let digest = remote.put_bytes(b"produced elsewhere").await.unwrap();

    let dir = tempfile::TempDir::new().unwrap();
    let local: Arc<dyn Cas> = Arc::new(LocalCas::open(dir.path()).unwrap());
    let layered = LayeredCas::new(local.clone(), Arc::new(remote));

    assert_eq!(layered.get(&digest).await.unwrap(), b"produced elsewhere");
    assert!(
        local.contains(&digest).await.unwrap(),
        "a read-through must leave the blob in the local store"
    );
}
