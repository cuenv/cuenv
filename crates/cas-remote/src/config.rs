//! Endpoint, instance and credential configuration for a remote cache.

use crate::error::{Error, Result};
use crate::retry::RetryPolicy;
use std::sync::Arc;
use std::time::Duration;

/// Dynamic credential supplier invoked per request (for example a cached Bazel
/// credential helper that refreshes automatically before `expires`).
pub trait CredentialProvider: Send + Sync {
    /// Return the headers to attach to the next request.
    ///
    /// # Errors
    ///
    /// Returns an error if credentials cannot be obtained or refreshed.
    fn headers(&self) -> Result<Vec<(String, String)>>;
}

/// Custom CA bundle and/or mTLS client identity for TLS remote cache endpoints.
#[derive(Clone, Default)]
pub struct TlsConfig {
    /// PEM-encoded CA certificate bundle added to the TLS trust store.
    pub ca_cert_pem: Option<Vec<u8>>,
    /// PEM-encoded client certificate chain for mTLS.
    pub client_cert_pem: Option<Vec<u8>>,
    /// PEM-encoded client private key for mTLS.
    pub client_key_pem: Option<Vec<u8>>,
}

impl TlsConfig {
    /// Whether any custom CA or client certificate is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ca_cert_pem.is_none()
            && self.client_cert_pem.is_none()
            && self.client_key_pem.is_none()
    }

    /// Validate that client certificate and private key are paired.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when only one of `client_cert_pem` or
    /// `client_key_pem` is provided.
    pub fn validate(&self) -> Result<()> {
        match (&self.client_cert_pem, &self.client_key_pem) {
            (Some(_), None) => Err(Error::config(
                "mTLS client certificate is set without a matching client private key",
            )),
            (None, Some(_)) => Err(Error::config(
                "mTLS client private key is set without a matching client certificate",
            )),
            _ => Ok(()),
        }
    }
}

impl std::fmt::Debug for TlsConfig {
    /// Never render private key bytes in debug/trace output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig")
            .field(
                "ca_cert_pem",
                &self.ca_cert_pem.as_ref().map(|_| "<present>"),
            )
            .field(
                "client_cert_pem",
                &self.client_cert_pem.as_ref().map(|_| "<present>"),
            )
            .field(
                "client_key_pem",
                &self.client_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

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
    /// Send multiple headers, such as those returned by a Bazel credential
    /// helper (e.g. `bazel-credential-nsc`, which emits both `authorization`
    /// and `x-nsc-ingress-auth`).
    Headers(Vec<(String, String)>),
    /// Dynamically resolve headers per request (e.g. a credential helper with
    /// `expires`-aware caching and automatic mid-build refresh).
    Dynamic(Arc<dyn CredentialProvider>),
}

impl std::fmt::Debug for Credentials {
    /// Never render the secret: this type ends up in error and trace output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::Header { name, .. } => {
                write!(f, "Header {{ name: {name:?}, value: <redacted> }}")
            }
            Self::Headers(headers) => {
                let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
                write!(f, "Headers {{ names: {names:?}, values: <redacted> }}")
            }
            Self::Dynamic(_) => f.write_str("Dynamic(<provider>)"),
        }
    }
}

/// Transport protocol spoken by a remote cache server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteProtocol {
    /// Bazel Remote Execution API v2 over gRPC (default, including for
    /// `http://` and `https://` endpoints for backwards compatibility).
    #[default]
    Grpc,
    /// Bazel HTTP/REST remote cache layout (`/cas/<sha256>`, `/ac/<sha256>`).
    Http,
}

/// Connection settings for a remote cache.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Endpoint URL. `grpc://` is plaintext, `grpcs://` is TLS; `http://`
    /// and `https://` are accepted as gRPC synonyms when `protocol` is
    /// [`RemoteProtocol::Grpc`], or as HTTP/REST endpoints when `protocol` is
    /// [`RemoteProtocol::Http`].
    pub endpoint: String,
    /// Transport protocol. Defaults to [`RemoteProtocol::Grpc`].
    pub protocol: RemoteProtocol,
    /// REAPI instance name. Most single-tenant servers use the empty string;
    /// multi-tenant providers use it to select a cache.
    pub instance_name: String,
    /// Credentials sent with every request.
    pub credentials: Credentials,
    /// Custom CA bundle and/or mTLS client identity.
    pub tls: TlsConfig,
    /// Per-request deadline.
    pub timeout: Duration,
    /// Retry policy for transient RPC failures.
    pub retry: RetryPolicy,
    /// Whether this client may write. A build that cannot be trusted to have
    /// produced sound results — a fork's pull request, say — should read the
    /// shared cache without being able to poison it.
    pub writable: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            protocol: RemoteProtocol::Grpc,
            instance_name: String::new(),
            credentials: Credentials::None,
            tls: TlsConfig::default(),
            timeout: Duration::from_secs(60),
            retry: RetryPolicy::default(),
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

    /// Set the transport protocol.
    #[must_use]
    pub fn with_protocol(mut self, protocol: RemoteProtocol) -> Self {
        self.protocol = protocol;
        self
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

    /// Set custom CA and/or mTLS client certificates.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls = tls;
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

    /// Set the transient RPC retry policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
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

    /// The endpoint as a validated HTTP/HTTPS base URL for Bazel HTTP remote
    /// caching (`/cas/<sha256>`, `/ac/<sha256>`).
    ///
    /// Preserves any configured path prefix and rejects `grpc://` / `grpcs://`
    /// schemes, embedded credentials, query strings, and fragments.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is empty, does not use `http://` or
    /// `https://`, or is not a valid URL.
    pub fn http_base_url(&self) -> Result<reqwest::Url> {
        let endpoint = self.endpoint.trim();
        if endpoint.is_empty() {
            return Err(Error::config("remote cache endpoint is empty"));
        }

        match endpoint.split_once("://") {
            Some(("http" | "https", _)) => {}
            Some((scheme, _)) => {
                return Err(Error::config(format!(
                    "unsupported HTTP remote cache scheme '{scheme}': use http:// or https://"
                )));
            }
            None => {
                return Err(Error::config(format!(
                    "remote cache endpoint '{endpoint}' has no scheme: \
                     prefix it with https:// (TLS) or http:// (plaintext)"
                )));
            }
        }

        let url = reqwest::Url::parse(endpoint).map_err(|e| {
            Error::config(format!(
                "invalid HTTP remote cache endpoint '{endpoint}': {e}"
            ))
        })?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::config(
                "HTTP remote cache endpoint must not embed credentials in the URL; use auth config",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Error::config(
                "HTTP remote cache endpoint must not contain a query string or fragment",
            ));
        }
        if url.host_str().is_none() {
            return Err(Error::config(format!(
                "HTTP remote cache endpoint '{endpoint}' has no host"
            )));
        }
        Ok(url)
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
        assert!(
            RemoteConfig::new("https://cache.example.com")
                .is_tls()
                .unwrap()
        );
    }

    #[test]
    fn a_bare_host_is_rejected_rather_than_guessed() {
        // Guessing plaintext would send a bearer token in the clear.
        let error = RemoteConfig::new("cache.example.com:443")
            .uri()
            .unwrap_err();
        assert!(error.to_string().contains("no scheme"), "{error}");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let error = RemoteConfig::new("ftp://cache.example.com")
            .uri()
            .unwrap_err();
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

        let multi = Credentials::Headers(vec![
            ("Authorization".into(), "Bearer secret-one".into()),
            ("x-nsc-ingress-auth".into(), "Bearer secret-two".into()),
        ]);
        let rendered = format!("{multi:?}");
        assert!(!rendered.contains("secret-one"));
        assert!(!rendered.contains("secret-two"));
        assert!(rendered.contains("Authorization"));
        assert!(rendered.contains("x-nsc-ingress-auth"));
    }

    #[test]
    fn config_debug_does_not_leak_credentials() {
        let config = RemoteConfig::new("grpcs://cache.example.com")
            .with_credentials(Credentials::Bearer("super-secret-token".into()))
            .with_tls(TlsConfig {
                ca_cert_pem: Some(b"ca-pem".to_vec()),
                client_cert_pem: Some(b"cert-pem".to_vec()),
                client_key_pem: Some(b"super-secret-private-key".to_vec()),
            });
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(!rendered.contains("super-secret-private-key"));
    }

    #[test]
    fn tls_config_rejects_unpaired_client_cert_or_key() {
        let cert_only = TlsConfig {
            ca_cert_pem: None,
            client_cert_pem: Some(b"cert".to_vec()),
            client_key_pem: None,
        };
        assert!(cert_only.validate().is_err());

        let key_only = TlsConfig {
            ca_cert_pem: None,
            client_cert_pem: None,
            client_key_pem: Some(b"key".to_vec()),
        };
        assert!(key_only.validate().is_err());

        let paired = TlsConfig {
            ca_cert_pem: None,
            client_cert_pem: Some(b"cert".to_vec()),
            client_key_pem: Some(b"key".to_vec()),
        };
        assert!(paired.validate().is_ok());
    }
}
