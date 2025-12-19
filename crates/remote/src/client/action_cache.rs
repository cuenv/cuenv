//! ActionCache client for caching execution results

use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;

/// Placeholder for REAPI ActionResult
///
/// Phase 2: Replace with actual generated type from protos
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub exit_code: i32,
    pub stdout_digest: Option<Digest>,
    pub stderr_digest: Option<Digest>,
}

/// Client for REAPI ActionCache service
///
/// Phase 2: This will wrap the actual gRPC client generated from protos
pub struct ActionCacheClient {
    #[allow(dead_code)]
    config: RemoteConfig,
}

impl ActionCacheClient {
    /// Create a new ActionCache client
    pub fn new(config: RemoteConfig) -> Self {
        Self { config }
    }

    /// Get a cached action result
    ///
    /// Phase 2: Implement using GetActionResult RPC
    pub async fn get_action_result(&self, _action_digest: &Digest) -> Result<Option<ActionResult>> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "ActionCache client not yet implemented (Phase 2)",
        ))
    }

    /// Update the cached action result
    ///
    /// Phase 2: Implement using UpdateActionResult RPC
    pub async fn update_action_result(
        &self,
        _action_digest: &Digest,
        _result: ActionResult,
    ) -> Result<()> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "ActionCache client not yet implemented (Phase 2)",
        ))
    }
}
