//! Building the remote half of the task cache.
//!
//! `cuenv-cas-remote` speaks the Bazel Remote Execution API; this is where a
//! project's declared configuration turns into a connected client, and where
//! the environment gets to override it so CI can inject an endpoint without
//! editing CUE.

use cuenv_cas::{ActionCache, Cas};
use cuenv_cas_remote::{
    Credentials, LayeredActionCache, LayeredCas, RemoteActionCache, RemoteCas, RemoteClient,
    RemoteConfig,
};
use cuenv_core::manifest::{Cache, RemoteCache};
use std::sync::Arc;

/// Environment variable overriding the configured endpoint.
const ENDPOINT_ENV: &str = "CUENV_REMOTE_CACHE";
/// Environment variable overriding whether this machine uploads.
const UPLOAD_ENV: &str = "CUENV_REMOTE_CACHE_UPLOAD";

/// The local stores, wrapped with a remote layer when one is configured.
pub struct CacheLayers {
    /// Blob store the executor should use.
    pub cas: Arc<dyn Cas>,
    /// Action cache the executor should use.
    pub action_cache: Arc<dyn ActionCache>,
}

/// Resolve the effective remote configuration.
///
/// `CUENV_REMOTE_CACHE` supplies or replaces the endpoint, so a CI job can
/// point at a cache the repository does not hard-code. Setting it to an empty
/// value turns the remote off, which is the escape hatch when a cache is
/// misbehaving and editing CUE is not an option.
fn resolve(configured: Option<&RemoteCache>) -> Option<RemoteCache> {
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

    if let Some(upload) = upload_override() {
        remote.upload = upload;
    }
    Some(remote)
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

    if auth.bearer_token_env.is_some() && auth.header.is_some() {
        tracing::warn!(
            "remote cache auth config sets both bearerTokenEnv and header; disabling the remote"
        );
        return CredentialResolution::Ambiguous;
    }

    if let Some(name) = &auth.bearer_token_env {
        return match std::env::var(name) {
            Ok(token) if !token.is_empty() => {
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
            Ok(value) if !value.is_empty() => CredentialResolution::Ready(Credentials::Header {
                name: header.name.clone(),
                value,
            }),
            _ => {
                tracing::warn!(
                    variable = header.value_env,
                    "credential header variable is unset or empty"
                );
                CredentialResolution::MissingConfigured
            }
        };
    }

    CredentialResolution::Ready(Credentials::None)
}

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
        CredentialResolution::Ambiguous => return local,
    };
    let upload = false;
    if remote.upload && credential_allows_upload {
        tracing::warn!(
            "remote cache upload is temporarily disabled until strict filesystem isolation is available"
        );
    }
    let config = RemoteConfig::new(&remote.endpoint)
        .with_instance_name(&remote.instance)
        .with_credentials(credentials);
    let config = if upload { config.writable() } else { config };

    let client = match RemoteClient::connect(config).await {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(
                endpoint = remote.endpoint,
                error = %e,
                "remote cache unreachable; continuing with the local cache only"
            );
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
            return local;
        }
    };

    tracing::info!(
        endpoint = remote.endpoint,
        instance = remote.instance,
        upload,
        "remote cache connected"
    );

    let remote_cas = Arc::new(remote_cas) as Arc<dyn Cas>;
    let mut cas = LayeredCas::new(local.cas, remote_cas.clone());
    let mut action_cache =
        LayeredActionCache::new(local.action_cache, Arc::new(RemoteActionCache::new(client)))
            .with_remote_cas(remote_cas);
    if upload {
        cas = cas.with_push();
        action_cache = action_cache.with_push();
    }

    CacheLayers {
        cas: Arc::new(cas),
        action_cache: Arc::new(action_cache),
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
        temp_env::with_vars([(ENDPOINT_ENV, None::<&str>), (UPLOAD_ENV, None)], || {
            assert!(resolve(None).is_none())
        });
    }

    #[test]
    fn the_environment_can_supply_an_endpoint_on_its_own() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("grpcs://ci-cache:443")),
                (UPLOAD_ENV, None),
            ],
            || {
                let resolved = resolve(None).expect("endpoint from the environment");
                assert_eq!(resolved.endpoint, "grpcs://ci-cache:443");
                assert!(!resolved.upload, "uploading stays opt-in");
            },
        );
    }

    #[test]
    fn the_environment_overrides_a_configured_endpoint() {
        temp_env::with_vars(
            [
                (ENDPOINT_ENV, Some("grpcs://override:443")),
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
        temp_env::with_vars([(ENDPOINT_ENV, Some("")), (UPLOAD_ENV, None)], || {
            assert!(resolve(Some(&configured("grpcs://from-cue:443"))).is_none());
        });
    }

    #[test]
    fn upload_can_be_enabled_for_a_trusted_builder() {
        temp_env::with_vars(
            [(ENDPOINT_ENV, None::<&str>), (UPLOAD_ENV, Some("true"))],
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
            [(ENDPOINT_ENV, None::<&str>), (UPLOAD_ENV, Some("false"))],
            || assert!(!resolve(Some(&uploading)).unwrap().upload),
        );
    }

    #[test]
    fn an_unreadable_upload_value_never_enables_uploading() {
        // Failing open here would let a typo publish unsound entries to a
        // shared cache.
        temp_env::with_vars(
            [(ENDPOINT_ENV, None::<&str>), (UPLOAD_ENV, Some("maybe"))],
            || {
                assert!(
                    !resolve(Some(&configured("grpcs://cache:443")))
                        .unwrap()
                        .upload
                )
            },
        );
    }

    #[test]
    fn a_bearer_token_is_read_from_the_named_variable() {
        let mut remote = configured("grpcs://cache:443");
        remote.auth = Some(CacheAuth {
            bearer_token_env: Some("TEST_CACHE_TOKEN".to_string()),
            header: None,
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
}
