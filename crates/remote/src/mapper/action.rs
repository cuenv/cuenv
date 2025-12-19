//! Builder for REAPI Action

use crate::error::Result;
use crate::mapper::command::MappedCommand;
use crate::merkle::Digest;
use crate::reapi::Action as ReapiAction;
use prost::Message;
use prost_types::Duration;

/// Result of building an action
#[derive(Debug)]
pub struct MappedAction {
    /// The REAPI Action proto
    pub action: ReapiAction,
    /// Serialized bytes of the action
    pub action_bytes: Vec<u8>,
    /// Digest of the serialized action
    pub action_digest: Digest,
}

/// Builder for constructing REAPI Actions
pub struct ActionBuilder;

impl ActionBuilder {
    /// Build an Action from a MappedCommand and input root digest
    ///
    /// The Action references the command digest (command must be uploaded to CAS first)
    /// and the input root digest (directory tree must be uploaded to CAS first).
    pub fn build_action(
        mapped_command: &MappedCommand,
        input_root_digest: &Digest,
        timeout_secs: Option<u64>,
    ) -> Result<MappedAction> {
        // Build timeout duration
        let timeout = timeout_secs.map(|secs| Duration {
            seconds: secs as i64,
            nanos: 0,
        });

        // Build the Action proto
        let action = ReapiAction {
            command_digest: Some(digest_to_proto(&mapped_command.command_digest)),
            input_root_digest: Some(digest_to_proto(input_root_digest)),
            timeout,
            do_not_cache: false,
            salt: Vec::new(),
            platform: None, // Platform is in Command, not Action
        };

        // Serialize and compute digest
        let action_bytes = action.encode_to_vec();
        let action_digest = Digest::from_bytes(&action_bytes);

        Ok(MappedAction {
            action,
            action_bytes,
            action_digest,
        })
    }
}

/// Convert our Digest to proto Digest
fn digest_to_proto(digest: &Digest) -> crate::reapi::Digest {
    crate::reapi::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretsMode;
    use crate::mapper::CommandMapper;
    use cuenv_core::environment::Environment;
    use cuenv_core::tasks::Task;

    #[test]
    fn test_build_action() {
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            ..Default::default()
        };

        let env = Environment::default();
        let mapped_command = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();
        let input_root = Digest::from_bytes(b"root");

        let mapped_action = ActionBuilder::build_action(&mapped_command, &input_root, Some(60)).unwrap();

        // Verify action references correct digests
        assert_eq!(
            mapped_action.action.command_digest.as_ref().unwrap().hash,
            mapped_command.command_digest.hash
        );
        assert_eq!(
            mapped_action.action.input_root_digest.as_ref().unwrap().hash,
            input_root.hash
        );

        // Verify timeout
        assert_eq!(mapped_action.action.timeout.as_ref().unwrap().seconds, 60);

        // Verify action digest is computed
        assert_eq!(mapped_action.action_digest.hash.len(), 64);
    }

    #[test]
    fn test_action_deterministic() {
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            ..Default::default()
        };

        let env = Environment::default();
        let mapped_command = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();
        let input_root = Digest::from_bytes(b"root");

        let action1 = ActionBuilder::build_action(&mapped_command, &input_root, Some(60)).unwrap();
        let action2 = ActionBuilder::build_action(&mapped_command, &input_root, Some(60)).unwrap();

        // Same inputs should produce same action digest
        assert_eq!(action1.action_digest.hash, action2.action_digest.hash);
    }
}
