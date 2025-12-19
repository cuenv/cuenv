//! Capabilities client for querying server features

use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::reapi::{
    GetCapabilitiesRequest, capabilities_client::CapabilitiesClient as ProtoCapabilitiesClient,
};
use std::sync::Arc;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, instrument};

/// Server capabilities information
#[derive(Debug, Clone)]
pub struct ServerCapabilities {
    /// Whether the server supports caching
    pub cache_capabilities: bool,
    /// Whether the server supports execution
    pub execution_capabilities: bool,
    /// Maximum batch size for blob operations
    pub max_batch_total_size_bytes: i64,
    /// Supported compressor types
    pub supported_compressors: Vec<i32>,
    /// Supported digest functions
    pub supported_digest_functions: Vec<i32>,
}

/// Client for REAPI Capabilities service
pub struct CapabilitiesClient {
    client: ProtoCapabilitiesClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
}

impl CapabilitiesClient {
    /// Create a new Capabilities client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client = ProtoCapabilitiesClient::with_interceptor(channel.channel(), interceptor);
        Self {
            client,
            config: Arc::new(config),
        }
    }

    /// Get server capabilities
    #[instrument(skip(self))]
    pub async fn get_capabilities(&self) -> Result<ServerCapabilities> {
        let request = GetCapabilitiesRequest {
            instance_name: self.config.instance_name.clone(),
        };

        let mut client = self.client.clone();

        let response = client
            .get_capabilities(request)
            .await
            .map_err(|e| RemoteError::grpc_error("GetCapabilities", e))?
            .into_inner();

        let cache_capabilities = response.cache_capabilities.is_some();
        let execution_capabilities = response.execution_capabilities.is_some();

        let max_batch_total_size_bytes = response
            .cache_capabilities
            .as_ref()
            .map(|c| c.max_batch_total_size_bytes)
            .unwrap_or(0);

        let supported_compressors = response
            .cache_capabilities
            .as_ref()
            .map(|c| c.supported_compressors.clone())
            .unwrap_or_default();

        let supported_digest_functions = response
            .cache_capabilities
            .as_ref()
            .map(|c| c.digest_functions.clone())
            .unwrap_or_default();

        debug!(
            cache = cache_capabilities,
            execution = execution_capabilities,
            max_batch = max_batch_total_size_bytes,
            "Retrieved server capabilities"
        );

        Ok(ServerCapabilities {
            cache_capabilities,
            execution_capabilities,
            max_batch_total_size_bytes,
            supported_compressors,
            supported_digest_functions,
        })
    }
}
