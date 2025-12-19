//! gRPC channel management with BuildBuddy-specific configuration

use crate::config::{AuthConfig, RemoteConfig};
use crate::error::{RemoteError, Result};
use std::sync::Arc;
use std::time::Duration;
use tonic::metadata::{AsciiMetadataValue, MetadataMap};
use tonic::service::Interceptor;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, info};

/// Shared gRPC channel for all REAPI services
#[derive(Clone)]
pub struct GrpcChannel {
    inner: Channel,
    config: Arc<RemoteConfig>,
}

impl GrpcChannel {
    /// Create a new gRPC channel from configuration
    pub async fn connect(config: &RemoteConfig) -> Result<Self> {
        let endpoint = create_endpoint(config)?;

        info!(endpoint = %config.endpoint, "Connecting to REAPI server");

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| RemoteError::connection_failed(&config.endpoint, e.to_string()))?;

        debug!("Successfully connected to REAPI server");

        Ok(Self {
            inner: channel,
            config: Arc::new(config.clone()),
        })
    }

    /// Get the raw channel for creating service clients
    pub fn channel(&self) -> Channel {
        self.inner.clone()
    }

    /// Create an interceptor with auth headers for this channel
    pub fn auth_interceptor(&self) -> AuthInterceptor {
        AuthInterceptor::new(&self.config)
    }

    /// Get the instance name for requests
    pub fn instance_name(&self) -> &str {
        &self.config.instance_name
    }
}

/// The authentication mode for requests
#[derive(Clone)]
enum AuthMode {
    /// Bearer token authentication (Authorization: Bearer <token>)
    Bearer(AsciiMetadataValue),
    /// BuildBuddy API key (x-buildbuddy-api-key: <token>)
    BuildBuddyApiKey(AsciiMetadataValue),
    /// No authentication
    None,
}

/// Interceptor that adds authentication and BuildBuddy headers to requests
#[derive(Clone)]
pub struct AuthInterceptor {
    auth_mode: AuthMode,
    instance_name: String,
}

impl AuthInterceptor {
    /// Create a new auth interceptor from config
    pub fn new(config: &RemoteConfig) -> Self {
        let auth_mode = config
            .auth
            .as_ref()
            .map(|auth| match auth {
                AuthConfig::Bearer { token } => {
                    let value = format!("Bearer {}", token);
                    match AsciiMetadataValue::try_from(&value) {
                        Ok(v) => AuthMode::Bearer(v),
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "Bearer token contains invalid characters, proceeding without auth"
                            );
                            AuthMode::None
                        }
                    }
                }
                AuthConfig::BuildBuddy { api_key } => {
                    match AsciiMetadataValue::try_from(api_key) {
                        Ok(v) => AuthMode::BuildBuddyApiKey(v),
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "BuildBuddy API key contains invalid characters, proceeding without auth"
                            );
                            AuthMode::None
                        }
                    }
                }
                AuthConfig::MTls { .. } => AuthMode::None, // mTLS is handled at channel level
                AuthConfig::GoogleCloud => {
                    // GoogleCloud auth is not yet implemented - log error and continue without auth
                    // This allows testing connection but will likely fail with permission errors
                    tracing::error!(
                        "GoogleCloud authentication is not yet implemented. \
                         Use 'bearer' or 'buildbuddy' auth instead. \
                         Proceeding without authentication."
                    );
                    AuthMode::None
                }
            })
            .unwrap_or(AuthMode::None);

        Self {
            auth_mode,
            instance_name: config.instance_name.clone(),
        }
    }

    /// Apply auth headers to a metadata map
    pub fn apply_to_metadata(&self, metadata: &mut MetadataMap) {
        match &self.auth_mode {
            AuthMode::Bearer(header) => {
                metadata.insert("authorization", header.clone());
            }
            AuthMode::BuildBuddyApiKey(key) => {
                metadata.insert("x-buildbuddy-api-key", key.clone());
            }
            AuthMode::None => {}
        }

        // BuildBuddy-specific: add platform header for routing
        if let Ok(value) = AsciiMetadataValue::try_from(&self.instance_name) {
            metadata.insert("x-buildbuddy-platform", value);
        }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> std::result::Result<tonic::Request<()>, tonic::Status> {
        self.apply_to_metadata(request.metadata_mut());
        Ok(request)
    }
}

/// Create a tonic Endpoint from RemoteConfig
fn create_endpoint(config: &RemoteConfig) -> Result<Endpoint> {
    let endpoint_str = &config.endpoint;

    // Normalize endpoint URL
    let uri = if endpoint_str.starts_with("grpcs://") {
        endpoint_str.replace("grpcs://", "https://")
    } else if endpoint_str.starts_with("grpc://") {
        endpoint_str.replace("grpc://", "http://")
    } else if !endpoint_str.starts_with("http://") && !endpoint_str.starts_with("https://") {
        // Default to https for bare hostnames
        format!("https://{}", endpoint_str)
    } else {
        endpoint_str.clone()
    };

    debug!(original = %endpoint_str, normalized = %uri, "Normalizing endpoint URL");

    let mut endpoint = Endpoint::from_shared(uri.clone()).map_err(|e| {
        RemoteError::config_error(format!("Invalid endpoint '{}': {}", endpoint_str, e))
    })?;

    // Configure timeouts
    endpoint = endpoint
        .timeout(Duration::from_secs(config.timeout_secs))
        .connect_timeout(Duration::from_secs(30));

    // Configure TLS for HTTPS endpoints
    if uri.starts_with("https://") {
        let tls = ClientTlsConfig::new().with_native_roots();
        endpoint = endpoint
            .tls_config(tls)
            .map_err(|e| RemoteError::config_error(format!("TLS configuration error: {}", e)))?;
    }

    // Configure keep-alive for long-running connections
    endpoint = endpoint
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true);

    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoint_normalization_grpcs() {
        let config = RemoteConfig {
            endpoint: "grpcs://remote.buildbuddy.io".to_string(),
            ..Default::default()
        };
        let endpoint = create_endpoint(&config);
        assert!(endpoint.is_ok());
    }

    #[test]
    fn test_endpoint_normalization_grpc() {
        let config = RemoteConfig {
            endpoint: "grpc://localhost:8980".to_string(),
            ..Default::default()
        };
        let endpoint = create_endpoint(&config);
        assert!(endpoint.is_ok());
    }

    #[test]
    fn test_endpoint_normalization_bare() {
        let config = RemoteConfig {
            endpoint: "remote.buildbuddy.io:443".to_string(),
            ..Default::default()
        };
        let endpoint = create_endpoint(&config);
        assert!(endpoint.is_ok());
    }

    #[test]
    fn test_auth_interceptor_bearer() {
        let config = RemoteConfig {
            endpoint: "grpc://localhost:8980".to_string(),
            auth: Some(AuthConfig::Bearer {
                token: "test-token".to_string(),
            }),
            ..Default::default()
        };

        let interceptor = AuthInterceptor::new(&config);
        assert!(matches!(interceptor.auth_mode, AuthMode::Bearer(_)));
    }

    #[test]
    fn test_auth_interceptor_buildbuddy() {
        let config = RemoteConfig {
            endpoint: "grpc://localhost:8980".to_string(),
            auth: Some(AuthConfig::BuildBuddy {
                api_key: "test-api-key".to_string(),
            }),
            ..Default::default()
        };

        let interceptor = AuthInterceptor::new(&config);
        assert!(matches!(
            interceptor.auth_mode,
            AuthMode::BuildBuddyApiKey(_)
        ));
    }

    #[test]
    fn test_auth_interceptor_no_auth() {
        let config = RemoteConfig {
            endpoint: "grpc://localhost:8980".to_string(),
            auth: None,
            ..Default::default()
        };

        let interceptor = AuthInterceptor::new(&config);
        assert!(matches!(interceptor.auth_mode, AuthMode::None));
    }
}
