use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use crate::merkle::digest::Digest;
use crate::RemoteError;

pub struct ActionBuilder;

impl ActionBuilder {
    pub fn build_action(
        command_digest: Digest,
        input_root_digest: Digest,
        timeout: Option<std::time::Duration>,
        do_not_cache: bool,
    ) -> Result<reapi::Action, RemoteError> {
        Ok(reapi::Action {
            command_digest: Some(reapi::Digest {
                hash: command_digest.hash,
                size_bytes: command_digest.size_bytes,
            }),
            input_root_digest: Some(reapi::Digest {
                hash: input_root_digest.hash,
                size_bytes: input_root_digest.size_bytes,
            }),
            timeout: timeout.map(|d| prost_types::Duration {
                seconds: d.as_secs() as i64,
                nanos: d.subsec_nanos() as i32,
            }),
            do_not_cache,
            platform: None, 
            salt: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_build_action() {
        let cmd = Digest::new("cmd".to_string(), 10);
        let root = Digest::new("root".to_string(), 20);
        
        let action = ActionBuilder::build_action(cmd, root, Some(Duration::from_secs(60)), false).unwrap();
        
        assert_eq!(action.command_digest.unwrap().hash, "cmd");
        assert_eq!(action.input_root_digest.unwrap().hash, "root");
        assert_eq!(action.timeout.unwrap().seconds, 60);
        assert!(!action.do_not_cache);
    }
}