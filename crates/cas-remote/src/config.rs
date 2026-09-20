//! Endpoint, instance and credential configuration for a remote cache.

use crate::error::{Error, Result};
use std::time::Duration;

/// How a client proves who it is to the remote cache.
#[derive(Clone, Default)]
pub enum Credentials {
    /// Send nothing. Appropriate for a cache on a trusted network, such as a
    /// `bazel-remote` running inside the same cluster.
    #[default]
    None,
    /// Send `authorization: Bearer <token>` on every request.
    ///
    /// This is what the hosted providers issue — Namespace's
    /// `nsc bazel setup --static` writes exactly this as a Bazel
    /// `--remote_header`, and BuildBuddy's API key works the same way.
    Bearer(String),
    /// Send an arbitrary header, for a provider that names its own.
    Header {
        /// Header name.
        name: String,
        /// Header value.
        value: String,
    },
}

impl std::fmt::Debug for Credentials {
    /// Never render the secret: this type ends up in error and trace output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::Header { name, .. } => write!(f, "Header {{ name: {name:?}, value: <redacted> }}"),
        }
    }
}

/// Connection settings for a REAPI cache.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Endpoint URL. `grpc://` is plaintext, `grpcs://` is TLS; `http://`
    /// and `https://` are accepted as synonyms because that is what most
    /// providers print in their setup output.
    pub endpoint: String,
    /// REAPI instance name. Most single-tenant servers use the empty string;
    /// multi-tenant providers use it to select a cache.
    pub instance_name: String,
    /// Credentials sent with every request.
    pub credentials: Credentials,
    /// Per-request deadline.
    pub timeout: Duration,
    /// Whether this client may write. A build that cannot be trusted to have
    /// produced sound results — a fork's pull request, say — should read the
    /// shared cache without being able to poison it.
    pub writable: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            instance_name: String::new(),
            credentials: Credentials::None,
            timeout: Duration::from_secs(60),
            writable: false,
        }
    }
}

impl RemoteConfig {
    /// Build a configuration for `endpoint`, leaving everything else default.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            ..Self::default()
        }
    }

    /// Set the REAPI instance name.
    #[must_use]
    pub fn with_instance_name(mut self, instance_name: impl Into<String>) -> Self {
        self.instance_name = instance_name.into();
        self
    }

    /// Set the credentials.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = credentials;
        self
    }

    /// Allow this client to upload blobs and update action results.
    #[must_use]
    pub fn writable(mut self) -> Self {
        self.writable = true;
        self
    }

    /// Set the per-request deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The endpoint as a URI tonic can dial.
    ///
    /// `grpc`/`grpcs` are the schemes Bazel's `--remote_cache` accepts and so
    /// the ones providers document; they are rewritten to `http`/`https`,
    /// which is what they mean on the wire.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is empty, carries an unknown scheme,
    /// or is not a valid URI.
    pub fn uri(&self) -> Result<tonic::transport::Uri> {
        let endpoint = self.endpoint.trim();
        if endpoint.is_empty() {
            return Err(Error::config("remote cache endpoint is empty"));
        }

        let rewritten = match endpoint.split_once("://") {
            Some(("grpc", rest)) => format!("http://{rest}"),
            Some(("grpcs", rest)) => format!("https://{rest}"),
            Some(("http" | "https", _)) => endpoint.to_string(),
            Some((scheme, _)) => {
                return Err(Error::config(format!(
                    "unsupported remote cache scheme '{scheme}': use grpc:// or grpcs://"
                )));
            }
            // A bare host:port is ambiguous, and guessing wrong either sends
            // credentials in the clear or fails a handshake with a confusing
            // error. Make the user say which they meant.
            None => {
                return Err(Error::config(format!(
                    "remote cache endpoint '{endpoint}' has no scheme: \
                     prefix it with grpcs:// (TLS) or grpc:// (plaintext)"
                )));
            }
        };

        rewritten
            .parse()
            .map_err(|e| Error::config(format!("invalid remote cache endpoint '{endpoint}': {e}")))
    }

    /// Whether the endpoint will be dialled over TLS.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Self::uri`].
    pub fn is_tls(&self) -> Result<bool> {
        Ok(self.uri()?.scheme_str() == Some("https"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpcs_becomes_https() {
        let config = RemoteConfig::new("grpcs://cache.example.com:443");
        assert_eq!(
            config.uri().unwrap().to_string(),
            "https://cache.example.com:443/"
        );
        assert!(config.is_tls().unwrap());
    }

    #[test]
    fn grpc_becomes_http() {
        let config = RemoteConfig::new("grpc://localhost:8980");
        assert_eq!(config.uri().unwrap().to_string(), "http://localhost:8980/");
        assert!(!config.is_tls().unwrap());
    }

    #[test]
    fn http_and_https_pass_through() {
        assert!(!RemoteConfig::new("http://localhost:8980").is_tls().unwrap());
        assert!(RemoteConfig::new("https://cache.example.com").is_tls().unwrap());
    }

    #[test]
    fn a_bare_host_is_rejected_rather_than_guessed() {
        // Guessing plaintext would send a bearer token in the clear.
        let error = RemoteConfig::new("cache.example.com:443").uri().unwrap_err();
        assert!(error.to_string().contains("no scheme"), "{error}");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let error = RemoteConfig::new("ftp://cache.example.com").uri().unwrap_err();
        assert!(error.to_string().contains("unsupported"), "{error}");
    }

    #[test]
    fn empty_endpoint_is_rejected() {
        assert!(RemoteConfig::new("   ").uri().is_err());
    }

    #[test]
    fn clients_are_read_only_until_asked_otherwise() {
        assert!(!RemoteConfig::new("grpc://x:1").writable);
        assert!(RemoteConfig::new("grpc://x:1").writable().writable);
    }

    #[test]
    fn credentials_never_render_their_secret() {
        let creds = Credentials::Bearer("super-secret-token".into());
        assert!(!format!("{creds:?}").contains("super-secret-token"));

        let header = Credentials::Header {
            name: "x-api-key".into(),
            value: "super-secret-token".into(),
        };
        let rendered = format!("{header:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("x-api-key"));
    }

    #[test]
    fn config_debug_does_not_leak_credentials() {
        let config = RemoteConfig::new("grpcs://cache.example.com")
            .with_credentials(Credentials::Bearer("super-secret-token".into()));
        assert!(!format!("{config:?}").contains("super-secret-token"));
    }
}
