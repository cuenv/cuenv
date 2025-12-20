//! Runtime digest computation for cache keys
//!
//! Computes content-addressable digests for task execution based on:
//! - Input file hashes
//! - Command and arguments
//! - Environment variables
//! - Runtime configuration (flake.lock, output path)
//! - Secret fingerprints (salted HMAC)

use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Runtime digest builder for cache key computation
pub struct DigestBuilder {
    hasher: Sha256,
}

impl DigestBuilder {
    /// Create a new digest builder
    #[must_use]
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    /// Add command to digest
    pub fn add_command(&mut self, command: &[String]) -> &mut Self {
        for arg in command {
            self.hasher.update(arg.as_bytes());
            self.hasher.update([0u8]); // separator
        }
        self
    }

    /// Add environment variables to digest (sorted by key for determinism)
    pub fn add_env(&mut self, env: &HashMap<String, String>) -> &mut Self {
        let mut sorted: Vec<_> = env.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);

        for (key, value) in sorted {
            self.hasher.update(key.as_bytes());
            self.hasher.update([b'=']);
            self.hasher.update(value.as_bytes());
            self.hasher.update([0u8]); // separator
        }
        self
    }

    /// Add input file patterns to digest
    pub fn add_inputs(&mut self, inputs: &[String]) -> &mut Self {
        for input in inputs {
            self.hasher.update(input.as_bytes());
            self.hasher.update([0u8]); // separator
        }
        self
    }

    /// Add runtime configuration to digest
    pub fn add_runtime(&mut self, flake: &str, output: &str, system: &str) -> &mut Self {
        self.hasher.update(flake.as_bytes());
        self.hasher.update([0u8]);
        self.hasher.update(output.as_bytes());
        self.hasher.update([0u8]);
        self.hasher.update(system.as_bytes());
        self.hasher.update([0u8]);
        self
    }

    /// Add secret fingerprints to digest (HMAC-SHA256 with system salt)
    ///
    /// # Arguments
    /// * `secrets` - Map of secret names to their values
    /// * `salt` - System-wide salt for HMAC computation
    pub fn add_secret_fingerprints(
        &mut self,
        secrets: &HashMap<String, String>,
        salt: &str,
    ) -> &mut Self {
        let mut sorted: Vec<_> = secrets.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);

        for (key, value) in sorted {
            // Compute HMAC-SHA256(key + value, salt)
            let mut hmac = Sha256::new();
            hmac.update(salt.as_bytes());
            hmac.update(key.as_bytes());
            hmac.update(value.as_bytes());
            let fingerprint = hmac.finalize();

            // Add fingerprint to overall digest
            self.hasher.update(fingerprint);
        }
        self
    }

    /// Add a UUID for impure flake inputs (forces cache miss)
    pub fn add_impurity_uuid(&mut self, uuid: &str) -> &mut Self {
        self.hasher.update(b"IMPURE:");
        self.hasher.update(uuid.as_bytes());
        self.hasher.update([0u8]);
        self
    }

    /// Finalize and return hex-encoded digest
    #[must_use]
    pub fn finalize(self) -> String {
        let result = self.hasher.finalize();
        format!("sha256:{}", hex::encode(result))
    }
}

impl Default for DigestBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute task runtime digest
#[must_use]
pub fn compute_task_digest(
    command: &[String],
    env: &HashMap<String, String>,
    inputs: &[String],
    runtime_digest: Option<&str>,
    secret_fingerprints: Option<&HashMap<String, String>>,
    system_salt: Option<&str>,
) -> String {
    let mut builder = DigestBuilder::new();

    builder.add_command(command);
    builder.add_env(env);
    builder.add_inputs(inputs);

    if let Some(runtime) = runtime_digest {
        builder.hasher.update(runtime.as_bytes());
    }

    if let Some(secrets) = secret_fingerprints
        && let Some(salt) = system_salt
    {
        builder.add_secret_fingerprints(secrets, salt);
    }

    builder.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_deterministic() {
        let command = vec!["cargo".to_string(), "build".to_string()];
        let env = HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]);
        let inputs = vec!["src/**/*.rs".to_string()];

        let digest1 = compute_task_digest(&command, &env, &inputs, None, None, None);
        let digest2 = compute_task_digest(&command, &env, &inputs, None, None, None);

        assert_eq!(digest1, digest2);
        assert!(digest1.starts_with("sha256:"));
    }

    #[test]
    fn test_digest_changes_with_command() {
        let env = HashMap::new();
        let inputs = vec![];

        let digest1 =
            compute_task_digest(&vec!["echo".to_string()], &env, &inputs, None, None, None);
        let digest2 = compute_task_digest(&vec!["ls".to_string()], &env, &inputs, None, None, None);

        assert_ne!(digest1, digest2);
    }

    #[test]
    fn test_digest_changes_with_env() {
        let command = vec!["echo".to_string()];
        let inputs = vec![];

        let env1 = HashMap::from([("KEY".to_string(), "value1".to_string())]);
        let env2 = HashMap::from([("KEY".to_string(), "value2".to_string())]);

        let digest1 = compute_task_digest(&command, &env1, &inputs, None, None, None);
        let digest2 = compute_task_digest(&command, &env2, &inputs, None, None, None);

        assert_ne!(digest1, digest2);
    }

    #[test]
    fn test_digest_env_order_independent() {
        let command = vec!["echo".to_string()];
        let inputs = vec![];

        let mut env1 = HashMap::new();
        env1.insert("A".to_string(), "1".to_string());
        env1.insert("B".to_string(), "2".to_string());

        let mut env2 = HashMap::new();
        env2.insert("B".to_string(), "2".to_string());
        env2.insert("A".to_string(), "1".to_string());

        let digest1 = compute_task_digest(&command, &env1, &inputs, None, None, None);
        let digest2 = compute_task_digest(&command, &env2, &inputs, None, None, None);

        assert_eq!(digest1, digest2);
    }

    #[test]
    fn test_secret_fingerprints() {
        let command = vec!["deploy".to_string()];
        let env = HashMap::new();
        let inputs = vec![];

        let secrets = HashMap::from([("API_KEY".to_string(), "secret123".to_string())]);
        let salt = "system-wide-salt";

        let digest1 =
            compute_task_digest(&command, &env, &inputs, None, Some(&secrets), Some(salt));

        // Change secret value
        let secrets2 = HashMap::from([("API_KEY".to_string(), "secret456".to_string())]);

        let digest2 =
            compute_task_digest(&command, &env, &inputs, None, Some(&secrets2), Some(salt));

        // Digests should differ when secret changes
        assert_ne!(digest1, digest2);
    }

    #[test]
    fn test_secret_fingerprints_deterministic() {
        let command = vec!["deploy".to_string()];
        let env = HashMap::new();
        let inputs = vec![];

        let secrets = HashMap::from([("API_KEY".to_string(), "secret123".to_string())]);
        let salt = "system-wide-salt";

        let digest1 =
            compute_task_digest(&command, &env, &inputs, None, Some(&secrets), Some(salt));
        let digest2 =
            compute_task_digest(&command, &env, &inputs, None, Some(&secrets), Some(salt));

        assert_eq!(digest1, digest2);
    }

    #[test]
    fn test_impurity_uuid() {
        let mut builder = DigestBuilder::new();
        builder.add_command(&vec!["echo".to_string()]);
        builder.add_impurity_uuid("550e8400-e29b-41d4-a716-446655440000");
        let digest1 = builder.finalize();

        let mut builder = DigestBuilder::new();
        builder.add_command(&vec!["echo".to_string()]);
        builder.add_impurity_uuid("550e8400-e29b-41d4-a716-446655440001");
        let digest2 = builder.finalize();

        assert_ne!(digest1, digest2);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Property: Same inputs always produce the same digest
        #[test]
        fn digest_is_deterministic(
            cmd in prop::collection::vec("[a-z]+", 1..5),
            key in "[A-Z_]+",
            value in "[a-zA-Z0-9]+",
        ) {
            let env = HashMap::from([(key.clone(), value.clone())]);
            let inputs: Vec<String> = vec![];

            let digest1 = compute_task_digest(&cmd, &env, &inputs, None, None, None);
            let digest2 = compute_task_digest(&cmd, &env, &inputs, None, None, None);

            prop_assert_eq!(digest1, digest2);
        }

        /// Property: Different commands produce different digests
        #[test]
        fn different_commands_produce_different_digests(
            cmd1 in "[a-z]+",
            cmd2 in "[a-z]+",
        ) {
            prop_assume!(cmd1 != cmd2);

            let env = HashMap::new();
            let inputs: Vec<String> = vec![];

            let digest1 = compute_task_digest(&vec![cmd1], &env, &inputs, None, None, None);
            let digest2 = compute_task_digest(&vec![cmd2], &env, &inputs, None, None, None);

            prop_assert_ne!(digest1, digest2);
        }

        /// Property: Different env values produce different digests
        #[test]
        fn different_env_values_produce_different_digests(
            key in "[A-Z]+",
            value1 in "[a-z]+",
            value2 in "[a-z]+",
        ) {
            prop_assume!(value1 != value2);

            let cmd = vec!["test".to_string()];
            let env1 = HashMap::from([(key.clone(), value1)]);
            let env2 = HashMap::from([(key, value2)]);
            let inputs: Vec<String> = vec![];

            let digest1 = compute_task_digest(&cmd, &env1, &inputs, None, None, None);
            let digest2 = compute_task_digest(&cmd, &env2, &inputs, None, None, None);

            prop_assert_ne!(digest1, digest2);
        }

        /// Property: Env order doesn't matter (digest is order-independent)
        #[test]
        fn env_order_is_irrelevant(
            pairs in prop::collection::vec(("[A-Z]+", "[a-z]+"), 2..5),
        ) {
            let cmd = vec!["test".to_string()];
            let inputs: Vec<String> = vec![];

            // Create env in original order
            let env1: HashMap<String, String> = pairs.iter()
                .cloned()
                .collect();

            // Create env in reverse order
            let env2: HashMap<String, String> = pairs.iter()
                .rev()
                .cloned()
                .collect();

            let digest1 = compute_task_digest(&cmd, &env1, &inputs, None, None, None);
            let digest2 = compute_task_digest(&cmd, &env2, &inputs, None, None, None);

            prop_assert_eq!(digest1, digest2);
        }

        /// Property: Digests always have the sha256: prefix
        #[test]
        fn digest_has_correct_format(
            cmd in prop::collection::vec("[a-z]+", 1..3),
        ) {
            let env = HashMap::new();
            let inputs: Vec<String> = vec![];

            let digest = compute_task_digest(&cmd, &env, &inputs, None, None, None);

            prop_assert!(digest.starts_with("sha256:"));
            // SHA256 produces 64 hex characters
            prop_assert_eq!(digest.len(), 7 + 64); // "sha256:" + 64 hex chars
        }
    }
}
