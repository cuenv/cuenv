//! Builder for REAPI Action

use crate::error::Result;
use crate::mapper::command::Command;
use crate::merkle::Digest;

/// Placeholder for REAPI Action proto
///
/// Phase 2: Replace with actual generated type from protos
#[derive(Debug, Clone)]
pub struct Action {
    pub command_digest: Digest,
    pub input_root_digest: Digest,
    pub timeout: Option<i64>,
    pub do_not_cache: bool,
}

/// Builder for constructing REAPI Actions
pub struct ActionBuilder;

impl ActionBuilder {
    /// Build an Action from a Command and input root digest
    ///
    /// Phase 3: Implement full Action construction:
    /// 1. Serialize Command to bytes
    /// 2. Upload Command to CAS, get digest
    /// 3. Build Action proto with command_digest and input_root_digest
    /// 4. Serialize Action to bytes
    /// 5. Compute Action digest
    pub fn build_action(
        _command: &Command,
        _input_root_digest: Digest,
        _timeout_secs: Option<u64>,
    ) -> Result<(Action, Digest)> {
        // TODO: Implement in Phase 3
        // For now, return placeholder
        let action = Action {
            command_digest: Digest::default(),
            input_root_digest: Digest::default(),
            timeout: None,
            do_not_cache: false,
        };

        let action_digest = Digest::default();

        Ok((action, action_digest))
    }
}
