//! Building the remote half of the task cache.
//!
//! `cuenv-cas-remote` speaks the Bazel Remote Execution API v2 (gRPC) and the
//! Bazel HTTP/REST cache layout; this is where a project's declared
//! configuration turns into a connected client, and where the environment gets
//! to override it so CI can inject an endpoint without editing CUE.

use cuenv_cas::{ActionCache, ActionPublisher, Cas};
use cuenv_cas_remote::{
    CredentialProvider, Credentials, HttpActionCache, HttpCas, HttpRemoteClient,
    LayeredActionCache, LayeredCas, RemoteActionCache, RemoteCas, RemoteClient, RemoteConfig,
    RemoteProtocol, RemotePublisher, TlsConfig,
};
use cuenv_core::manifest::{Cache, CacheAuth, CacheProtocol, CacheTls, RemoteCache};
use cuenv_events::{CacheEvent, EventCategory};
use serde::Serialize;
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Environment variable overriding the configured endpoint.
const ENDPOINT_ENV: &str = "CUENV_REMOTE_CACHE";
/// Environment variable overriding the configured protocol (`grpc` or `http`).
const PROTOCOL_ENV: &str = "CUENV_REMOTE_CACHE_PROTOCOL";
/// Environment variable overriding whether this machine uploads.
const UPLOAD_ENV: &str = "CUENV_REMOTE_CACHE_UPLOAD";
const INSTANCE_ENV: &str = "CUENV_REMOTE_CACHE_INSTANCE";
/// Environment variable overriding the Bazel credential helper executable.
const CREDENTIAL_HELPER_ENV: &str = "CUENV_REMOTE_CACHE_CREDENTIAL_HELPER";
/// Environment variable overriding the custom CA certificate PEM path.
const CA_CERT_ENV: &str = "CUENV_REMOTE_CACHE_CA_CERT";
/// Environment variable overriding the mTLS client certificate PEM path.
const CLIENT_CERT_ENV: &str = "CUENV_REMOTE_CACHE_CLIENT_CERT";
/// Environment variable overriding the mTLS client private key PEM path.
const CLIENT_KEY_ENV: &str = "CUENV_REMOTE_CACHE_CLIENT_KEY";
/// Maximum time to wait for a Bazel credential helper subprocess.
const CREDENTIAL_HELPER_TIMEOUT: Duration = Duration::from_secs(15);
/// Refresh credential-helper tokens this far ahead of their `expires` timestamp.
const CREDENTIAL_REFRESH_SKEW_SECS: i64 = 60;
/// Maximum bytes read from a credential helper's stdout or stderr.
const MAX_CREDENTIAL_HELPER_OUTPUT_BYTES: u64 = 64 * 1024;

/// The local stores, wrapped with a remote layer when one is configured.
pub struct CacheLayers {
    /// Blob store the executor should use (always read-through; writes stay local).
    pub cas: Arc<dyn Cas>,
    /// Action cache the executor should use (always read-through; writes stay local).
    pub action_cache: Arc<dyn ActionCache>,
    /// Optional post-run remote publisher, present only when upload is enabled
    /// and credentials resolved cleanly.
    pub publisher: Option<Arc<dyn ActionPublisher>>,
}

/// Snapshot of remote cache resolution and connectivity for `cuenv cache status`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteCacheStatus {
    /// Whether a remote cache is configured (after env overrides).
    pub configured: bool,
    /// Effective endpoint URL, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Effective transport protocol (`"grpc"` or `"http"`), if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Effective instance name, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Whether upload was requested by config/env.
    pub upload_requested: bool,
    /// Whether upload is effectively enabled (requested, credentials resolved, and connected).
    pub upload_enabled: bool,
    /// Connectivity check outcome (`"connected"`, `"unreachable"`, `"disabled"`, etc.).
    pub state: String,
    /// Diagnostic detail when unreachable, degraded, or misconfigured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Process environment variables used to authenticate to the configured cache.
pub fn credential_variables(cache: Option<&Cache>) -> impl Iterator<Item = &str> {
    let auth = cache
        .and_then(|cache| cache.remote.as_ref())
        .and_then(|remote| remote.auth.as_ref());
    auth.into_iter().flat_map(|auth| {
        auth.bearer_token_env
            .as_deref()
            .into_iter()
            .chain(auth.header.as_ref().map(|header| header.value_env.as_str()))
    })
}

/// Resolve the effective remote configuration.
///
/// `CUENV_REMOTE_CACHE` supplies or replaces the endpoint, so a CI job can
/// point at a cache the repository does not hard-code. Setting it to an empty
/// value turns the remote off, which is the escape hatch when a cache is
/// misbehaving and editing CUE is not an option.
pub fn resolve(configured: Option<&RemoteCache>) -> Option<RemoteCache> {
    let endpoint_override = std::env::var(ENDPOINT_ENV).ok();
    if endpoint_override.as_deref() == Some("") {
        tracing::debug!("remote cache disabled by an empty {ENDPOINT_ENV}");
        return None;
    }

    let mut remote = match (configured, endpoint_override) {
        (Some(configured), endpoint) => {
            let mut remote = configured.clone();
            if let Some(endpoint) = endpoint {
                remote.endpoint = endpoint;
            }
            remote
        }
        // Configured only by the environment: no CUE, no credentials, which
        // is the right shape for an unauthenticated in-cluster cache.
        (None, Some(endpoint)) => RemoteCache {
            endpoint,
            ..RemoteCache::default()
        },
        (None, None) => return None,
    };

    if let Ok(instance) = std::env::var(INSTANCE_ENV) {
        remote.instance = instance;
    }
    if let Some(protocol) = protocol_override() {
        remote.protocol = protocol;
    }
    if let Some(upload) = upload_override() {
        remote.upload = upload;
    }
    if let Ok(helper) = std::env::var(CREDENTIAL_HELPER_ENV) {
        let helper = helper.trim();
        if !helper.is_empty() {
            remote.auth = Some(CacheAuth {
                bearer_token_env: None,
                header: None,
                credential_helper: Some(helper.to_string()),
            });
        }
    }
    apply_tls_env_overrides(&mut remote);
    Some(remote)
}

fn apply_tls_env_overrides(remote: &mut RemoteCache) {
    let ca = std::env::var(CA_CERT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let cert = std::env::var(CLIENT_CERT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let key = std::env::var(CLIENT_KEY_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if ca.is_none() && cert.is_none() && key.is_none() {
        return;
    }

    let tls = remote.tls.get_or_insert_with(CacheTls::default);
    if let Some(ca) = ca {
        tls.ca_cert_path = Some(ca);
    }
    if let Some(cert) = cert {
        tls.client_cert_path = Some(cert);
    }
    if let Some(key) = key {
        tls.client_key_path = Some(key);
    }
}

fn protocol_override() -> Option<CacheProtocol> {
    let raw = std::env::var(PROTOCOL_ENV).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "grpc" => Some(CacheProtocol::Grpc),
        "http" => Some(CacheProtocol::Http),
        other => {
            tracing::warn!(
                value = other,
                "ignoring unrecognised {PROTOCOL_ENV}; expected grpc or http"
            );
            None
        }
    }
}

fn to_remote_protocol(protocol: CacheProtocol) -> RemoteProtocol {
    match protocol {
        CacheProtocol::Grpc => RemoteProtocol::Grpc,
        CacheProtocol::Http => RemoteProtocol::Http,
    }
}

fn protocol_label(protocol: CacheProtocol) -> &'static str {
    match protocol {
        CacheProtocol::Grpc => "grpc",
        CacheProtocol::Http => "http",
    }
}

/// Parse `CUENV_REMOTE_CACHE_UPLOAD`, ignoring anything unrecognised.
///
/// An unreadable value must not silently enable uploading: the default is
/// read-only precisely because an unsound entry becomes everyone's problem.
fn upload_override() -> Option<bool> {
    let raw = std::env::var(UPLOAD_ENV).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        other => {
            tracing::warn!(
                value = other,
                "ignoring unrecognised {UPLOAD_ENV}; expected true or false"
            );
            None
        }
    }
}

/// Read credentials out of the environment variables the config names.
///
/// A configured variable that is unset is a warning rather than an error: the
/// cache still works read-only against an unauthenticated endpoint, and
/// failing the whole run because a token is missing would be a poor trade for
/// something that is only an optimization.
#[derive(Debug)]
enum CredentialResolution {
    Ready(Credentials),
    MissingConfigured,
    Ambiguous,
}

fn credentials(remote: &RemoteCache) -> CredentialResolution {
    let Some(auth) = &remote.auth else {
        return CredentialResolution::Ready(Credentials::None);
    };

    let configured_count = usize::from(auth.bearer_token_env.is_some())
        + usize::from(auth.header.is_some())
        + usize::from(auth.credential_helper.is_some());
    if configured_count > 1 {
        tracing::warn!(
            "remote cache auth config sets multiple credential methods; disabling the remote"
        );
        return CredentialResolution::Ambiguous;
    }

    if let Some(name) = &auth.bearer_token_env {
        return match std::env::var(name) {
            Ok(token) if !token.is_empty() => {
                // The project's configuration names this variable, so cuenv
                // cannot know at startup that its value is a secret.
                cuenv_events::register_secret(token.clone());
                CredentialResolution::Ready(Credentials::Bearer(token))
            }
            _ => {
                tracing::warn!(variable = name, "bearer token variable is unset or empty");
                CredentialResolution::MissingConfigured
            }
        };
    }

    if let Some(header) = &auth.header {
        return match std::env::var(&header.value_env) {
            Ok(value) if !value.is_empty() => {
                cuenv_events::register_secret(value.clone());
                CredentialResolution::Ready(Credentials::Header {
                    name: header.name.clone(),
                    value,
                })
            }
            _ => {
                tracing::warn!(
                    variable = header.value_env,
                    "credential header variable is unset or empty"
                );
                CredentialResolution::MissingConfigured
            }
        };
    }

    if let Some(helper) = &auth.credential_helper {
        return match CachingCredentialHelperProvider::initialize(helper, &remote.endpoint) {
            Ok(provider) => CredentialResolution::Ready(Credentials::Dynamic(Arc::new(provider))),
            Err(reason) => {
                tracing::warn!(
                    helper = helper,
                    reason = %reason,
                    "remote cache credential helper failed"
                );
                CredentialResolution::MissingConfigured
            }
        };
    }

    CredentialResolution::Ready(Credentials::None)
}

#[derive(serde::Deserialize)]
struct CredentialHelperResponse {
    #[serde(default)]
    headers: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    expires: Option<String>,
}

struct CachedHelperEntry {
    headers: Vec<(String, String)>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Bazel credential helper provider that caches returned headers and refreshes
/// automatically before the optional RFC 3339 `expires` timestamp.
struct CachingCredentialHelperProvider {
    helper: String,
    endpoint: String,
    cached: Mutex<CachedHelperEntry>,
}

impl CachingCredentialHelperProvider {
    fn initialize(helper: &str, endpoint: &str) -> std::result::Result<Self, String> {
        let entry = invoke_credential_helper(helper, endpoint)?;
        Ok(Self {
            helper: helper.to_string(),
            endpoint: endpoint.to_string(),
            cached: Mutex::new(entry),
        })
    }

    fn is_expired(expires_at: Option<chrono::DateTime<chrono::Utc>>) -> bool {
        let Some(expires) = expires_at else {
            return false;
        };
        let refresh_at = expires - chrono::Duration::seconds(CREDENTIAL_REFRESH_SKEW_SECS);
        chrono::Utc::now() >= refresh_at
    }
}

impl CredentialProvider for CachingCredentialHelperProvider {
    fn headers(&self) -> cuenv_cas_remote::Result<Vec<(String, String)>> {
        let mut guard = self.cached.lock().map_err(|_| {
            cuenv_cas_remote::Error::config("credential helper cache lock poisoned")
        })?;
        if Self::is_expired(guard.expires_at) {
            match invoke_credential_helper(&self.helper, &self.endpoint) {
                Ok(refreshed) => *guard = refreshed,
                Err(reason) => {
                    return Err(cuenv_cas_remote::Error::config(format!(
                        "failed to refresh remote cache credentials: {reason}"
                    )));
                }
            }
        }
        Ok(guard.headers.clone())
    }
}

/// Normalize a remote cache endpoint URI for Bazel credential helpers.
///
/// Bazel rewrites gRPC schemes to their underlying HTTP/2 transport schemes
/// (`grpcs://` -> `https://`, `grpc://` -> `http://`) before sending `uri` to
/// a credential helper; helpers such as `bazel-credential-nsc` strictly require
/// `https://`.
fn normalize_credential_helper_uri(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    match trimmed.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("grpcs") => format!("https://{rest}"),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("grpc") => format!("http://{rest}"),
        _ => trimmed.to_string(),
    }
}

/// Invoke a Bazel-compatible credential helper (`<helper> get`) with
/// `{"uri": "<normalized-endpoint>"}` on stdin and parse
/// `{"headers": {...}, "expires"?: "..."}` from stdout.
fn invoke_credential_helper(
    helper: &str,
    endpoint: &str,
) -> std::result::Result<CachedHelperEntry, String> {
    let helper = helper.trim();
    if helper.is_empty() {
        return Err("credential helper path is empty".to_string());
    }

    let uri = normalize_credential_helper_uri(endpoint);
    let request = serde_json::to_vec(&serde_json::json!({ "uri": uri }))
        .map_err(|error| format!("serialize credential helper request: {error}"))?;

    let mut child = Command::new(helper)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn credential helper '{helper}': {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&request);
        drop(stdin);
    }

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || read_bounded_pipe(stdout_pipe));
    let stderr_thread = std::thread::spawn(move || read_bounded_pipe(stderr_pipe));

    let deadline = Instant::now() + CREDENTIAL_HELPER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "credential helper '{helper}' timed out after {}s",
                        CREDENTIAL_HELPER_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for credential helper '{helper}': {error}"));
            }
        }
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| "credential helper stdout reader panicked".to_string())??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "credential helper stderr reader panicked".to_string())??;

    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_string();
        if detail.is_empty() {
            return Err(format!("credential helper '{helper}' exited with {status}"));
        }
        return Err(format!(
            "credential helper '{helper}' exited with {status}: {detail}"
        ));
    }

    let parsed: CredentialHelperResponse = serde_json::from_slice(&stdout)
        .map_err(|error| format!("credential helper '{helper}' returned invalid JSON: {error}"))?;

    let mut headers = Vec::new();
    for (name, values) in parsed.headers {
        for value in values {
            if !value.is_empty() {
                cuenv_events::register_secret(value.clone());
                headers.push((name.clone(), value));
            }
        }
    }
    if headers.is_empty() {
        return Err(format!(
            "credential helper '{helper}' returned no credential headers"
        ));
    }

    let expires_at = parsed.expires.as_deref().and_then(|raw| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|error| {
                tracing::warn!(
                    helper = helper,
                    error = %error,
                    "ignoring unparseable expires timestamp from credential helper"
                );
            })
            .ok()
    });

    Ok(CachedHelperEntry {
        headers,
        expires_at,
    })
}

fn read_bounded_pipe(pipe: Option<impl std::io::Read>) -> std::result::Result<Vec<u8>, String> {
    let Some(pipe) = pipe else {
        return Ok(Vec::new());
    };
    let mut buffer = Vec::new();
    pipe.take(MAX_CREDENTIAL_HELPER_OUTPUT_BYTES)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("read credential helper output: {error}"))?;
    Ok(buffer)
}

fn resolve_tls(remote: &RemoteCache) -> std::result::Result<TlsConfig, String> {
    let Some(tls) = &remote.tls else {
        return Ok(TlsConfig::default());
    };
    let ca_cert_pem = tls
        .ca_cert_path
        .as_deref()
        .map(|path| {
            std::fs::read(path)
                .map_err(|error| format!("read remote cache CA certificate '{path}': {error}"))
        })
        .transpose()?;
    let client_cert_pem = tls
        .client_cert_path
        .as_deref()
        .map(|path| {
            std::fs::read(path)
                .map_err(|error| format!("read remote cache client certificate '{path}': {error}"))
        })
        .transpose()?;
    let client_key_pem = tls
        .client_key_path
        .as_deref()
        .map(|path| {
            std::fs::read(path)
                .map_err(|error| format!("read remote cache client key '{path}': {error}"))
        })
        .transpose()?;
    let config = TlsConfig {
        ca_cert_pem,
        client_cert_pem,
        client_key_pem,
    };
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn emit_degraded(endpoint: &str, reason: impl Into<String>) {
    let _ = cuenv_events::emit(EventCategory::Cache(CacheEvent::RemoteDegraded {
        endpoint: endpoint.to_string(),
        reason: reason.into(),
    }));
}

/// Probe remote cache configuration and live connectivity for `cuenv cache status`.
pub async fn probe_status(cache: Option<&Cache>) -> RemoteCacheStatus {
    let Some(remote) = resolve(cache.and_then(|c| c.remote.as_ref())) else {
        return RemoteCacheStatus {
            configured: false,
            endpoint: None,
            protocol: None,
            instance: None,
            upload_requested: false,
            upload_enabled: false,
            state: "disabled".to_string(),
            detail: None,
        };
    };

    let proto_label = protocol_label(remote.protocol).to_string();
    let (credentials, credential_allows_upload) = match credentials(&remote) {
        CredentialResolution::Ready(creds) => (creds, true),
        CredentialResolution::MissingConfigured => (Credentials::None, false),
        CredentialResolution::Ambiguous => {
            return RemoteCacheStatus {
                configured: true,
                endpoint: Some(remote.endpoint),
                protocol: Some(proto_label),
                instance: Some(remote.instance),
                upload_requested: remote.upload,
                upload_enabled: false,
                state: "misconfigured".to_string(),
                detail: Some("auth sets multiple credential methods".to_string()),
            };
        }
    };

    let tls = match resolve_tls(&remote) {
        Ok(tls) => tls,
        Err(detail) => {
            return RemoteCacheStatus {
                configured: true,
                endpoint: Some(remote.endpoint),
                protocol: Some(proto_label),
                instance: Some(remote.instance),
                upload_requested: remote.upload,
                upload_enabled: false,
                state: "misconfigured".to_string(),
                detail: Some(detail),
            };
        }
    };

    let config = RemoteConfig::new(&remote.endpoint)
        .with_protocol(to_remote_protocol(remote.protocol))
        .with_instance_name(&remote.instance)
        .with_credentials(credentials)
        .with_tls(tls);

    let probe_result = match remote.protocol {
        CacheProtocol::Grpc => match RemoteClient::connect(config).await {
            Ok(client) => client.check_capabilities().await.map(|_| ()),
            Err(e) => Err(e),
        },
        CacheProtocol::Http => match HttpRemoteClient::new(config) {
            Ok(client) => client.check_health().await,
            Err(e) => Err(e),
        },
    };

    let (state, detail) = match probe_result {
        Ok(()) => ("connected".to_string(), None),
        Err(e) => ("unreachable".to_string(), Some(e.to_string())),
    };
    let upload_enabled = remote.upload && credential_allows_upload && state == "connected";

    RemoteCacheStatus {
        configured: true,
        endpoint: Some(remote.endpoint),
        protocol: Some(proto_label),
        instance: Some(remote.instance),
        upload_requested: remote.upload,
        upload_enabled,
        state,
        detail,
    }
}

type RemoteHandles = (
    Arc<dyn Cas>,
    Arc<dyn ActionCache>,
    Option<Arc<dyn ActionPublisher>>,
);

/// Stack a remote cache behind `local`, when one is configured and reachable.
///
/// A remote cache is an optimization, so every failure here degrades to the
/// local-only stores rather than failing the run: an unreachable endpoint, a
/// rejected handshake or a malformed URL all leave the user with a slower
/// build, not a broken one.
pub async fn build(
    cache: Option<&Cache>,
    local_cas: Arc<dyn Cas>,
    local_action_cache: Arc<dyn ActionCache>,
) -> CacheLayers {
    let local = CacheLayers {
        cas: local_cas,
        action_cache: local_action_cache,
        publisher: None,
    };

    let Some(remote) = resolve(cache.and_then(|cache| cache.remote.as_ref())) else {
        return local;
    };

    let (credentials, credential_allows_upload) = match credentials(&remote) {
        CredentialResolution::Ready(credentials) => (credentials, true),
        CredentialResolution::MissingConfigured => {
            tracing::warn!(
                "remote cache upload disabled because configured credentials are unavailable"
            );
            (Credentials::None, false)
        }
        CredentialResolution::Ambiguous => {
            emit_degraded(&remote.endpoint, "ambiguous auth configuration");
            return local;
        }
    };
    let tls = match resolve_tls(&remote) {
        Ok(tls) => tls,
        Err(reason) => {
            tracing::warn!(
                endpoint = remote.endpoint,
                reason = %reason,
                "invalid remote cache TLS configuration; continuing with the local cache only"
            );
            emit_degraded(&remote.endpoint, reason);
            return local;
        }
    };
    let upload = remote.upload && credential_allows_upload;
    // Ordinary CAS / ActionCache layers are ALWAYS read-only so action
    // construction, cache hits, failed sandbox setups, and failed tasks can
    // never push to the remote cache. Publication flows exclusively through
    // the separate post-run `RemotePublisher` after verified strict confinement.
    let read_config = RemoteConfig::new(&remote.endpoint)
        .with_protocol(to_remote_protocol(remote.protocol))
        .with_instance_name(&remote.instance)
        .with_credentials(credentials)
        .with_tls(tls);
    let write_config = read_config.clone().writable();

    let (remote_cas, remote_ac, publisher): RemoteHandles = match remote.protocol {
        CacheProtocol::Grpc => {
            let client = match RemoteClient::connect(read_config).await {
                Ok(client) => client,
                Err(e) => {
                    tracing::warn!(
                        endpoint = remote.endpoint,
                        error = %e,
                        "remote cache unreachable; continuing with the local cache only"
                    );
                    emit_degraded(&remote.endpoint, e.to_string());
                    return local;
                }
            };

            let remote_cas = match RemoteCas::connect(client.clone()).await {
                Ok(remote_cas) => remote_cas,
                Err(e) => {
                    tracing::warn!(
                        endpoint = remote.endpoint,
                        error = %e,
                        "remote cache rejected the capabilities handshake; continuing with the local cache only"
                    );
                    emit_degraded(&remote.endpoint, e.to_string());
                    return local;
                }
            };

            let publisher = if upload {
                match RemoteClient::connect(write_config).await {
                    Ok(write_client) => {
                        let write_cas = Arc::new(RemoteCas::with_capabilities(
                            write_client.clone(),
                            remote_cas.capabilities().clone(),
                        ));
                        let write_ac = Arc::new(RemoteActionCache::new(write_client));
                        Some(Arc::new(RemotePublisher::grpc(write_cas, write_ac))
                            as Arc<dyn ActionPublisher>)
                    }
                    Err(e) => {
                        tracing::warn!(
                            endpoint = remote.endpoint,
                            error = %e,
                            "could not initialize remote cache publisher; continuing read-only"
                        );
                        None
                    }
                }
            } else {
                None
            };

            (
                Arc::new(remote_cas),
                Arc::new(RemoteActionCache::new(client)),
                publisher,
            )
        }
        CacheProtocol::Http => {
            let client = match HttpRemoteClient::new(read_config) {
                Ok(client) => client,
                Err(e) => {
                    tracing::warn!(
                        endpoint = remote.endpoint,
                        error = %e,
                        "invalid HTTP remote cache configuration; continuing with the local cache only"
                    );
                    emit_degraded(&remote.endpoint, e.to_string());
                    return local;
                }
            };

            if let Err(e) = client.check_health().await {
                tracing::warn!(
                    endpoint = remote.endpoint,
                    error = %e,
                    "HTTP remote cache unreachable; continuing with the local cache only"
                );
                emit_degraded(&remote.endpoint, e.to_string());
                return local;
            }

            let publisher = if upload {
                match HttpRemoteClient::new(write_config) {
                    Ok(write_client) => {
                        let write_cas = Arc::new(HttpCas::new(write_client.clone()));
                        let write_ac = Arc::new(HttpActionCache::new(write_client));
                        Some(Arc::new(RemotePublisher::http(write_cas, write_ac))
                            as Arc<dyn ActionPublisher>)
                    }
                    Err(e) => {
                        tracing::warn!(
                            endpoint = remote.endpoint,
                            error = %e,
                            "could not initialize HTTP remote cache publisher; continuing read-only"
                        );
                        None
                    }
                }
            } else {
                None
            };

            (
                Arc::new(HttpCas::new(client.clone())),
                Arc::new(HttpActionCache::new(client)),
                publisher,
            )
        }
    };

    tracing::info!(
        endpoint = remote.endpoint,
        protocol = protocol_label(remote.protocol),
        instance = remote.instance,
        upload = publisher.is_some(),
        "remote cache connected"
    );
    let _ = cuenv_events::emit(EventCategory::Cache(CacheEvent::RemoteConnected {
        endpoint: remote.endpoint.clone(),
        protocol: protocol_label(remote.protocol).to_string(),
        instance: remote.instance.clone(),
        upload_enabled: publisher.is_some(),
    }));

    let cas = LayeredCas::new(local.cas, remote_cas.clone());
    let action_cache =
        LayeredActionCache::new(local.action_cache, remote_ac).with_remote_cas(remote_cas);

    CacheLayers {
        cas: Arc::new(cas),
        action_cache: Arc::new(action_cache),
        publisher,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::{CacheAuth, CacheAuthHeader};

    fn configured(endpoint: &str) -> RemoteCache {
        RemoteCache {
            endpoint: endpoint.to_string(),
            ..RemoteCache::default()
        }
    }

    #[test]
    fn no_configuration_and_no_environment_means_local_only() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
            ],
            || {
                assert!(resolve(None).is_none());
            },
        );
    }

    #[test]
    fn the_environment_can_supply_an_endpoint_on_its_own() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("grpcs://ci-cache:443")),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
            ],
            || {
                let resolved = resolve(None).expect("endpoint from the environment");
                assert_eq!(resolved.endpoint, "grpcs://ci-cache:443");
                assert_eq!(resolved.protocol, CacheProtocol::Grpc);
                assert!(!resolved.upload, "uploading stays opt-in");
            },
        );
    }

    #[test]
    fn the_environment_can_select_http_protocol() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("https://ci-cache.example.com")),
                (PROTOCOL_ENV, Some("http")),
                (UPLOAD_ENV, None),
            ],
            || {
                let resolved = resolve(None).expect("endpoint from the environment");
                assert_eq!(resolved.protocol, CacheProtocol::Http);
            },
        );
    }

    #[test]
    fn the_environment_overrides_a_configured_endpoint() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("grpcs://override:443")),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
            ],
            || {
                let resolved = resolve(Some(&configured("grpcs://from-cue:443"))).unwrap();
                assert_eq!(resolved.endpoint, "grpcs://override:443");
            },
        );
    }

    #[test]
    fn an_empty_endpoint_variable_turns_the_remote_off() {
        // The escape hatch when a cache misbehaves and editing CUE is not an
        // option.
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("")),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
            ],
            || {
                assert!(resolve(Some(&configured("grpcs://from-cue:443"))).is_none());
            },
        );
    }

    #[test]
    fn upload_can_be_enabled_for_a_trusted_builder() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, Some("true")),
            ],
            || {
                let resolved = resolve(Some(&configured("grpcs://cache:443"))).unwrap();
                assert!(resolved.upload);
            },
        );
    }

    #[test]
    fn upload_can_be_disabled_for_an_untrusted_one() {
        let mut uploading = configured("grpcs://cache:443");
        uploading.upload = true;
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, Some("false")),
            ],
            || assert!(!resolve(Some(&uploading)).unwrap().upload),
        );
    }

    #[test]
    fn an_unreadable_upload_value_never_enables_uploading() {
        // Failing open here would let a typo publish unsound entries to a
        // shared cache.
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, Some("maybe")),
            ],
            || {
                assert!(
                    !resolve(Some(&configured("grpcs://cache:443")))
                        .unwrap()
                        .upload
                );
            },
        );
    }

    #[test]
    fn a_bearer_token_is_read_from_the_named_variable() {
        let mut remote = configured("grpcs://cache:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: Some("TEST_CACHE_TOKEN".to_string()),
            header: None,
            credential_helper: None,
        });
        temp_env::with_var("TEST_CACHE_TOKEN", Some("tok"), || {
            assert!(matches!(
                credentials(&remote),
                CredentialResolution::Ready(Credentials::Bearer(token)) if token == "tok"
            ));
        });
    }

    #[test]
    fn a_missing_token_disables_authenticated_writes() {
        let mut remote = configured("grpcs://cache:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: Some("TEST_CACHE_TOKEN_ABSENT".to_string()),
            header: None,
            credential_helper: None,
        });
        temp_env::with_var("TEST_CACHE_TOKEN_ABSENT", None::<&str>, || {
            assert!(matches!(
                credentials(&remote),
                CredentialResolution::MissingConfigured
            ));
        });
    }

    #[test]
    fn a_custom_header_is_read_from_the_named_variable() {
        let mut remote = configured("grpcs://cache:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: None,
            header: Some(CacheAuthHeader {
                name: "x-api-key".to_string(),
                value_env: "TEST_CACHE_KEY".to_string(),
            }),
            credential_helper: None,
        });
        temp_env::with_var("TEST_CACHE_KEY", Some("abc"), || {
            match credentials(&remote) {
                CredentialResolution::Ready(Credentials::Header { name, value }) => {
                    assert_eq!(name, "x-api-key");
                    assert_eq!(value, "abc");
                }
                other => panic!("expected a header credential, got {other:?}"),
            }
        });
    }

    #[test]
    fn no_auth_block_means_anonymous() {
        assert!(matches!(
            credentials(&configured("grpc://cache:1")),
            CredentialResolution::Ready(Credentials::None)
        ));
    }

    #[test]
    fn credential_helper_env_overrides_auth_config() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
                (CREDENTIAL_HELPER_ENV, Some("bazel-credential-nsc")),
            ],
            || {
                let resolved =
                    resolve(Some(&configured("grpcs://cache.namespace.so:443"))).unwrap();
                assert_eq!(
                    resolved.auth.unwrap().credential_helper.as_deref(),
                    Some("bazel-credential-nsc")
                );
            },
        );
    }

    #[test]
    fn credential_helper_uri_normalizes_grpc_schemes_to_http_schemes() {
        assert_eq!(
            normalize_credential_helper_uri("grpcs://cache.namespace.so:443"),
            "https://cache.namespace.so:443"
        );
        assert_eq!(
            normalize_credential_helper_uri("grpc://localhost:8980"),
            "http://localhost:8980"
        );
        assert_eq!(
            normalize_credential_helper_uri("  GRPCS://cache.namespaced.app:443/prefix  "),
            "https://cache.namespaced.app:443/prefix"
        );
        assert_eq!(
            normalize_credential_helper_uri("GRPC://cache.internal:8980"),
            "http://cache.internal:8980"
        );
        assert_eq!(
            normalize_credential_helper_uri("https://cache.example.com/bazel"),
            "https://cache.example.com/bazel"
        );
        assert_eq!(
            normalize_credential_helper_uri("http://localhost:8080/bazel"),
            "http://localhost:8080/bazel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_invokes_get_and_parses_multi_header_json() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("fake-bazel-credential-nsc");
        std::fs::write(
            &helper,
            "#!/bin/sh\n\
             if [ \"$1\" != \"get\" ]; then exit 2; fi\n\
             input=$(cat)\n\
             case \"$input\" in\n\
               *'\"uri\":\"https://cache.namespace.so:443\"'*) ;;\n\
               *) echo \"unexpected stdin: $input\" >&2; exit 3 ;;\n\
             esac\n\
             printf '{\"headers\":{\"Authorization\":[\"Bearer nsc-tok\"],\"x-nsc-ingress-auth\":[\"Bearer nsc-tok\"]}}\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut remote = configured("grpcs://cache.namespace.so:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: None,
            header: None,
            credential_helper: Some(helper.to_string_lossy().into_owned()),
        });

        match credentials(&remote) {
            CredentialResolution::Ready(Credentials::Dynamic(provider)) => {
                assert_eq!(
                    provider.headers().unwrap(),
                    vec![
                        ("Authorization".to_string(), "Bearer nsc-tok".to_string()),
                        (
                            "x-nsc-ingress-auth".to_string(),
                            "Bearer nsc-tok".to_string()
                        ),
                    ]
                );
            }
            other => panic!("expected dynamic credential helper provider, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_refreshes_when_expires_is_past() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let helper = dir.path().join("refreshing-helper");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\n\
                 n=0\n\
                 if [ -f \"{}\" ]; then n=$(cat \"{}\"); fi\n\
                 n=$((n + 1))\n\
                 printf '%s' \"$n\" > \"{}\"\n\
                 cat >/dev/null\n\
                 if [ \"$n\" = \"1\" ]; then\n\
                   printf '{{\"headers\":{{\"Authorization\":[\"Bearer tok-1\"]}},\"expires\":\"2020-01-01T00:00:00Z\"}}\\n'\n\
                 else\n\
                   printf '{{\"headers\":{{\"Authorization\":[\"Bearer tok-2\"]}},\"expires\":\"2099-01-01T00:00:00Z\"}}\\n'\n\
                 fi\n",
                counter.display(),
                counter.display(),
                counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut remote = configured("grpcs://cache.namespace.so:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: None,
            header: None,
            credential_helper: Some(helper.to_string_lossy().into_owned()),
        });

        let CredentialResolution::Ready(Credentials::Dynamic(provider)) = credentials(&remote)
        else {
            panic!("expected dynamic provider");
        };
        // First headers() sees past `expires`, refreshes to tok-2.
        assert_eq!(
            provider.headers().unwrap(),
            vec![("Authorization".to_string(), "Bearer tok-2".to_string())]
        );
        // Second headers() serves cached tok-2 without invoking helper again (count stays 2).
        assert_eq!(
            provider.headers().unwrap(),
            vec![("Authorization".to_string(), "Bearer tok-2".to_string())]
        );
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), "2");
    }

    #[test]
    fn tls_env_overrides_populate_ca_and_mtls_paths() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, None::<&str>),
                (PROTOCOL_ENV, None),
                (UPLOAD_ENV, None),
                (CREDENTIAL_HELPER_ENV, None),
                (CA_CERT_ENV, Some("/etc/ssl/custom-ca.pem")),
                (CLIENT_CERT_ENV, Some("/run/mtls/client.crt")),
                (CLIENT_KEY_ENV, Some("/run/mtls/client.key")),
            ],
            || {
                let resolved =
                    resolve(Some(&configured("grpcs://cache.namespace.so:443"))).unwrap();
                let tls = resolved.tls.unwrap();
                assert_eq!(tls.ca_cert_path.as_deref(), Some("/etc/ssl/custom-ca.pem"));
                assert_eq!(
                    tls.client_cert_path.as_deref(),
                    Some("/run/mtls/client.crt")
                );
                assert_eq!(tls.client_key_path.as_deref(), Some("/run/mtls/client.key"));
            },
        );
    }

    #[test]
    fn failing_credential_helper_disables_authenticated_writes() {
        let mut remote = configured("grpcs://cache.namespace.so:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: None,
            header: None,
            credential_helper: Some("/nonexistent/bazel-credential-nsc".to_string()),
        });

        assert!(matches!(
            credentials(&remote),
            CredentialResolution::MissingConfigured
        ));
    }
}
