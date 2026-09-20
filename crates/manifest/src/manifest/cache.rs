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
}

/// A cache server speaking the Bazel Remote Execution API v2.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCache {
    /// Endpoint URL: `grpc://` (plaintext) or `grpcs://` (TLS).
    pub endpoint: String,

    /// REAPI instance name. Empty for most single-tenant servers.
    #[serde(default)]
    pub instance: String,

    /// Whether this machine may upload. Reading is always allowed.
    ///
    /// Defaults to `false` and should stay there until filesystem isolation
    /// lands: a task can currently read files it did not declare, so an entry
    /// it records may be wrong elsewhere, and uploading is what turns one
    /// machine's unsound entry into everyone's.
    #[serde(default)]
    pub upload: bool,

    /// Credentials, named by environment variable rather than value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CacheAuth>,
}

/// How to authenticate to a remote cache.
///
/// Only the *name* of an environment variable is ever stored, so a token
/// cannot end up committed in CUE.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheAuth {
    /// Environment variable holding a bearer token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,

    /// An arbitrary header, for a provider that names its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<CacheAuthHeader>,
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
    fn a_remote_cache_defaults_to_read_only() {
        let parsed: RemoteCache =
            serde_json::from_value(serde_json::json!({"endpoint": "grpcs://cache:443"})).unwrap();
        assert!(!parsed.upload, "uploading must be opt-in");
        assert_eq!(parsed.instance, "");
        assert!(parsed.auth.is_none());
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
    fn an_absent_cache_block_is_local_only() {
        let parsed: Cache = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(parsed.remote.is_none());
    }
}
