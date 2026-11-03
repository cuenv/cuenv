//! Offline protocol tests against the checksum-pinned official Cloudflare 5.28.1
//! Linux amd64 binary. The HTTP service binds only loopback, accepts only exact
//! synthetic routes/authorization, and never records bodies in its journal.
//! Provider processes receive synthetic credentials and a loopback `base_url`;
//! their HTTP proxies also point at this server, which rejects proxy requests.
//!
//! Build the binary without modifying any user provider installation:
//!
//! ```text
//! nix build --impure --expr 'let f = builtins.getFlake (toString ./.); pkgs = import f.inputs.nixpkgs { system = "x86_64-linux"; }; in import ./crates/infrastructure/tests/cloudflare_provider.nix { inherit pkgs; }' --out-link /tmp/cuenv-cloudflare-provider-v5.28.1
//! nix develop --accept-flake-config -c env CUENV_INFRASTRUCTURE_TEST_CLOUDFLARE_PROVIDER=/tmp/cuenv-cloudflare-provider-v5.28.1/bin/terraform-provider-cloudflare_v5.28.1 cuenv exec -- cargo test -p cuenv-infrastructure --test cloudflare_offline -- --ignored
//! ```
//!
//! These prove actual provider protocol behaviour against a fake API, not live
//! Cloudflare behaviour. No Cloudflare account or real credential is used.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cuenv_infrastructure::{
    Action, ApplyContext, Cancellation, EngineOptions, EngineSetup, InfrastructureEngine,
    InfrastructureError, MemoryStateStore, Plan, PlanMode, StateStore, TenantKey,
};
use cuenv_manifest::manifest::Infrastructure;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

type TestResult<Success = ()> = Result<Success, Box<dyn std::error::Error + Send + Sync>>;

const ACCOUNT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PROVIDER_BINARY_SHA256: &str =
    "f16ab52866803039e0ad2ae1ba0f5694f305a12e82f72f453153bf865c6bbf1e";

/// Deliberately stores synthetic API objects only in memory. The operation
/// journal and rejection list contain fixed strings, never headers or bodies.
#[derive(Default)]
struct RemoteState {
    tokens: BTreeMap<String, Value>,
    sinks: BTreeMap<String, Value>,
    operations: Vec<&'static str>,
    rejections: Vec<&'static str>,
    token_creates: usize,
    sink_creates: usize,
    fail_sink_create: bool,
}

struct LoopbackApi {
    origin: String,
    authorization: String,
    state: Arc<Mutex<RemoteState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LoopbackApi {
    async fn start() -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let authority = listener.local_addr()?.to_string();
        let identity = uuid::Uuid::new_v4().simple().to_string();
        let authorization = format!("{identity}offline1");
        let state = Arc::new(Mutex::new(RemoteState::default()));
        let task_state = Arc::clone(&state);
        let task_authorization = authorization.clone();
        let task_identity = identity.clone();
        let task_authority = authority.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, peer)) = listener.accept().await {
                if !peer.ip().is_loopback() {
                    task_state.lock().await.rejections.push("non-loopback peer");
                    continue;
                }
                let request =
                    tokio::time::timeout(Duration::from_secs(3), read_request(&mut stream)).await;
                let response = if let Ok(Ok(request)) = request {
                    route(
                        request,
                        &RouteContext {
                            authority: &task_authority,
                            authorization: &task_authorization,
                            identity: &task_identity,
                        },
                        &mut *task_state.lock().await,
                    )
                } else {
                    task_state
                        .lock()
                        .await
                        .rejections
                        .push("malformed HTTP request");
                    (
                        400,
                        json!({"success": false, "errors": [{"message": "invalid request"}]}),
                    )
                };
                let body = response.1.to_string();
                let headers = format!(
                    "HTTP/1.1 {} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    response.0,
                    body.len()
                );
                let _ = stream.write_all(headers.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Ok(Self {
            origin: format!("http://{authority}"),
            authorization,
            state,
            task,
        })
    }
}

struct HttpRequest {
    method: String,
    target: String,
    host: String,
    authorization: String,
    body: Value,
}

async fn read_request(stream: &mut TcpStream) -> TestResult<HttpRequest> {
    const MAXIMUM_REQUEST_BYTES: usize = 65_536;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 || bytes.len() + count > MAXIMUM_REQUEST_BYTES {
            return Err("invalid HTTP request length".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.lines();
    let mut first = lines
        .next()
        .ok_or("missing request line")?
        .split_whitespace();
    let method = first.next().ok_or("missing method")?.to_owned();
    let target = first.next().ok_or("missing target")?.to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err("unsupported HTTP protocol".into());
    }
    let mut host = None;
    let mut authorization = None;
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid header")?;
        match name.to_ascii_lowercase().as_str() {
            "host" => {
                if host.replace(value.trim().to_owned()).is_some() {
                    return Err("duplicate host".into());
                }
            }
            "authorization" => {
                if authorization.replace(value.trim().to_owned()).is_some() {
                    return Err("duplicate authorization".into());
                }
            }
            "content-length" => {
                if length.replace(value.trim().parse::<usize>()?).is_some() {
                    return Err("duplicate content length".into());
                }
            }
            "transfer-encoding" => return Err("unsupported transfer encoding".into()),
            _ => {}
        }
    }
    let length = length.unwrap_or(0);
    if header_end + length > MAXIMUM_REQUEST_BYTES {
        return Err("request body too large".into());
    }
    while bytes.len() < header_end + length {
        let count = stream.read(&mut chunk).await?;
        if count == 0 || bytes.len() + count > MAXIMUM_REQUEST_BYTES {
            return Err("invalid request body length".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.len() != header_end + length {
        return Err("unexpected trailing HTTP data".into());
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..])?
    };
    Ok(HttpRequest {
        method,
        target,
        host: host.ok_or("host missing")?,
        authorization: authorization.unwrap_or_default(),
        body,
    })
}

struct RouteContext<'request> {
    authority: &'request str,
    authorization: &'request str,
    identity: &'request str,
}

fn reject(state: &mut RemoteState, reason: &'static str) -> (u16, Value) {
    state.rejections.push(reason);
    (
        400,
        json!({"success": false, "errors": [{"message": reason}]}),
    )
}

fn route(
    request: HttpRequest,
    context: &RouteContext<'_>,
    state: &mut RemoteState,
) -> (u16, Value) {
    if request.host != context.authority {
        return reject(state, "unexpected host or proxy request");
    }
    if request.authorization != format!("Bearer {}", context.authorization) {
        return reject(state, "unexpected authorization");
    }
    let components: Vec<_> = request.target.split('/').collect();
    if components.get(..5) != Some(["", "client", "v4", "accounts", ACCOUNT].as_slice()) {
        return reject(state, "unexpected API prefix");
    }
    let result = match (request.method.as_str(), &components[5..]) {
        ("POST", ["tokens"]) => {
            if request.body["name"] != "offline-token" || !request.body["policies"].is_array() {
                return reject(state, "unexpected token creation body");
            }
            state.token_creates += 1;
            state.operations.push("token POST");
            let identifier = format!("{:032x}", state.token_creates);
            let mut result = request.body;
            result["id"] = json!(identifier);
            result["status"] = json!("active");
            result["value"] = json!(format!(
                "offline-token-{}-{}",
                context.identity, state.token_creates
            ));
            state.tokens.insert(identifier, result.clone());
            result
        }
        ("GET", ["tokens", identifier]) => {
            state.operations.push("token GET without value");
            let Some(mut result) = state.tokens.get(*identifier).cloned() else {
                return reject(state, "unknown token identifier");
            };
            if let Some(object) = result.as_object_mut() {
                object.remove("value");
            }
            result
        }
        ("DELETE", ["tokens", identifier]) => {
            state.operations.push("token DELETE");
            if state.tokens.remove(*identifier).is_none() {
                return reject(state, "unknown token deletion");
            }
            json!({"id": identifier})
        }
        ("POST", ["pipelines", "v1", "sinks"]) => {
            let expected_token = state.tokens.values().any(|token| {
                token["status"] == "active" && token["value"] == request.body["config"]["token"]
            });
            if !expected_token
                || request.body["config"]["bucket"] != "ordinary-bucket"
                || request.body["config"]["account_id"] != ACCOUNT
            {
                return reject(state, "unexpected nested sink creation body");
            }
            state.operations.push("sink POST");
            if state.fail_sink_create {
                return (
                    400,
                    json!({"success": false, "errors": [{"code": 1000,
                    "message": format!("synthetic sink failure: {}; credential {}", request.body["config"]["token"].as_str().unwrap_or_default(), context.authorization),
                    }]}),
                );
            }
            state.sink_creates += 1;
            let identifier = format!("{:032x}", state.sink_creates + 100);
            let mut result = request.body;
            result["id"] = json!(identifier);
            result["created_at"] = json!("2026-11-03T00:00:00Z");
            result["modified_at"] = json!("2026-11-03T00:00:00Z");
            state.sinks.insert(identifier, result.clone());
            result
        }
        ("GET", ["pipelines", "v1", "sinks", identifier]) => {
            state.operations.push("sink GET");
            let Some(result) = state.sinks.get(*identifier).cloned() else {
                return reject(state, "unknown sink identifier");
            };
            result
        }
        ("DELETE", ["pipelines", "v1", "sinks", identifier]) => {
            state.operations.push("sink DELETE");
            if state.sinks.remove(*identifier).is_none() {
                return reject(state, "unknown sink deletion");
            }
            json!({"id": identifier})
        }
        _ => return reject(state, "unexpected API route or method"),
    };
    (
        200,
        json!({"success": true, "errors": [], "messages": [], "result": result}),
    )
}

struct Fixture {
    api: LoopbackApi,
    directory: tempfile::TempDir,
    binary: PathBuf,
    tenant: TenantKey,
    store: Arc<MemoryStateStore>,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let binary = PathBuf::from(std::env::var(
            "CUENV_INFRASTRUCTURE_TEST_CLOUDFLARE_PROVIDER",
        )?);
        let mut file = std::fs::File::open(&binary)?;
        let mut digest = Sha256::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let count = file.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            digest.update(&chunk[..count]);
        }
        let digest = hex::encode(digest.finalize());
        if digest != PROVIDER_BINARY_SHA256 {
            return Err(
                "expected the checksum-pinned official Cloudflare v5.28.1 Linux amd64 binary"
                    .into(),
            );
        }
        let store = Arc::new(MemoryStateStore::new());
        store.migrate().await?;
        Ok(Self {
            api: LoopbackApi::start().await?,
            directory: tempfile::tempdir()?,
            binary,
            tenant: TenantKey::try_new(
                format!("example.com/cloudflare-offline-{}", uuid::Uuid::new_v4()),
                "offline",
            )?,
            store,
        })
    }

    fn engine(&self) -> TestResult<InfrastructureEngine> {
        let variables: BTreeMap<String, String> = [
            ("HOME", self.directory.path().to_string_lossy().into_owned()),
            ("HTTP_PROXY", self.api.origin.clone()),
            ("HTTPS_PROXY", self.api.origin.clone()),
            ("http_proxy", self.api.origin.clone()),
            ("https_proxy", self.api.origin.clone()),
            ("ALL_PROXY", self.api.origin.clone()),
            ("all_proxy", self.api.origin.clone()),
            ("NO_PROXY", "127.0.0.1".to_string()),
            ("no_proxy", "127.0.0.1".to_string()),
            ("TF_LOG", "OFF".to_string()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect();
        let provided: Vec<_> = variables.keys().cloned().collect();
        let withheld = cuenv_infrastructure::plugin::isolated_withheld_names(
            std::env::vars_os().map(|(name, _)| name),
            &provided,
        );
        let infrastructure: Infrastructure = serde_json::from_value(json!({
            "state": {"turso": {"url": "http://unused"}},
            "providers": {"cloudflare": {
                "source": "cloudflare/cloudflare", "path": self.binary,
                "configuration": {"api_token": self.api.authorization, "base_url": format!("{}/client/v4/", self.api.origin)},
            }},
            "resources": {
                "token": {"type": "cloudflare_account_token", "configuration": {
                    "account_id": ACCOUNT, "name": "offline-token", "policies": [{
                        "effect": "allow", "permission_groups": [{"id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}],
                        "resources": "{\"com.cloudflare.api.account.*\":\"*\"}",
                    }],
                }},
                "sink": {"type": "cloudflare_pipeline_sink", "configuration": {
                    "account_id": ACCOUNT, "name": "offline-sink", "type": "r2",
                    "config": {"account_id": ACCOUNT, "bucket": "ordinary-bucket", "token": {
                        "fromResource": "cloudflare_account_token.token", "attribute": "value",
                    }},
                }},
            },
        }))?;
        let store: Arc<dyn StateStore> = self.store.clone();
        Ok(InfrastructureEngine::new(EngineSetup {
            tenant: self.tenant.clone(),
            store,
            infrastructure,
            options: EngineOptions {
                project_directory: self.directory.path().to_path_buf(),
                plugin_cache_directory: None,
                withheld_environment_variables: withheld,
                provider_environment_variables: variables,
                unrecorded_directory: Some(self.directory.path().join("unrecorded")),
                cancellation: Cancellation::default(),
                locked_providers: BTreeMap::new(),
            },
        }))
    }

    async fn plan(&self) -> TestResult<Plan> {
        let mut engine = self.engine()?;
        let planned = engine.plan(PlanMode::Apply).await;
        engine.shutdown().await;
        Ok(planned?)
    }

    async fn converge(&self) -> TestResult<(Plan, Result<(), InfrastructureError>)> {
        let mut engine = self.engine()?;
        let lock = self
            .store
            .lock(&self.tenant, "offline-provider-test")
            .await?;
        let planned = engine.plan(PlanMode::Apply).await;
        let result = match planned {
            Ok(plan) => {
                let applied = engine
                    .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
                    .await;
                Ok((plan, applied.map(|_| ())))
            }
            Err(error) => Err(error),
        };
        self.store.unlock(&self.tenant, &lock).await?;
        engine.shutdown().await;
        Ok(result?)
    }

    async fn record(&self, name: &str) -> TestResult<cuenv_infrastructure::ManagedResource> {
        self.store
            .list(&self.tenant)
            .await?
            .into_iter()
            .find(|record| record.address.name == name)
            .ok_or_else(|| format!("{name} not recorded").into())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires checksum-pinned Cloudflare v5.28.1 binary; see module documentation"]
async fn actual_cloudflare_provider_retains_create_only_value_and_replaces_nested_sink()
-> TestResult {
    let fixture = Fixture::new().await?;
    let (plan, applied) = fixture.converge().await?;
    applied?;
    assert!(
        plan.changes
            .iter()
            .all(|change| change.action == Action::Create)
    );
    let token = fixture.record("token").await?;
    let value = token.state["value"].as_str().ok_or("token value absent")?;
    let rendered = cuenv_infrastructure::render_plan(&plan);
    assert!(
        rendered.contains("config.token = (sensitive)"),
        "{rendered}"
    );
    assert!(!rendered.contains(value));
    let sink = fixture.record("sink").await?;
    assert!(
        sink.state["config"]["token"] == value,
        "sink did not receive create-only value"
    );
    assert_eq!(sink.state["config"]["bucket"], "ordinary-bucket");
    assert_eq!(sink.dependencies, ["cloudflare_account_token.token"]);
    assert!(
        cuenv_events::redact(value) == cuenv_events::REDACTED_PLACEHOLDER,
        "the complete source value must be registered for redaction"
    );
    {
        let state = fixture.api.state.lock().await;
        assert_eq!(state.token_creates, 1);
        assert_eq!(state.sink_creates, 1);
        assert!(state.rejections.is_empty(), "{:?}", state.rejections);
        assert!(
            state
                .sinks
                .values()
                .all(|sink| sink["config"]["token"] == value)
        );
    }

    // Each plan starts a new actual provider process. GET deliberately omits
    // value; only the provider's preserved prior state can retain it here.
    let fresh = fixture.plan().await?;
    assert!(!fresh.has_work(), "{:?}", fresh.summary());
    assert!(!cuenv_infrastructure::render_plan(&fresh).contains(value));
    {
        let mut state = fixture.api.state.lock().await;
        assert_eq!(state.token_creates, 1);
        assert!(state.operations.contains(&"token GET without value"));
        state.operations.clear();
        let identifier = token.state["id"]
            .as_str()
            .ok_or("token identifier absent")?;
        state
            .tokens
            .get_mut(identifier)
            .ok_or("remote token absent")?["status"] = json!("expired");
    }

    // The actual provider treats an expired token as absent, so a new token
    // must be created and config.token becomes unknown during the first plan.
    let (rotation, applied) = fixture.converge().await?;
    let sink_change = rotation
        .changes
        .iter()
        .find(|change| change.address.name == "sink")
        .ok_or("sink rotation not planned")?;
    assert_eq!(sink_change.action, Action::Replace);
    assert!(sink_change.requires_replace.contains(&"config".to_string()));
    applied?;
    let rotated = fixture.record("token").await?;
    assert!(
        rotated.state["value"] != token.state["value"],
        "rotation reused the old value"
    );
    assert!(
        fixture.record("sink").await?.state["config"]["token"] == rotated.state["value"],
        "rotated sink did not receive the new value"
    );
    {
        let state = fixture.api.state.lock().await;
        assert_eq!(state.token_creates, 2);
        assert_eq!(state.sink_creates, 2);
        assert!(state.rejections.is_empty(), "{:?}", state.rejections);
        let source = state
            .operations
            .iter()
            .position(|operation| *operation == "token POST")
            .ok_or("token not recreated")?;
        let sink = state
            .operations
            .iter()
            .position(|operation| *operation == "sink POST")
            .ok_or("sink not recreated")?;
        assert!(source < sink);
    }
    assert!(!fixture.plan().await?.has_work());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires checksum-pinned Cloudflare v5.28.1 binary; see module documentation"]
async fn actual_cloudflare_provider_nested_sink_errors_redact_create_only_value() -> TestResult {
    let fixture = Fixture::new().await?;
    fixture.api.state.lock().await.fail_sink_create = true;
    let (_, applied) = fixture.converge().await?;
    let error = applied.expect_err("the fake API must reject sink creation");
    let rendered = error.to_string();
    assert!(rendered.contains("synthetic sink failure"));
    let token = fixture.record("token").await?;
    let value = token.state["value"].as_str().ok_or("token value absent")?;
    assert!(
        !rendered.contains(value),
        "provider diagnostics exposed synthetic token"
    );
    assert!(
        !rendered.contains(&fixture.api.authorization),
        "provider diagnostics exposed synthetic credential"
    );
    let identity = fixture
        .api
        .authorization
        .get(..32)
        .ok_or("synthetic identity absent")?;
    assert!(
        !rendered.contains(identity),
        "provider diagnostics exposed a synthetic secret fragment"
    );
    assert_eq!(fixture.store.list(&fixture.tenant).await?.len(), 1);
    let state = fixture.api.state.lock().await;
    assert_eq!(state.token_creates, 1);
    assert_eq!(state.sink_creates, 0);
    assert!(state.rejections.is_empty(), "{:?}", state.rejections);
    Ok(())
}
