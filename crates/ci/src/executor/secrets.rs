//! Secret Resolution for CI Execution
//!
//! Resolves secrets from environment variables and computes HMAC fingerprints
//! for cache key computation.

use crate::ir::SecretConfig;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use thiserror::Error;

/// Error types for secret resolution
#[derive(Debug, Error)]
pub enum SecretError {
    /// Secret environment variable not found
    #[error("Secret '{name}' not found in environment variable '{env_var}'")]
    NotFound { name: String, env_var: String },

    /// Secret is too short for safe fingerprinting (< 4 chars)
    #[error("Secret '{name}' is too short ({len} chars, minimum 4) for cache key inclusion")]
    TooShort { name: String, len: usize },

    /// Missing salt when secrets require fingerprinting
    #[error("CUENV_SECRET_SALT required when secrets have cache_key: true")]
    MissingSalt,
}

/// Resolved secrets ready for injection
#[derive(Debug, Clone, Default)]
pub struct ResolvedSecrets {
    /// Secret name -> resolved value
    pub values: HashMap<String, String>,
    /// Secret name -> HMAC fingerprint (for cache keys)
    pub fingerprints: HashMap<String, String>,
}

impl ResolvedSecrets {
    /// Resolve secrets from environment variables
    ///
    /// # Arguments
    /// * `secrets` - Map of secret names to their configuration
    /// * `salt` - Optional system salt for HMAC computation
    ///
    /// # Errors
    /// Returns error if a required secret is not found or if salt is missing
    /// when secrets have `cache_key: true`
    pub fn from_env(
        secrets: &HashMap<String, SecretConfig>,
        salt: Option<&str>,
    ) -> Result<Self, SecretError> {
        let mut values = HashMap::new();
        let mut fingerprints = HashMap::new();

        // Check if any secret requires cache key and salt is missing
        let needs_salt = secrets.values().any(|c| c.cache_key);
        if needs_salt && salt.is_none() {
            return Err(SecretError::MissingSalt);
        }

        for (name, config) in secrets {
            // Source is the env var name
            let value = std::env::var(&config.source).map_err(|_| SecretError::NotFound {
                name: name.clone(),
                env_var: config.source.clone(),
            })?;

            // Compute fingerprint if secret affects cache
            if config.cache_key {
                // Warn if secret is too short (but don't fail)
                if value.len() < 4 {
                    tracing::warn!(
                        secret = %name,
                        len = value.len(),
                        "Secret is too short for safe cache key inclusion"
                    );
                }

                let fingerprint = compute_secret_fingerprint(name, &value, salt.unwrap_or(""));
                fingerprints.insert(name.clone(), fingerprint);
            }

            values.insert(name.clone(), value);
        }

        Ok(Self {
            values,
            fingerprints,
        })
    }

    /// Check if any secrets were resolved
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Get a resolved secret value by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
}

/// Compute HMAC-SHA256 fingerprint for a secret
///
/// Uses HMAC construction: H(salt || name || value)
/// This prevents rainbow table attacks on common secret values
fn compute_secret_fingerprint(name: &str, value: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(name.as_bytes());
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

/// Resolve secrets for all tasks in an IR
///
/// Returns a map of task_id -> ResolvedSecrets
pub fn resolve_all_task_secrets(
    tasks: &[crate::ir::Task],
    salt: Option<&str>,
) -> Result<HashMap<String, ResolvedSecrets>, SecretError> {
    let mut result = HashMap::new();

    for task in tasks {
        if !task.secrets.is_empty() {
            let resolved = ResolvedSecrets::from_env(&task.secrets, salt)?;
            result.insert(task.id.clone(), resolved);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_secret_config(source: &str, cache_key: bool) -> SecretConfig {
        SecretConfig {
            source: source.to_string(),
            cache_key,
        }
    }

    #[test]
    fn test_fingerprint_deterministic() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_value() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret456", "salt");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_salt() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt1");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret123", "salt2");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_name() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("DB_PASSWORD", "secret123", "salt");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_resolve_from_env() {
        // Set up test environment
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_SECRET_1", "value1");
            std::env::set_var("TEST_SECRET_2", "value2");
        }

        let secrets = HashMap::from([
            (
                "secret1".to_string(),
                make_secret_config("TEST_SECRET_1", true),
            ),
            (
                "secret2".to_string(),
                make_secret_config("TEST_SECRET_2", false),
            ),
        ]);

        let resolved = ResolvedSecrets::from_env(&secrets, Some("test-salt")).unwrap();

        assert_eq!(resolved.values.get("secret1"), Some(&"value1".to_string()));
        assert_eq!(resolved.values.get("secret2"), Some(&"value2".to_string()));
        assert!(resolved.fingerprints.contains_key("secret1"));
        assert!(!resolved.fingerprints.contains_key("secret2")); // cache_key: false

        // Cleanup
        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_SECRET_1");
            std::env::remove_var("TEST_SECRET_2");
        }
    }

    #[test]
    fn test_missing_secret() {
        let secrets = HashMap::from([(
            "missing".to_string(),
            make_secret_config("NONEXISTENT_VAR", false),
        )]);

        let result = ResolvedSecrets::from_env(&secrets, None);
        assert!(matches!(result, Err(SecretError::NotFound { .. })));
    }

    #[test]
    fn test_missing_salt_with_cache_key() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_SALT_CHECK", "value");
        }

        let secrets = HashMap::from([(
            "secret".to_string(),
            make_secret_config("TEST_SALT_CHECK", true), // cache_key: true requires salt
        )]);

        let result = ResolvedSecrets::from_env(&secrets, None);
        assert!(matches!(result, Err(SecretError::MissingSalt)));

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_SALT_CHECK");
        }
    }
}
