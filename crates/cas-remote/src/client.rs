//! Connection and request plumbing shared by the remote stores.

use crate::config::{Credentials, RemoteConfig};
use crate::error::{Error, Result};
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::build::bazel::remote::execution::v2::capabilities_client::CapabilitiesClient;
use tonic::metadata::{MetadataKey, MetadataValue};
use tonic::transport::{Channel, ClientTlsConfig};

/// A connected REAPI channel plus the settings every request needs.
#[derive(Clone, Debug)]
pub struct RemoteClient {
    channel: Channel,
    config: RemoteConfig,
}

impl RemoteClient {
    /// Dial the configured endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is malformed or the connection
    /// cannot be established.
    pub async fn connect(config: RemoteConfig) -> Result<Self> {
        let uri = config.uri()?;
        let mut endpoint = Channel::builder(uri).timeout(config.timeout);
        if config.is_tls()? {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|e| Error::connect(&config.endpoint, e.to_string()))?;
        }
        let channel = endpoint
            .connect()
            .await
            .map_err(|e| Error::connect(&config.endpoint, e.to_string()))?;
        Ok(Self { channel, config })
    }

    /// Build a client around an already-connected channel.
    ///
    /// Used by tests that serve REAPI in-process, and by callers that want to
    /// manage the channel themselves.
    #[must_use]
    pub fn from_channel(channel: Channel, config: RemoteConfig) -> Self {
        Self { channel, config }
    }

    /// The underlying channel.
    #[must_use]
    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }

    /// The configuration this client was built with.
    #[must_use]
    pub fn config(&self) -> &RemoteConfig {
        &self.config
    }

    /// The REAPI instance name to put on every request.
    #[must_use]
    pub fn instance_name(&self) -> String {
        self.config.instance_name.clone()
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

    /// Wrap a message in a request carrying this client's credentials.
    ///
    /// # Errors
    ///
    /// Returns an error if a configured header name or value is not valid
    /// ASCII, which gRPC metadata requires.
    pub fn request<T>(&self, message: T) -> Result<tonic::Request<T>> {
        let mut request = tonic::Request::new(message);
        let (name, value) = match &self.config.credentials {
            Credentials::None => return Ok(request),
            Credentials::Bearer(token) => ("authorization", format!("Bearer {token}")),
            Credentials::Header { name, value } => (name.as_str(), value.clone()),
        };

        let key: MetadataKey<tonic::metadata::Ascii> = name
            .parse()
            .map_err(|_| Error::config(format!("invalid credential header name '{name}'")))?;

        // tonic will happily put non-ASCII bytes on the wire — HTTP/2 permits
        // them — and the server then rejects the request with an opaque
        // authentication failure. Catching it here names the real problem.
        // The value is a secret, so no error message quotes it.
        if !is_valid_header_value(&value) {
            return Err(Error::config(format!(
                "credential value for header '{name}' contains characters that are not \
                 printable ASCII; check for a stray newline or a smart quote in the token"
            )));
        }

        let value: MetadataValue<tonic::metadata::Ascii> = value.parse().map_err(|_| {
            Error::config(format!(
                "credential value for header '{name}' is not a valid header value"
            ))
        })?;
        request.metadata_mut().insert(key, value);
        Ok(request)
    }

    /// Ask the server what it supports, and refuse it if cuenv cannot use it.
    ///
    /// cuenv digests with SHA-256 and nothing else, so a server that does not
    /// speak SHA-256 would silently never produce a hit. Failing the
    /// handshake makes that a configuration error instead of a permanent
    /// mystery miss.
    ///
    /// Returns the server's maximum batch payload size, or `None` when it
    /// does not advertise one.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails or the server is incompatible.
    pub async fn check_capabilities(&self) -> Result<Option<i64>> {
        let mut client = CapabilitiesClient::new(self.channel());
        let request = self.request(pb::GetCapabilitiesRequest {
            instance_name: self.instance_name(),
        })?;
        let response = client
            .get_capabilities(request)
            .await
            .map_err(|status| Error::rpc("GetCapabilities", &status))?
            .into_inner();

        let Some(cache) = response.cache_capabilities else {
            return Err(Error::incompatible(
                "server advertises no cache capabilities",
            ));
        };

        let sha256 = i32::from(pb::digest_function::Value::Sha256);
        if !cache.digest_functions.is_empty() && !cache.digest_functions.contains(&sha256) {
            return Err(Error::incompatible(format!(
                "server does not offer SHA-256 digests (advertises {:?}); \
                 cuenv computes SHA-256 digests only",
                cache.digest_functions
            )));
        }

        Ok(Some(cache.max_batch_total_size_bytes).filter(|size| *size > 0))
    }
}

/// Whether every byte is printable ASCII, the safe subset for a header value.
///
/// Horizontal tab is permitted by the HTTP grammar but has no business in a
/// credential, so it is excluded too.
fn is_valid_header_value(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(config: RemoteConfig) -> RemoteClient {
        // A channel that is never dialled: these tests only exercise request
        // construction, which does no I/O.
        let channel = Channel::builder("http://127.0.0.1:1".parse().unwrap()).connect_lazy();
        RemoteClient::from_channel(channel, config)
    }

    #[tokio::test]
    async fn bearer_credentials_become_an_authorization_header() {
        let client = client(
            RemoteConfig::new("grpc://x:1").with_credentials(Credentials::Bearer("tok".into())),
        );
        let request = client.request(()).unwrap();
        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer tok"
        );
    }

    #[tokio::test]
    async fn custom_header_credentials_are_sent_verbatim() {
        let client = client(RemoteConfig::new("grpc://x:1").with_credentials(
            Credentials::Header {
                name: "x-api-key".into(),
                value: "tok".into(),
            },
        ));
        let request = client.request(()).unwrap();
        assert_eq!(request.metadata().get("x-api-key").unwrap(), "tok");
    }

    #[tokio::test]
    async fn no_credentials_sends_no_metadata() {
        let client = client(RemoteConfig::new("grpc://x:1"));
        let request = client.request(()).unwrap();
        assert!(request.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn a_non_ascii_credential_is_rejected_without_quoting_it() {
        let client = client(
            RemoteConfig::new("grpc://x:1").with_credentials(Credentials::Bearer("tøken".into())),
        );
        let error = client.request(()).unwrap_err();
        assert!(!error.to_string().contains("tøken"), "{error}");
        assert!(error.to_string().contains("printable ASCII"), "{error}");
    }

    #[tokio::test]
    async fn a_credential_with_a_trailing_newline_is_rejected() {
        // Reading a token from a file is the usual way this happens.
        let client = client(
            RemoteConfig::new("grpc://x:1").with_credentials(Credentials::Bearer("tok\n".into())),
        );
        assert!(client.request(()).is_err());
    }

    #[test]
    fn header_value_validation() {
        assert!(is_valid_header_value("Bearer abc.DEF-123_~"));
        assert!(!is_valid_header_value(""));
        assert!(!is_valid_header_value("has\nnewline"));
        assert!(!is_valid_header_value("has\ttab"));
        assert!(!is_valid_header_value("nøn-ascii"));
    }

    #[tokio::test]
    async fn read_only_clients_refuse_writes() {
        let client = client(RemoteConfig::new("grpc://x:1"));
        assert!(!client.is_writable());
        assert!(client.ensure_writable("upload").is_err());
    }

    #[tokio::test]
    async fn writable_clients_allow_writes() {
        let client = client(RemoteConfig::new("grpc://x:1").writable());
        assert!(client.ensure_writable("upload").is_ok());
    }
}
