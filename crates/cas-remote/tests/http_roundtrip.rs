//! End-to-end integration tests for Bazel HTTP/REST remote caching
//! (`HttpRemoteClient`, `HttpCas`, `HttpActionCache`) against an in-process
//! HTTP/1.1 server.

use cuenv_cas::{ActionCache, ActionResult, CanonicalMessage, Cas, Digest, OutputFile};
use cuenv_cas_remote::{
    Credentials, HttpActionCache, HttpCas, HttpRemoteClient, LayeredActionCache, LayeredCas,
    RemoteConfig, RemoteProtocol,
};
use prost::Message as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default)]
struct MockState {
    cas: HashMap<String, Vec<u8>>,
    ac: HashMap<String, Vec<u8>>,
    required_auth: Option<(String, String)>,
    redirect_to: Option<String>,
    put_redirects: HashMap<String, PutRedirect>,
    corrupt_cas: bool,
    oversized_ac: bool,
    seen_paths: Vec<String>,
}

struct PutRedirect {
    status: &'static str,
    location: String,
    rewrite: Option<(PathBuf, Vec<u8>)>,
}

async fn spawn_mock_http_server(state: Arc<Mutex<MockState>>) -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                handle_connection(stream, state).await;
            });
        }
    });

    Ok(format!("http://{addr}"))
}

async fn handle_connection(mut stream: tokio::net::TcpStream, state: Arc<Mutex<MockState>>) {
    let (reader, mut writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);

    let mut request_line = String::new();
    if buf_reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
        return;
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    let mut headers = HashMap::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if buf_reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_length = val.parse().unwrap_or(0);
            }
            headers.insert(key, val);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 && buf_reader.read_exact(&mut body).await.is_err() {
        return;
    }

    let (status, extra_headers, resp_body) = {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.seen_paths.push(format!("{method} {path}"));

        if let Some(location) = guard.redirect_to.clone() {
            ("302 Found", format!("Location: {location}\r\n"), Vec::new())
        } else if let Some((ref name, ref expected)) = guard.required_auth
            && headers.get(&name.to_ascii_lowercase()) != Some(expected)
        {
            ("401 Unauthorized", String::new(), b"unauthorized".to_vec())
        } else if method == "PUT"
            && let Some(redirect) = guard.put_redirects.get(&path)
        {
            if let Some((source, bytes)) = &redirect.rewrite
                && std::fs::write(source, bytes).is_err()
            {
                return;
            }
            (
                redirect.status,
                format!("Location: {}\r\n", redirect.location),
                Vec::new(),
            )
        } else if let Some((_, hash)) = path.rsplit_once("/cas/") {
            match method.as_str() {
                "HEAD" => {
                    if guard.cas.contains_key(hash) {
                        ("200 OK", String::new(), Vec::new())
                    } else {
                        ("404 Not Found", String::new(), Vec::new())
                    }
                }
                "GET" => match guard.cas.get(hash) {
                    Some(bytes) => {
                        let mut data = bytes.clone();
                        if guard.corrupt_cas && !data.is_empty() {
                            data[0] ^= 0xff;
                        }
                        ("200 OK", String::new(), data)
                    }
                    None => ("404 Not Found", String::new(), Vec::new()),
                },
                "PUT" => {
                    guard.cas.insert(hash.to_string(), body);
                    ("200 OK", String::new(), Vec::new())
                }
                _ => ("405 Method Not Allowed", String::new(), Vec::new()),
            }
        } else if let Some((_, hash)) = path.rsplit_once("/ac/") {
            match method.as_str() {
                "GET" => {
                    if guard.oversized_ac {
                        // Advertise a body larger than MAX_ACTION_RESULT_BYTES (16 MiB).
                        let huge = 20 * 1024 * 1024;
                        (
                            "200 OK",
                            format!("Content-Length: {huge}\r\nX-Raw-Oversized: 1\r\n"),
                            Vec::new(),
                        )
                    } else {
                        match guard.ac.get(hash) {
                            Some(bytes) => ("200 OK", String::new(), bytes.clone()),
                            None => ("404 Not Found", String::new(), Vec::new()),
                        }
                    }
                }
                "PUT" => {
                    guard.ac.insert(hash.to_string(), body);
                    ("200 OK", String::new(), Vec::new())
                }
                _ => ("405 Method Not Allowed", String::new(), Vec::new()),
            }
        } else {
            ("404 Not Found", String::new(), Vec::new())
        }
    };

    let head = if extra_headers.contains("X-Raw-Oversized: 1") {
        format!("HTTP/1.1 {status}\r\n{extra_headers}Connection: close\r\n\r\n")
    } else {
        format!(
            "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            resp_body.len()
        )
    };
    let _ = writer.write_all(head.as_bytes()).await;
    if method != "HEAD" && !resp_body.is_empty() {
        let _ = writer.write_all(&resp_body).await;
    }
}

#[tokio::test]
async fn http_read_through_cas_and_ac_roundtrip_with_prefix_and_instance() {
    let state = Arc::new(Mutex::new(MockState {
        required_auth: Some(("authorization".into(), "Bearer test-token".into())),
        ..Default::default()
    }));

    let payload = b"compiled artifact bytes";
    let blob_digest = Digest::of_bytes(payload);
    let action_digest = Digest::of_bytes(b"action-key");
    let action_result = ActionResult {
        exit_code: 0,
        output_files: vec![OutputFile {
            path: "out.bin".into(),
            digest: blob_digest.clone(),
            is_executable: true,
        }],
        ..Default::default()
    };
    let encoded_ar = action_result.to_proto().expect("proto").encode_to_vec();
    {
        let mut guard = state.lock().expect("lock");
        guard.cas.insert(blob_digest.hash.clone(), payload.to_vec());
        guard.ac.insert(action_digest.hash.clone(), encoded_ar);
    }

    let base = spawn_mock_http_server(Arc::clone(&state)).await.unwrap();
    let config = RemoteConfig::new(format!("{base}/cache-prefix"))
        .with_protocol(RemoteProtocol::Http)
        .with_instance_name("team-a")
        .with_credentials(Credentials::Bearer("test-token".into()));
    let client = HttpRemoteClient::new(config).expect("client");

    client.check_health().await.expect("health check passes");

    let cas = HttpCas::new(client.clone());
    let ac = HttpActionCache::new(client);

    assert!(cas.contains(&blob_digest).await.expect("contains"));
    assert_eq!(cas.get(&blob_digest).await.expect("get"), payload);

    // Stream directly to file and verify contents.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("artifact.bin");
    cas.get_to_file(&blob_digest, &dest)
        .await
        .expect("get_to_file");
    assert_eq!(std::fs::read(&dest).expect("read"), payload);

    // Read-only client must refuse writes.
    assert!(cas.put_bytes(b"new").await.is_err());
    assert!(ac.update(&action_digest, &action_result).await.is_err());

    // ActionCache hit returns decoded ActionResult.
    let hit = ac
        .lookup(&action_digest)
        .await
        .expect("lookup")
        .expect("hit");
    assert_eq!(hit.output_files.len(), 1);
    assert_eq!(hit.output_files[0].digest, blob_digest);

    // Verify prefix + instance were sent on the wire.
    let seen = state.lock().expect("lock").seen_paths.clone();
    assert!(
        seen.iter()
            .any(|p| p == &format!("GET /cache-prefix/team-a/cas/{}", blob_digest.hash)),
        "expected prefixed CAS path, saw {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|p| p == &format!("GET /cache-prefix/team-a/ac/{}", action_digest.hash)),
        "expected prefixed AC path, saw {seen:?}"
    );
}

#[tokio::test]
async fn http_cas_rejects_corrupted_blob_bytes() {
    let state = Arc::new(Mutex::new(MockState {
        corrupt_cas: true,
        ..Default::default()
    }));
    let payload = b"authentic bytes";
    let digest = Digest::of_bytes(payload);
    state
        .lock()
        .expect("lock")
        .cas
        .insert(digest.hash.clone(), payload.to_vec());

    let base = spawn_mock_http_server(Arc::clone(&state)).await.unwrap();
    let client = HttpRemoteClient::new(RemoteConfig::new(base).with_protocol(RemoteProtocol::Http))
        .expect("client");
    let cas = HttpCas::new(client);

    let err = cas.get(&digest).await.unwrap_err();
    assert!(
        err.to_string().contains("mismatch"),
        "unexpected err: {err}"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("bad.bin");
    assert!(cas.get_to_file(&digest, &dest).await.is_err());
    assert!(!dest.exists(), "corrupted download must be removed");
}

#[tokio::test]
async fn http_ac_rejects_oversized_action_result() {
    let state = Arc::new(Mutex::new(MockState {
        oversized_ac: true,
        ..Default::default()
    }));
    let action_digest = Digest::of_bytes(b"huge-ac");

    let base = spawn_mock_http_server(Arc::clone(&state)).await.unwrap();
    let client = HttpRemoteClient::new(RemoteConfig::new(base).with_protocol(RemoteProtocol::Http))
        .expect("client");
    let ac = HttpActionCache::new(client);

    let err = ac.lookup(&action_digest).await.unwrap_err();
    assert!(
        err.to_string().contains("exceeds limit"),
        "unexpected err: {err}"
    );
}

#[tokio::test]
async fn http_client_refuses_cross_origin_redirect_with_credentials() {
    let state = Arc::new(Mutex::new(MockState {
        redirect_to: Some("https://evil.example.com/steal-token".into()),
        ..Default::default()
    }));
    let digest = Digest::of_bytes(b"secret-blob");

    let base = spawn_mock_http_server(Arc::clone(&state)).await.unwrap();
    let client = HttpRemoteClient::new(
        RemoteConfig::new(base)
            .with_protocol(RemoteProtocol::Http)
            .with_credentials(Credentials::Bearer("top-secret".into())),
    )
    .expect("client");
    let cas = HttpCas::new(client);

    let err = cas.get(&digest).await.unwrap_err();
    assert!(
        err.to_string().contains("cross-origin"),
        "expected cross-origin redirect refusal, got: {err}"
    );
}

#[tokio::test]
async fn http_remote_integrates_with_layered_read_through_cache() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let payload = b"remote-only payload";
    let digest = Digest::of_bytes(payload);
    let action_digest = Digest::of_bytes(b"remote-action");
    let result = ActionResult {
        exit_code: 0,
        output_files: vec![OutputFile {
            path: "a.txt".into(),
            digest: digest.clone(),
            is_executable: false,
        }],
        ..Default::default()
    };
    {
        let mut guard = state.lock().expect("lock");
        guard.cas.insert(digest.hash.clone(), payload.to_vec());
        guard.ac.insert(
            action_digest.hash.clone(),
            result.to_proto().expect("proto").encode_to_vec(),
        );
    }

    let base = spawn_mock_http_server(Arc::clone(&state)).await.unwrap();
    let client = HttpRemoteClient::new(RemoteConfig::new(base).with_protocol(RemoteProtocol::Http))
        .expect("client");

    let local_dir = tempfile::tempdir().expect("tempdir");
    let local_cas = Arc::new(cuenv_cas::LocalCas::open(local_dir.path()).expect("local cas"));
    let local_ac = Arc::new(cuenv_cas::LocalActionCache::open(local_dir.path()).expect("local ac"));

    let layered_cas = LayeredCas::new(local_cas.clone(), Arc::new(HttpCas::new(client.clone())));
    let layered_ac =
        LayeredActionCache::new(local_ac.clone(), Arc::new(HttpActionCache::new(client)));

    assert!(
        layered_ac
            .lookup(&action_digest)
            .await
            .expect("ac")
            .is_some()
    );
    assert_eq!(layered_cas.get(&digest).await.expect("cas"), payload);
    assert!(local_cas.contains(&digest).await.expect("promoted locally"));
}

#[tokio::test]
async fn http_put_file_round_trips_large_artifact() -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(Mutex::new(MockState::default()));
    let base = spawn_mock_http_server(state).await?;
    let client = HttpRemoteClient::new(
        RemoteConfig::new(base)
            .with_protocol(RemoteProtocol::Http)
            .writable(),
    )?;
    let cas = HttpCas::new(client);
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("artifact.bin");
    let payload = vec![0xa5; 5 * 1024 * 1024];
    std::fs::write(&source, &payload)?;

    let digest = cas.put_file(&source).await?;
    assert_eq!(digest, Digest::of_bytes(&payload));
    let restored = temp.path().join("restored.bin");
    cas.get_to_file(&digest, &restored).await?;
    assert_eq!(std::fs::read(restored)?, payload);
    Ok(())
}

#[tokio::test]
async fn http_put_file_replays_same_origin_redirects() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"artifact";
    let digest = Digest::of_bytes(payload);
    let original = format!("/cas/{}", digest.hash);
    let redirected = format!("/intermediate/cas/{}", digest.hash);
    let final_path = format!("/backend/cas/{}", digest.hash);
    let state = Arc::new(Mutex::new(MockState {
        required_auth: Some(("x-cache-token".into(), "redirect-token".into())),
        put_redirects: HashMap::from([
            (
                original.clone(),
                PutRedirect {
                    status: "307 Temporary Redirect",
                    location: redirected.clone(),
                    rewrite: None,
                },
            ),
            (
                redirected.clone(),
                PutRedirect {
                    status: "308 Permanent Redirect",
                    location: final_path.clone(),
                    rewrite: None,
                },
            ),
        ]),
        ..Default::default()
    }));
    let base = spawn_mock_http_server(Arc::clone(&state)).await?;
    let cas = HttpCas::new(HttpRemoteClient::new(
        RemoteConfig::new(base)
            .with_protocol(RemoteProtocol::Http)
            .with_credentials(Credentials::Header {
                name: "x-cache-token".into(),
                value: "redirect-token".into(),
            })
            .writable(),
    )?);
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("artifact");
    std::fs::write(&source, payload)?;

    assert_eq!(cas.put_file(&source).await?, digest);
    let state = state.lock().expect("lock");
    assert_eq!(
        state.seen_paths,
        [
            format!("PUT {original}"),
            format!("PUT {redirected}"),
            format!("PUT {final_path}"),
        ],
    );
    assert_eq!(state.cas[&digest.hash], payload);
    Ok(())
}

#[tokio::test]
async fn http_put_file_refuses_changed_source_on_redirect() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("artifact");
    let initial = vec![0x61; 128 * 1024];
    let mut changed = initial.clone();
    changed[0] = 0x62;
    let digest = Digest::of_bytes(&initial);
    let original = format!("/cas/{}", digest.hash);
    let redirected = format!("/backend/cas/{}", digest.hash);
    std::fs::write(&source, &initial)?;
    let state = Arc::new(Mutex::new(MockState {
        put_redirects: HashMap::from([(
            original.clone(),
            PutRedirect {
                status: "307 Temporary Redirect",
                location: redirected,
                rewrite: Some((source.clone(), changed)),
            },
        )]),
        ..Default::default()
    }));
    let base = spawn_mock_http_server(Arc::clone(&state)).await?;
    let cas = HttpCas::new(HttpRemoteClient::new(
        RemoteConfig::new(base)
            .with_protocol(RemoteProtocol::Http)
            .writable(),
    )?);

    let error = cas.put_file(&source).await.expect_err("source changed");
    assert!(error.to_string().contains("digest mismatch"), "{error}");
    let state = state.lock().expect("lock");
    assert_eq!(state.seen_paths, [format!("PUT {original}")]);
    assert!(state.cas.is_empty());
    Ok(())
}

#[tokio::test]
async fn http_put_file_refuses_cross_origin_redirect() -> Result<(), Box<dyn std::error::Error>> {
    let destination = Arc::new(Mutex::new(MockState::default()));
    let destination_base = spawn_mock_http_server(Arc::clone(&destination)).await?;
    let payload = b"artifact";
    let digest = Digest::of_bytes(payload);
    let state = Arc::new(Mutex::new(MockState {
        put_redirects: HashMap::from([(
            format!("/cas/{}", digest.hash),
            PutRedirect {
                status: "308 Permanent Redirect",
                location: format!("{destination_base}/cas/{}", digest.hash),
                rewrite: None,
            },
        )]),
        ..Default::default()
    }));
    let base = spawn_mock_http_server(state).await?;
    let cas = HttpCas::new(HttpRemoteClient::new(
        RemoteConfig::new(base)
            .with_protocol(RemoteProtocol::Http)
            .with_credentials(Credentials::Bearer("redirect-token".into()))
            .writable(),
    )?);
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("artifact");
    std::fs::write(&source, payload)?;

    let error = cas
        .put_file(&source)
        .await
        .expect_err("cross-origin redirect");
    assert!(error.to_string().contains("cross-origin"), "{error}");
    assert_eq!(
        destination.lock().expect("lock").seen_paths,
        Vec::<String>::new(),
    );
    Ok(())
}
