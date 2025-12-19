//! Execution client for remote task execution

use crate::client::action_cache::ActionResult;
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;

/// Client for REAPI Execution service
///
/// Phase 2: This will wrap the actual gRPC client generated from protos
pub struct ExecutionClient {
    #[allow(dead_code)]
    config: RemoteConfig,
}

impl ExecutionClient {
    /// Create a new Execution client
    pub fn new(config: RemoteConfig) -> Self {
        Self { config }
    }

    /// Execute an action remotely
    ///
    /// Phase 2: Implement using Execute RPC (streaming)
    /// This will:
    /// 1. Send ExecuteRequest with action_digest
    /// 2. Stream Operation updates
    /// 3. Wait for completion
    /// 4. Extract ActionResult from final Operation
    pub async fn execute(&self, _action_digest: &Digest) -> Result<ActionResult> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "Execution client not yet implemented (Phase 2)",
        ))
    }

    /// Wait for an operation to complete
    ///
    /// Phase 2: Implement using WaitExecution RPC
    pub async fn wait_execution(&self, _operation_name: &str) -> Result<ActionResult> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "Execution client not yet implemented (Phase 2)",
        ))
    }
}
