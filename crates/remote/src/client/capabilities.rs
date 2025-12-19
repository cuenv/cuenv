//! Capabilities client for querying server features

use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};

/// Server capabilities information
///
/// Phase 2: Replace with actual generated type from protos
#[derive(Debug, Clone)]
pub struct ServerCapabilities {
    pub cache_capabilities: bool,
    pub execution_capabilities: bool,
    pub max_batch_total_size_bytes: i64,
}

/// Client for REAPI Capabilities service
///
/// Phase 2: This will wrap the actual gRPC client generated from protos
pub struct CapabilitiesClient {
    #[allow(dead_code)]
    config: RemoteConfig,
}

impl CapabilitiesClient {
    /// Create a new Capabilities client
    pub fn new(config: RemoteConfig) -> Self {
        Self { config }
    }

    /// Get server capabilities
    ///
    /// Phase 2: Implement using GetCapabilities RPC
    pub async fn get_capabilities(&self) -> Result<ServerCapabilities> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "Capabilities client not yet implemented (Phase 2)",
        ))
    }
}
