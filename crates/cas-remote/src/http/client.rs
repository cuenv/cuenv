//! HTTP client plumbing shared by [`HttpCas`](super::HttpCas) and
//! [`HttpActionCache`](super::HttpActionCache).

use crate::client::is_valid_header_value;
use crate::config::{Credentials, RemoteConfig, RemoteProtocol};
use crate::error::{Error, Result};
use cuenv_cas::Digest;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url, redirect};

/// Maximum requests made in one same-origin redirect chain.
const MAX_REDIRECT_REQUESTS: usize = 5;

/// A configured HTTP client for a Bazel HTTP/REST remote cache.
#[derive(Clone, Debug)]
pub struct HttpRemoteClient {
    http: Client,
    streaming_uploads: Option<Client>,
    base_url: Url,
    config: RemoteConfig,
    auth_header: Option<(HeaderName, HeaderValue)>,
}

impl HttpRemoteClient {
    /// Build a new HTTP remote cache client from `config`.
    ///
    /// Enforces that `config.protocol` is [`RemoteProtocol::Http`], validates
    /// the base URL and credentials, and installs a same-origin redirect policy
    /// so credentials can never leak to another origin.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the endpoint, instance name, or credentials
    /// are invalid, or [`Error::Connect`] if the TLS/HTTP client cannot be
    /// initialized.
    pub fn new(config: RemoteConfig) -> Result<Self> {
        if config.protocol != RemoteProtocol::Http {
            return Err(Error::config(
                "HttpRemoteClient requires RemoteProtocol::Http",
            ));
        }

        let _ = rustls::crypto::ring::default_provider().install_default();

        let base_url = build_base_url(&config)?;
        let auth_header = build_auth_header(&config.credentials)?;

        let allowed_origin = Origin::from_url(&base_url);
        let redirect_policy = redirect::Policy::custom(move |attempt| {
            match validate_redirect(attempt.url(), &allowed_origin, attempt.previous().len()) {
                Ok(()) => attempt.follow(),
                Err(reason) => attempt.error(reason),
            }
        });

        let http = build_http_client(&config, redirect_policy)?;
        // reqwest can replay buffered bodies, but streamed bodies must be
        // rebuilt by the caller if a redirect keeps the request method.
        let streaming_uploads = config
            .writable
            .then(|| build_http_client(&config, redirect::Policy::none()))
            .transpose()?;

        Ok(Self {
            http,
            streaming_uploads,
            base_url,
            config,
            auth_header,
        })
    }

    /// The configuration this client was built with.
    #[must_use]
    pub fn config(&self) -> &RemoteConfig {
        &self.config
    }

    /// The normalized base URL (including any configured path prefix and
    /// instance segment, without a trailing slash).
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Whether this client is allowed to write.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.config.writable
    }

    /// Refuse a write on a read-only client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ReadOnly`] when writes are not permitted.
    pub fn ensure_writable(&self, operation: &'static str) -> Result<()> {
        if self.config.writable {
            return Ok(());
        }
        Err(Error::ReadOnly { operation })
    }

    /// Build the URL for a CAS blob (`/cas/<sha256>`).
    ///
    /// # Errors
    ///
    /// Returns an error if `digest` is not a valid canonical SHA-256 digest.
    pub fn cas_url(&self, digest: &Digest) -> Result<Url> {
        digest.validate()?;
        self.object_url("cas", &digest.hash)
    }

    /// Build the URL for an ActionCache entry (`/ac/<sha256>`).
    ///
    /// # Errors
    ///
    /// Returns an error if `action_digest` is not a valid canonical SHA-256
    /// digest.
    pub fn ac_url(&self, action_digest: &Digest) -> Result<Url> {
        action_digest.validate()?;
        self.object_url("ac", &action_digest.hash)
    }

    fn object_url(&self, namespace: &str, hash: &str) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| Error::config("HTTP remote cache URL cannot be a base"))?
            .pop_if_empty()
            .push(namespace)
            .push(hash);
        Ok(url)
    }

    /// Construct a request with configured credentials attached.
    pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.authenticated_request(&self.http, method, url)
    }

    pub(super) fn streaming_put(&self, url: Url) -> Result<RequestBuilder> {
        let http = self.streaming_uploads.as_ref().ok_or(Error::ReadOnly {
            operation: "upload a CAS file",
        })?;
        Ok(self.authenticated_request(http, Method::PUT, url))
    }

    pub(super) fn put_redirect_target(
        &self,
        response: &Response,
        requests_sent: usize,
    ) -> Result<Option<Url>> {
        if !matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            return Ok(None);
        }
        let target = response
            .headers()
            .get(LOCATION)
            .and_then(|location| location.to_str().ok())
            .and_then(|location| response.url().join(location).ok())
            .ok_or_else(|| Error::protocol("HTTP.PUT.cas", "invalid redirect location"))?;
        validate_redirect(&target, &Origin::from_url(&self.base_url), requests_sent)
            .map_err(|reason| Error::protocol("HTTP.PUT.cas", reason))?;
        Ok(Some(target))
    }

    fn authenticated_request(&self, http: &Client, method: Method, url: Url) -> RequestBuilder {
        let mut builder = http.request(method, url);
        if let Some((name, value)) = &self.auth_header {
            let mut headers = HeaderMap::with_capacity(1);
            headers.insert(name.clone(), value.clone());
            builder = builder.headers(headers);
        }
        builder
    }

    /// Probe whether the HTTP remote cache endpoint is reachable and accepts
    /// our credentials.
    ///
    /// Issues a `HEAD /cas/<empty-sha256>` request. Both `200 OK` and
    /// `404 Not Found` indicate a healthy, reachable cache; `401` / `403` and
    /// connection failures return errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the server is unreachable, rejects credentials, or
    /// responds with an unexpected HTTP status.
    pub async fn check_health(&self) -> Result<()> {
        let empty = Digest::of_bytes(b"");
        let url = self.cas_url(&empty)?;
        let response = self
            .request(Method::HEAD, url)
            .send()
            .await
            .map_err(|e| Error::connect(&self.config.endpoint, format_reqwest_error(&e)))?;

        match response.status() {
            StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => Err(Error::Rpc {
                operation: "HTTP.HealthCheck",
                message: format!("authentication rejected ({status})"),
            }),
            status => Err(Error::Rpc {
                operation: "HTTP.HealthCheck",
                message: format!("unexpected HTTP status {status}"),
            }),
        }
    }
}

fn build_http_client(config: &RemoteConfig, policy: redirect::Policy) -> Result<Client> {
    Client::builder()
        .timeout(config.timeout)
        .redirect(policy)
        .build()
        .map_err(|e| Error::connect(&config.endpoint, e.to_string()))
}

fn validate_redirect(
    target: &Url,
    allowed_origin: &Origin,
    requests_sent: usize,
) -> std::result::Result<(), &'static str> {
    if requests_sent >= MAX_REDIRECT_REQUESTS {
        return Err("too many HTTP redirects from remote cache");
    }
    if &Origin::from_url(target) != allowed_origin {
        return Err("remote cache attempted a cross-origin HTTP redirect");
    }
    if !target.username().is_empty() || target.password().is_some() {
        return Err("remote cache redirected to a URL containing credentials");
    }
    Ok(())
}

/// Render a `reqwest::Error` together with its underlying cause chain so
/// policy rejections (such as cross-origin redirects) remain visible.
pub(crate) fn format_reqwest_error(err: &reqwest::Error) -> String {
    let mut out = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: Option<String>,
    port: Option<u16>,
}

impl Origin {
    fn from_url(url: &Url) -> Self {
        Self {
            scheme: url.scheme().to_ascii_lowercase(),
            host: url.host_str().map(str::to_ascii_lowercase),
            port: url.port_or_known_default(),
        }
    }
}

fn build_base_url(config: &RemoteConfig) -> Result<Url> {
    let mut url = config.http_base_url()?;
    let instance = config.instance_name.trim().trim_matches('/');
    if !instance.is_empty() {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| Error::config("HTTP remote cache URL cannot be a base"))?;
        segments.pop_if_empty();
        for segment in instance.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(Error::config(format!(
                    "invalid remote cache instance name '{}': path traversal or empty segments are not allowed",
                    config.instance_name
                )));
            }
            segments.push(segment);
        }
    }
    Ok(url)
}

fn build_auth_header(credentials: &Credentials) -> Result<Option<(HeaderName, HeaderValue)>> {
    let (name, value) = match credentials {
        Credentials::None => return Ok(None),
        Credentials::Bearer(token) => ("authorization", format!("Bearer {token}")),
        Credentials::Header { name, value } => (name.as_str(), value.clone()),
    };

    let header_name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| Error::config(format!("invalid credential header name '{name}'")))?;

    if !is_valid_header_value(&value) {
        return Err(Error::config(format!(
            "credential value for header '{name}' contains characters that are not \
             printable ASCII; check for a stray newline or a smart quote in the token"
        )));
    }

    let mut header_value = HeaderValue::from_str(&value).map_err(|_| {
        Error::config(format!(
            "credential value for header '{name}' is not a valid header value"
        ))
    })?;
    header_value.set_sensitive(true);
    Ok(Some((header_name, header_value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_origin_redirects_share_one_request_limit()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("https://cache.example.test/cas/digest")?;
        let origin = Origin::from_url(&url);
        assert!(validate_redirect(&url, &origin, MAX_REDIRECT_REQUESTS - 1).is_ok());
        assert_eq!(
            validate_redirect(&url, &origin, MAX_REDIRECT_REQUESTS),
            Err("too many HTTP redirects from remote cache")
        );
        Ok(())
    }

    #[test]
    fn cas_and_ac_urls_preserve_prefix_and_instance() {
        let config = RemoteConfig::new("https://cache.example.com/bazel/v1/")
            .with_protocol(RemoteProtocol::Http)
            .with_instance_name("team/ci");
        let client = HttpRemoteClient::new(config).unwrap();
        let digest = Digest::of_bytes(b"hello");

        assert_eq!(
            client.cas_url(&digest).unwrap().as_str(),
            format!(
                "https://cache.example.com/bazel/v1/team/ci/cas/{}",
                digest.hash
            )
        );
        assert_eq!(
            client.ac_url(&digest).unwrap().as_str(),
            format!(
                "https://cache.example.com/bazel/v1/team/ci/ac/{}",
                digest.hash
            )
        );
    }

    #[test]
    fn http_client_rejects_grpc_scheme_and_traversal_instance() {
        let grpc =
            RemoteConfig::new("grpcs://cache.example.com:443").with_protocol(RemoteProtocol::Http);
        assert!(HttpRemoteClient::new(grpc).is_err());

        let traversal = RemoteConfig::new("https://cache.example.com")
            .with_protocol(RemoteProtocol::Http)
            .with_instance_name("../escape");
        assert!(HttpRemoteClient::new(traversal).is_err());
    }

    #[test]
    fn http_client_rejects_non_ascii_credentials_without_leaking_secret() {
        let config = RemoteConfig::new("https://cache.example.com")
            .with_protocol(RemoteProtocol::Http)
            .with_credentials(Credentials::Bearer("tøken-secret".into()));
        let err = HttpRemoteClient::new(config).unwrap_err();
        assert!(!err.to_string().contains("tøken-secret"));
    }
}
