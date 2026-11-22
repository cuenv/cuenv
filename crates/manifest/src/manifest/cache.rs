//! Project-level cache configuration.
//!
//! Task-level `cache` ([`TaskCachePolicy`](crate::tasks::TaskCachePolicy))
//! decides *whether* a task is cached. This decides *where* the cache lives.

use serde::{Deserialize, Serialize};

/// Project-level cache settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cache {
    /// Remote cache to read through to. `None` is a local-only cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteCache>,

    /// Local on-disk cache settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalCache>,
}

/// Local on-disk cache settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalCache {
    /// Target maximum size for local cache garbage collection (e.g., `"20GiB"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<String>,
}

/// Transport protocol spoken by a remote cache server.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CacheProtocol {
    /// Bazel Remote Execution API v2 over gRPC (default).
    #[default]
    Grpc,
    /// Bazel HTTP/REST remote cache layout (`/cas/<sha256>`, `/ac/<sha256>`).
    Http,
}

/// A cache server speaking the Bazel Remote Execution API v2 or Bazel HTTP
/// remote cache protocol.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCache {
    /// Endpoint URL: `grpc://` (plaintext), `grpcs://` (TLS), `http://`, or `https://`.
    pub endpoint: String,

    /// Transport protocol. Defaults to [`CacheProtocol::Grpc`] so existing
    /// `http://` and `https://` gRPC endpoints continue to work unchanged.
    #[serde(default)]
    pub protocol: CacheProtocol,

    /// REAPI instance name. Empty for most single-tenant servers.
    #[serde(default)]
    pub instance: String,

    /// Upload opt-in. Reading is always allowed.
    ///
    /// Defaults to `false`. When `true`, cuenv publishes only after a task
    /// succeeds under validated Linux strict confinement with descendants
    /// reaped, outputs projected, and no project secrets in the environment.
    #[serde(default)]
    pub upload: bool,

    /// Credentials, named by environment variable or credential helper rather than value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CacheAuth>,

    /// Custom CA bundle and/or mTLS client certificate paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<CacheTls>,

    /// CI trust-boundary settings for generated workflows (`cuenv sync ci`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<RemoteCacheCi>,
}

/// Custom CA bundle and/or mTLS client certificate paths for a remote cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheTls {
    /// Path to a PEM-encoded CA certificate bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert_path: Option<String>,

    /// Path to a PEM-encoded client certificate chain for mTLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert_path: Option<String>,

    /// Path to a PEM-encoded client private key for mTLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_path: Option<String>,
}

/// Credential provisioning mode for generated CI workflows (`cuenv sync ci`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteCacheCiAuthMode {
    /// Inject static CI secrets (`read_secret` / `write_secret`) gated by
    /// `write_environment` (GitHub) or `write_queue` (Buildkite).
    #[default]
    Secrets,
    /// Credentials are provisioned outside static CI secrets (e.g. a Bazel
    /// credential helper such as `bazel-credential-nsc`, or an OIDC step
    /// writing into `$GITHUB_ENV`).
    Ambient,
}

/// CI trust-boundary settings for remote cache credentials in generated
/// workflows (`cuenv sync ci`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCacheCi {
    /// Credential provisioning mode for generated workflows.
    #[serde(default)]
    pub auth_mode: RemoteCacheCiAuthMode,

    /// Secret name holding a read-only cache credential for untrusted triggers
    /// (pull requests, forks). Omit to disable remote cache access on untrusted
    /// jobs when `auth_mode` is [`RemoteCacheCiAuthMode::Secrets`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_secret: Option<String>,

    /// Secret name holding a read-write cache credential, exposed only to
    /// explicitly authorized jobs on protected refs when `auth_mode` is
    /// [`RemoteCacheCiAuthMode::Secrets`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_secret: Option<String>,

    /// GitHub Actions Environment name gating `write_secret` to protected
    /// branches or tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_environment: Option<String>,

    /// Buildkite agent queue restricted via Buildkite secret access policies
    /// to protected branches that may access `write_secret`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_queue: Option<String>,

    /// Isolate persistent local cache directories across trust levels by
    /// setting distinct `CUENV_CACHE_DIR` paths per job trust level.
    #[serde(default = "default_true")]
    pub isolated_cache_dir: bool,
}

impl Default for RemoteCacheCi {
    fn default() -> Self {
        Self {
            auth_mode: RemoteCacheCiAuthMode::Secrets,
            read_secret: None,
            write_secret: None,
            write_environment: None,
            write_queue: None,
            isolated_cache_dir: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

/// How to authenticate to a remote cache.
///
/// Only the *name* of an environment variable or credential helper executable
/// is ever stored, so a token cannot end up committed in CUE.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheAuth {
    /// Environment variable holding a bearer token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,

    /// An arbitrary header, for a provider that names its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<CacheAuthHeader>,

    /// Path or command name of a Bazel-compatible credential helper
    /// (e.g. `bazel-credential-nsc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_helper: Option<String>,
}

/// A named header carrying a credential.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheAuthHeader {
    /// Header name.
    pub name: String,
    /// Environment variable holding the header value.
    pub value_env: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_cache_defaults_to_read_only_grpc() {
        let parsed: RemoteCache =
            serde_json::from_value(serde_json::json!({"endpoint": "grpcs://cache:443"})).unwrap();
        assert!(!parsed.upload, "uploading must be opt-in");
        assert_eq!(parsed.protocol, CacheProtocol::Grpc);
        assert_eq!(parsed.instance, "");
        assert!(parsed.auth.is_none());
    }

    #[test]
    fn http_protocol_and_local_max_size_round_trip() {
        let parsed: Cache = serde_json::from_value(serde_json::json!({
            "remote": {
                "endpoint": "https://cache.example.com/prefix",
                "protocol": "http",
                "instance": "ci",
            },
            "local": {
                "maxSize": "10GiB",
            }
        }))
        .unwrap();
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.protocol, CacheProtocol::Http);
        assert_eq!(remote.instance, "ci");
        assert_eq!(parsed.local.unwrap().max_size.as_deref(), Some("10GiB"));
    }

    #[test]
    fn bearer_auth_names_a_variable_not_a_token() {
        let parsed: Cache = serde_json::from_value(serde_json::json!({
            "remote": {
                "endpoint": "grpcs://cache:443",
                "upload": true,
                "auth": {"bearerTokenEnv": "CACHE_TOKEN"},
            }
        }))
        .unwrap();
        let remote = parsed.remote.unwrap();
        assert!(remote.upload);
        assert_eq!(
            remote.auth.unwrap().bearer_token_env.as_deref(),
            Some("CACHE_TOKEN")
        );
    }

    #[test]
    fn a_custom_header_round_trips() {
        let parsed: CacheAuth = serde_json::from_value(serde_json::json!({
            "header": {"name": "x-api-key", "valueEnv": "CACHE_KEY"}
        }))
        .unwrap();
        let header = parsed.header.unwrap();
        assert_eq!(header.name, "x-api-key");
        assert_eq!(header.value_env, "CACHE_KEY");
    }

    #[test]
    fn credential_helper_and_ambient_ci_mode_round_trip() {
        let parsed: Cache = serde_json::from_value(serde_json::json!({
            "remote": {
                "endpoint": "grpcs://cache.namespace.so:443",
                "upload": true,
                "auth": {"credentialHelper": "bazel-credential-nsc"},
                "ci": {
                    "authMode": "ambient",
                    "isolatedCacheDir": true
                }
            }
        }))
        .unwrap();
        let remote = parsed.remote.unwrap();
        assert_eq!(
            remote.auth.unwrap().credential_helper.as_deref(),
            Some("bazel-credential-nsc")
        );
        let ci = remote.ci.unwrap();
        assert_eq!(ci.auth_mode, RemoteCacheCiAuthMode::Ambient);
        assert!(ci.isolated_cache_dir);
    }

    #[test]
    fn an_absent_cache_block_is_local_only() {
        let parsed: Cache = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(parsed.remote.is_none());
        assert!(parsed.local.is_none());
    }
}
