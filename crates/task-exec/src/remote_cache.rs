//! Building the remote half of the task cache.
//!
//! `cuenv-cas-remote` speaks the Bazel Remote Execution API v2 (gRPC) and the
//! Bazel HTTP/REST cache layout; this is where a project's declared
//! configuration turns into a connected client, and where the environment gets
//! to override it so CI can inject an endpoint without editing CUE.

use cuenv_cas::{ActionCache, ActionPublisher, Cas};
use cuenv_cas_remote::{
    Credentials, HttpActionCache, HttpCas, HttpRemoteClient, LayeredActionCache, LayeredCas,
    RemoteActionCache, RemoteCas, RemoteClient, RemoteConfig, RemoteProtocol, RemotePublisher,
};
use cuenv_core::manifest::{Cache, CacheAuth, CacheProtocol, RemoteCache};
use cuenv_events::{CacheEvent, EventCategory};
use serde::Serialize;
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::Arc;
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
/// Maximum time to wait for a Bazel credential helper subprocess.
const CREDENTIAL_HELPER_TIMEOUT: Duration = Duration::from_secs(15);
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
    /// Whether upload is effectively enabled (currently false until strict Linux confinement lands).
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
    Some(remote)
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
        return match invoke_credential_helper(helper, &remote.endpoint) {
            Ok(headers) => {
                for (_, value) in &headers {
                    cuenv_events::register_secret(value.clone());
                }
                CredentialResolution::Ready(Credentials::Headers(headers))
            }
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
}

/// Invoke a Bazel-compatible credential helper (`<helper> get`) with
/// `{"uri": "<endpoint>"}` on stdin and parse `{"headers": {...}}` from stdout.
fn invoke_credential_helper(
    helper: &str,
    endpoint: &str,
) -> std::result::Result<Vec<(String, String)>, String> {
    let helper = helper.trim();
    if helper.is_empty() {
        return Err("credential helper path is empty".to_string());
    }

    let request = serde_json::to_vec(&serde_json::json!({ "uri": endpoint.trim() }))
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
                headers.push((name.clone(), value));
            }
        }
    }
    if headers.is_empty() {
        return Err(format!(
            "credential helper '{helper}' returned no credential headers"
        ));
    }

    Ok(headers)
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
    let credentials = match credentials(&remote) {
        CredentialResolution::Ready(creds) => creds,
        CredentialResolution::MissingConfigured => Credentials::None,
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

    let config = RemoteConfig::new(&remote.endpoint)
        .with_protocol(to_remote_protocol(remote.protocol))
        .with_instance_name(&remote.instance)
        .with_credentials(credentials);

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

    RemoteCacheStatus {
        configured: true,
        endpoint: Some(remote.endpoint),
        protocol: Some(proto_label),
        instance: Some(remote.instance),
        upload_requested: remote.upload,
        upload_enabled: false,
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
    let upload = remote.upload && credential_allows_upload;
    // Ordinary CAS / ActionCache layers are ALWAYS read-only so action
    // construction, cache hits, failed sandbox setups, and failed tasks can
    // never push to the remote cache. Publication flows exclusively through
    // the separate post-run `RemotePublisher` after verified strict confinement.
    let read_config = RemoteConfig::new(&remote.endpoint)
        .with_protocol(to_remote_protocol(remote.protocol))
        .with_instance_name(&remote.instance)
        .with_credentials(credentials);
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
               *'\"uri\":\"grpcs://cache.namespace.so:443\"'*) ;;\n\
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
            CredentialResolution::Ready(Credentials::Headers(headers)) => {
                assert_eq!(
                    headers,
                    vec![
                        ("Authorization".to_string(), "Bearer nsc-tok".to_string()),
                        (
                            "x-nsc-ingress-auth".to_string(),
                            "Bearer nsc-tok".to_string()
                        ),
                    ]
                );
            }
            other => panic!("expected multi-header credentials, got {other:?}"),
        }
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
