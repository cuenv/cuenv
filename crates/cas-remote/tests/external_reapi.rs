//! Interoperability smoke test for a real REAPI server.
//!
//! Run with:
//! `CUENV_REAPI_TEST_ENDPOINT=grpc://127.0.0.1:9092 cargo test -p cuenv-cas-remote --test external_reapi -- --ignored`

use cuenv_cas::{ActionCache, ActionResult, Cas, Digest, ExecutionMetadata};
use cuenv_cas_remote::{RemoteActionCache, RemoteCas, RemoteClient, RemoteConfig};
use tempfile::TempDir;

#[tokio::test]
#[ignore = "requires CUENV_REAPI_TEST_ENDPOINT pointing at a real REAPI cache"]
async fn real_server_roundtrip() {
    let endpoint = std::env::var("CUENV_REAPI_TEST_ENDPOINT")
        .expect("CUENV_REAPI_TEST_ENDPOINT must name the test server");
    let client = RemoteClient::connect(RemoteConfig::new(endpoint).writable())
        .await
        .expect("connect to REAPI server");
    let cas = RemoteCas::connect(client.clone())
        .await
        .expect("negotiate REAPI capabilities");
    let action_cache = RemoteActionCache::new(client);

    let small = b"real-server-small";
    let small_digest = cas.put_bytes(small).await.expect("upload small blob");
    assert_eq!(cas.get(&small_digest).await.unwrap(), small);

    let temp = TempDir::new().unwrap();
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("downloaded/large.bin");
    let large = vec![0x5a; 5 * 1024 * 1024];
    tokio::fs::write(&source, &large).await.unwrap();
    let large_digest = cas.put_file(&source).await.expect("stream large blob");
    cas.get_to_file(&large_digest, &destination)
        .await
        .expect("stream large blob back");
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), large);

    let action_digest = Digest::of_bytes(b"cuenv-external-reapi-action");
    let result = ActionResult {
        exit_code: 0,
        stdout_digest: Some(small_digest),
        execution_metadata: ExecutionMetadata {
            worker: "external-reapi-test".to_string(),
            duration_ms: 1,
            created_at: chrono::Utc::now(),
        },
        ..ActionResult::default()
    };
    action_cache
        .update(&action_digest, &result)
        .await
        .expect("update action cache");
    assert_eq!(
        action_cache.lookup(&action_digest).await.unwrap(),
        Some(result)
    );
}
