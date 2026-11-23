//! Acceptance tests for post-run remote cache publication (`RemotePublisher`).
//!
//! Verifies:
//! 1. Client 1 publishes `Action`, `Command`, logs, output files, and nested
//!    `Tree` file blobs BEFORE `PUT /ac/<sha256>`, and Client 2 restores the
//!    nested output tree from an empty local cache with zero remote writes.
//! 2. Any CAS blob upload failure prevents `PUT /ac/<sha256>` from running.

use cuenv_cas::{
    Action, ActionCache, ActionPublisher, ActionResult, CanonicalMessage, Cas, Command, Digest,
    ExecutionMetadata, LocalActionCache, LocalCas, OutputDirectory, OutputFile, Platform,
    PublishActionRequest, build_output_tree, materialize_output_tree, missing_blobs,
};
use cuenv_cas_remote::{
    CorruptBlobs, HttpActionCache, HttpCas, HttpRemoteClient, LayeredActionCache, LayeredCas,
    RemoteConfig, RemoteProtocol, RemotePublisher,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
struct ServerState {
    cas: HashMap<String, Vec<u8>>,
    ac: HashMap<String, Vec<u8>>,
    write_log: Vec<String>,
    fail_cas_hash: Option<String>,
}

async fn start_recording_http_cache(
    fail_cas_hash: Option<String>,
) -> std::io::Result<(String, Arc<Mutex<ServerState>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(Mutex::new(ServerState {
        fail_cas_hash,
        ..Default::default()
    }));
    let server_state = Arc::clone(&state);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let state = Arc::clone(&server_state);
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0_u8; 4096];
                let header_end = loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };

                let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
                let mut lines = headers.lines();
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");

                let content_length: usize = lines
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())
                            .flatten()
                    })
                    .unwrap_or(0);

                let mut body = raw[header_end..].to_vec();
                while body.len() < content_length {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                body.truncate(content_length);

                let (status, resp_body) = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .handle_request(method, path, body);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                if method != "HEAD" {
                    let _ = stream.write_all(&resp_body).await;
                }
            });
        }
    });

    Ok((format!("http://{addr}"), state))
}

impl ServerState {
    fn handle_request(
        &mut self,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> (&'static str, Vec<u8>) {
        if let Some(hash) = path.strip_prefix("/cas/") {
            return match method {
                "HEAD" => {
                    if self.cas.contains_key(hash) {
                        ("200 OK", Vec::new())
                    } else {
                        ("404 Not Found", Vec::new())
                    }
                }
                "GET" => match self.cas.get(hash) {
                    Some(bytes) => ("200 OK", bytes.clone()),
                    None => ("404 Not Found", Vec::new()),
                },
                "PUT" => {
                    if self.fail_cas_hash.as_deref() == Some(hash) {
                        return ("500 Internal Server Error", b"injected failure".to_vec());
                    }
                    self.write_log.push(format!("PUT /cas/{hash}"));
                    self.cas.insert(hash.to_string(), body);
                    ("200 OK", Vec::new())
                }
                _ => ("405 Method Not Allowed", Vec::new()),
            };
        }

        if let Some(hash) = path.strip_prefix("/ac/") {
            return match method {
                "GET" => match self.ac.get(hash) {
                    Some(bytes) => ("200 OK", bytes.clone()),
                    None => ("404 Not Found", Vec::new()),
                },
                "PUT" => {
                    self.write_log.push(format!("PUT /ac/{hash}"));
                    self.ac.insert(hash.to_string(), body);
                    ("200 OK", Vec::new())
                }
                _ => ("405 Method Not Allowed", Vec::new()),
            };
        }

        ("404 Not Found", Vec::new())
    }
}

#[tokio::test]
async fn client_one_publishes_nested_outputs_and_client_two_restores_from_empty_local_cache() {
    let (endpoint, state) = start_recording_http_cache(None).await.unwrap();

    // --- Client 1: produce nested directory + file outputs and publish ---
    let client1_cache_dir = TempDir::new().unwrap();
    let client1_work = TempDir::new().unwrap();
    let client1_cas = LocalCas::open(client1_cache_dir.path()).unwrap();

    // Create a top-level file and a nested output directory tree.
    let top_file = client1_work.path().join("summary.txt");
    std::fs::write(&top_file, b"build ok\n").unwrap();
    let dist_dir = client1_work.path().join("dist");
    std::fs::create_dir_all(dist_dir.join("nested/deep")).unwrap();
    std::fs::write(dist_dir.join("app.js"), b"console.log('hi');").unwrap();
    std::fs::write(dist_dir.join("nested/deep/data.bin"), b"\x00\x01\x02\x03").unwrap();

    let summary_digest = client1_cas.put_file(&top_file).await.unwrap();
    let tree_digest = build_output_tree(&dist_dir, &client1_cas).await.unwrap();
    let stdout_digest = client1_cas.put_bytes(b"compiled\n").await.unwrap();
    let stderr_digest = client1_cas.put_bytes(b"").await.unwrap();

    let command = Command {
        arguments: vec!["sh".into(), "-c".into(), "build".into()],
        environment_variables: std::collections::BTreeMap::new(),
        output_files: vec!["summary.txt".into()],
        output_directories: vec!["dist".into()],
        working_directory: String::new(),
    };
    let command_digest = client1_cas
        .put_bytes(&command.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let action = Action {
        command_digest: command_digest.clone(),
        input_root_digest: Digest::of_bytes(b"inputs"),
        platform: Platform::default(),
        action_semantics_version: cuenv_cas::ACTION_SEMANTICS_VERSION,
    };
    let action_digest = client1_cas
        .put_bytes(&action.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let action_result = ActionResult {
        output_files: vec![OutputFile {
            path: "summary.txt".into(),
            digest: summary_digest.clone(),
            is_executable: false,
        }],
        output_directories: vec![OutputDirectory {
            path: "dist".into(),
            tree_digest: tree_digest.clone(),
        }],
        exit_code: 0,
        stdout_digest: Some(stdout_digest),
        stderr_digest: Some(stderr_digest),
        execution_metadata: ExecutionMetadata::default(),
    };

    let write_cfg = RemoteConfig::new(&endpoint)
        .with_protocol(RemoteProtocol::Http)
        .writable();
    let write_client = HttpRemoteClient::new(write_cfg).unwrap();
    let publisher = RemotePublisher::http(
        Arc::new(HttpCas::new(write_client.clone())),
        Arc::new(HttpActionCache::new(write_client)),
    );

    publisher
        .publish(PublishActionRequest {
            local_cas: &client1_cas,
            action: &action,
            action_digest: &action_digest,
            command: &command,
            command_digest: &command_digest,
            result: &action_result,
        })
        .await
        .unwrap();

    // Verify REAPI publication contract: Action, Command, logs, outputs, Tree,
    // and nested file leaves were ALL uploaded BEFORE PUT /ac/<action_hash>.
    let write_count_after_publish = {
        let guard = state.lock().unwrap();
        assert!(
            guard.cas.contains_key(&command_digest.hash),
            "Command must be uploaded"
        );
        assert!(
            guard.cas.contains_key(&action_digest.hash),
            "Action must be uploaded"
        );
        assert!(
            guard.cas.contains_key(&tree_digest.hash),
            "Tree must be uploaded"
        );
        assert!(
            guard
                .cas
                .contains_key(&Digest::of_bytes(b"\x00\x01\x02\x03").hash),
            "nested leaf blob must be uploaded"
        );
        assert_eq!(
            guard.write_log.last().map(String::as_str),
            Some(format!("PUT /ac/{}", action_digest.hash).as_str()),
            "ActionResult must be published strictly last"
        );
        guard.write_log.len()
    };

    // --- Client 2: restore from an empty local cache (read-only remote) ---
    let client2_cache_dir = TempDir::new().unwrap();
    let client2_restore = TempDir::new().unwrap();
    let read_cfg = RemoteConfig::new(&endpoint).with_protocol(RemoteProtocol::Http);
    let read_client = HttpRemoteClient::new(read_cfg).unwrap();
    let remote_cas: Arc<dyn Cas> = Arc::new(HttpCas::new(read_client.clone()));
    let remote_ac: Arc<dyn ActionCache> = Arc::new(HttpActionCache::new(read_client));

    let local2_cas: Arc<dyn Cas> = Arc::new(LocalCas::open(client2_cache_dir.path()).unwrap());
    let local2_ac: Arc<dyn ActionCache> =
        Arc::new(LocalActionCache::open(client2_cache_dir.path()).unwrap());

    let layered_cas = LayeredCas::new(local2_cas, remote_cas.clone());
    let layered_ac = LayeredActionCache::new(local2_ac, remote_ac).with_remote_cas(remote_cas);

    let restored_result = layered_ac
        .lookup(&action_digest)
        .await
        .unwrap()
        .expect("Client 2 must hit the published ActionResult");
    assert!(
        missing_blobs(&layered_cas, &restored_result)
            .await
            .unwrap()
            .is_empty(),
        "every referenced blob must be present on Client 2"
    );

    let restored_dist = client2_restore.path().join("dist");
    materialize_output_tree(&layered_cas, &tree_digest, &restored_dist)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(restored_dist.join("app.js")).unwrap(),
        b"console.log('hi');"
    );
    assert_eq!(
        std::fs::read(restored_dist.join("nested/deep/data.bin")).unwrap(),
        b"\x00\x01\x02\x03"
    );

    // Client 2 must have issued ZERO remote writes.
    assert_eq!(
        state.lock().unwrap().write_log.len(),
        write_count_after_publish,
        "Client 2 cache hit must not issue any remote writes"
    );
}

#[tokio::test]
async fn failed_blob_upload_prevents_action_result_publication() {
    let nested_leaf_bytes = b"leaf-that-fails-upload";
    let nested_leaf_digest = Digest::of_bytes(nested_leaf_bytes);
    let (endpoint, state) = start_recording_http_cache(Some(nested_leaf_digest.hash.clone()))
        .await
        .unwrap();

    let cache_dir = TempDir::new().unwrap();
    let work_dir = TempDir::new().unwrap();
    let local_cas = LocalCas::open(cache_dir.path()).unwrap();

    let dist_dir = work_dir.path().join("dist");
    std::fs::create_dir_all(&dist_dir).unwrap();
    std::fs::write(dist_dir.join("leaf.bin"), nested_leaf_bytes).unwrap();
    let tree_digest = build_output_tree(&dist_dir, &local_cas).await.unwrap();

    let command = Command {
        arguments: vec!["sh".into(), "-c".into(), "true".into()],
        environment_variables: std::collections::BTreeMap::new(),
        output_files: vec![],
        output_directories: vec!["dist".into()],
        working_directory: String::new(),
    };
    let command_digest = local_cas
        .put_bytes(&command.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let action = Action {
        command_digest: command_digest.clone(),
        input_root_digest: Digest::of_bytes(b"in"),
        platform: Platform::default(),
        action_semantics_version: cuenv_cas::ACTION_SEMANTICS_VERSION,
    };
    let action_digest = local_cas
        .put_bytes(&action.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let result = ActionResult {
        output_files: vec![],
        output_directories: vec![OutputDirectory {
            path: "dist".into(),
            tree_digest,
        }],
        exit_code: 0,
        stdout_digest: None,
        stderr_digest: None,
        execution_metadata: ExecutionMetadata::default(),
    };

    let write_cfg = RemoteConfig::new(&endpoint)
        .with_protocol(RemoteProtocol::Http)
        .writable();
    let write_client = HttpRemoteClient::new(write_cfg).unwrap();
    let publisher = RemotePublisher::http(
        Arc::new(HttpCas::new(write_client.clone())),
        Arc::new(HttpActionCache::new(write_client)),
    );

    let err = publisher
        .publish(PublishActionRequest {
            local_cas: &local_cas,
            action: &action,
            action_digest: &action_digest,
            command: &command,
            command_digest: &command_digest,
            result: &result,
        })
        .await
        .expect_err("failed leaf blob upload must abort publication");
    assert!(err.to_string().contains("500"), "unexpected error: {err}");
    assert!(
        state.lock().unwrap().ac.is_empty(),
        "ActionResult must never be published when a blob upload fails"
    );
}

/// Publish one action whose only output is `output`, against `endpoint`,
/// sharing `corrupt` with the publisher.
#[expect(
    clippy::unwrap_used,
    reason = "test harness setup; clippy's allow-unwrap-in-tests does not reach helper fns"
)]
async fn publish_single_output(
    endpoint: &str,
    output: &[u8],
    corrupt: Arc<CorruptBlobs>,
) -> (Digest, Digest, cuenv_cas::Result<()>) {
    let cache_dir = TempDir::new().unwrap();
    let local_cas = LocalCas::open(cache_dir.path()).unwrap();
    let output_digest = local_cas.put_bytes(output).await.unwrap();
    let command = Command {
        arguments: vec!["sh".into(), "-c".into(), "true".into()],
        environment_variables: std::collections::BTreeMap::new(),
        output_files: vec!["out.txt".into()],
        output_directories: vec![],
        working_directory: String::new(),
    };
    let command_digest = local_cas
        .put_bytes(&command.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let action = Action {
        command_digest: command_digest.clone(),
        input_root_digest: Digest::of_bytes(b"in"),
        platform: Platform::default(),
        action_semantics_version: cuenv_cas::ACTION_SEMANTICS_VERSION,
    };
    let action_digest = local_cas
        .put_bytes(&action.to_canonical_bytes().unwrap())
        .await
        .unwrap();
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "out.txt".into(),
            digest: output_digest.clone(),
            is_executable: false,
        }],
        exit_code: 0,
        ..ActionResult::default()
    };

    let write_client = HttpRemoteClient::new(
        RemoteConfig::new(endpoint)
            .with_protocol(RemoteProtocol::Http)
            .writable(),
    )
    .unwrap();
    let publisher = RemotePublisher::http(
        Arc::new(HttpCas::new(write_client.clone())),
        Arc::new(HttpActionCache::new(write_client)),
    )
    .with_corrupt_blobs(corrupt);
    let outcome = publisher
        .publish(PublishActionRequest {
            local_cas: &local_cas,
            action: &action,
            action_digest: &action_digest,
            command: &command,
            command_digest: &command_digest,
            result: &result,
        })
        .await;
    (output_digest, action_digest, outcome)
}

#[tokio::test]
async fn a_blob_recorded_corrupt_is_replaced_before_the_result_is_published() {
    let (endpoint, state) = start_recording_http_cache(None).await.unwrap();
    let output = b"task output";
    let output_digest = Digest::of_bytes(output);
    // The server reports the blob present, but holds the wrong bytes.
    state
        .lock()
        .unwrap()
        .cas
        .insert(output_digest.hash.clone(), b"task outpux".to_vec());
    let corrupt = Arc::new(CorruptBlobs::new());
    corrupt.insert(output_digest.clone());

    let (_, action_digest, outcome) =
        publish_single_output(&endpoint, output, Arc::clone(&corrupt)).await;

    outcome.unwrap();
    let guard = state.lock().unwrap();
    assert_eq!(
        guard.cas.get(&output_digest.hash).map(Vec::as_slice),
        Some(output.as_slice()),
        "the corrupt blob is overwritten even though the server reported it present"
    );
    assert!(guard.ac.contains_key(&action_digest.hash));
    assert!(
        !corrupt.contains(&output_digest),
        "a verified repair clears the record"
    );
}

#[tokio::test]
async fn without_a_record_a_present_corrupt_blob_is_not_re_uploaded() {
    let (endpoint, state) = start_recording_http_cache(None).await.unwrap();
    let output = b"task output";
    let output_digest = Digest::of_bytes(output);
    state
        .lock()
        .unwrap()
        .cas
        .insert(output_digest.hash.clone(), b"task outpux".to_vec());

    let (_, _, outcome) =
        publish_single_output(&endpoint, output, Arc::new(CorruptBlobs::new())).await;

    outcome.unwrap();
    assert_eq!(
        state
            .lock()
            .unwrap()
            .cas
            .get(&output_digest.hash)
            .map(Vec::as_slice),
        Some(b"task outpux".as_slice()),
        "an index-only presence check cannot see rotten bytes; only the record can"
    );
}
