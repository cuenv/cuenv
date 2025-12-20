//! Secret Resolution for CI Execution
//!
//! Thin wrapper over `cuenv_secrets` providing CI-specific helpers for
//! resolving secrets from environment variables with HMAC fingerprinting.

use crate::ir::SecretConfig;
use std::collections::HashMap;

// Re-export core types from cuenv-secrets
pub use cuenv_secrets::{
    compute_secret_fingerprint, ResolvedSecrets, SaltConfig, SecretError,
};

/// CI-specific resolved secrets with convenience methods for IR types
#[derive(Debug, Clone, Default)]
pub struct CIResolvedSecrets {
    inner: ResolvedSecrets,
}

impl CIResolvedSecrets {
    /// Resolve secrets from environment variables using CI IR types
    ///
    /// # Arguments
    /// * `secrets` - Map of secret names to their CI configuration
    /// * `salt` - Optional system salt for HMAC computation
    ///
    /// # Errors
    /// Returns error if a required secret is not found or if salt is missing
    /// when secrets have `cache_key: true`
    pub fn from_env(
        secrets: &HashMap<String, SecretConfig>,
        salt: Option<&str>,
    ) -> Result<Self, SecretError> {
        let salt_config = SaltConfig::new(salt.map(String::from));
        Self::from_env_with_salt_config(secrets, &salt_config)
    }

    /// Resolve secrets with salt rotation support using CI IR types
    pub fn from_env_with_salt_config(
        secrets: &HashMap<String, SecretConfig>,
        salt_config: &SaltConfig,
    ) -> Result<Self, SecretError> {
        // Since we're in a sync context, resolve manually from env vars
        let mut values = HashMap::new();
        let mut fingerprints = HashMap::new();

        // Check if any secret requires cache key and salt is missing
        let needs_salt = secrets.values().any(|c| c.cache_key);
        if needs_salt && !salt_config.has_salt() {
            return Err(SecretError::MissingSalt);
        }

        for (name, config) in secrets {
            // Source is the env var name
            let value = std::env::var(&config.source).map_err(|_| SecretError::NotFound {
                name: name.clone(),
                secret_source: config.source.clone(),
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

                // Use write_salt for computing fingerprints (current salt preferred)
                let fingerprint = compute_secret_fingerprint(
                    name,
                    &value,
                    salt_config.write_salt().unwrap_or(""),
                );
                fingerprints.insert(name.clone(), fingerprint);
            }

            values.insert(name.clone(), value);
        }

        Ok(Self {
            inner: ResolvedSecrets {
                values,
                fingerprints,
            },
        })
    }

    /// Check if any secrets were resolved
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get a resolved secret value by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner.get(name)
    }

    /// Get the inner values map
    #[must_use]
    pub fn values(&self) -> &HashMap<String, String> {
        &self.inner.values
    }

    /// Get the inner fingerprints map
    #[must_use]
    pub fn fingerprints(&self) -> &HashMap<String, String> {
        &self.inner.fingerprints
    }

    /// Check if a cached fingerprint matches with salt rotation support
    #[must_use]
    pub fn fingerprint_matches(
        &self,
        name: &str,
        cached_fingerprint: &str,
        salt_config: &SaltConfig,
    ) -> bool {
        self.inner
            .fingerprint_matches(name, cached_fingerprint, salt_config)
    }

    /// Compute fingerprints using both current and previous salts
    #[must_use]
    pub fn compute_fingerprints_for_validation(
        &self,
        name: &str,
        salt_config: &SaltConfig,
    ) -> (Option<String>, Option<String>) {
        self.inner
            .compute_fingerprints_for_validation(name, salt_config)
    }
}

impl std::ops::Deref for CIResolvedSecrets {
    type Target = ResolvedSecrets;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Resolve secrets for all tasks in an IR
///
/// Returns a map of task_id -> CIResolvedSecrets
pub fn resolve_all_task_secrets(
    tasks: &[crate::ir::Task],
    salt: Option<&str>,
) -> Result<HashMap<String, CIResolvedSecrets>, SecretError> {
    let mut result = HashMap::new();

    for task in tasks {
        if !task.secrets.is_empty() {
            let resolved = CIResolvedSecrets::from_env(&task.secrets, salt)?;
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
        temp_env::with_vars(
            [
                ("TEST_SECRET_1", Some("value1")),
                ("TEST_SECRET_2", Some("value2")),
            ],
            || {
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

                let resolved = CIResolvedSecrets::from_env(&secrets, Some("test-salt")).unwrap();

                assert_eq!(resolved.values().get("secret1"), Some(&"value1".to_string()));
                assert_eq!(resolved.values().get("secret2"), Some(&"value2".to_string()));
                assert!(resolved.fingerprints().contains_key("secret1"));
                assert!(!resolved.fingerprints().contains_key("secret2")); // cache_key: false
            },
        );
    }

    #[test]
    fn test_missing_secret() {
        let secrets = HashMap::from([(
            "missing".to_string(),
            make_secret_config("NONEXISTENT_VAR", false),
        )]);

        let result = CIResolvedSecrets::from_env(&secrets, None);
        assert!(matches!(result, Err(SecretError::NotFound { .. })));
    }

    #[test]
    fn test_missing_salt_with_cache_key() {
        temp_env::with_var("TEST_SALT_CHECK", Some("value"), || {
            let secrets = HashMap::from([(
                "secret".to_string(),
                make_secret_config("TEST_SALT_CHECK", true),
            )]);

            let result = CIResolvedSecrets::from_env(&secrets, None);
            assert!(matches!(result, Err(SecretError::MissingSalt)));
        });
    }

    #[test]
    fn test_salt_config_new() {
        let config = SaltConfig::new(Some("current".to_string()));
        assert_eq!(config.current, Some("current".to_string()));
        assert_eq!(config.previous, None);
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("current"));
    }

    #[test]
    fn test_salt_config_with_rotation() {
        let config =
            SaltConfig::with_rotation(Some("new-salt".to_string()), Some("old-salt".to_string()));
        assert_eq!(config.current, Some("new-salt".to_string()));
        assert_eq!(config.previous, Some("old-salt".to_string()));
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("new-salt"));
    }

    #[test]
    fn test_fingerprint_matches_current_salt() {
        temp_env::with_var("TEST_FP_MATCH_1", Some("secret_value"), || {
            let secrets = HashMap::from([(
                "api_key".to_string(),
                make_secret_config("TEST_FP_MATCH_1", true),
            )]);

            let salt_config = SaltConfig::with_rotation(
                Some("current-salt".to_string()),
                Some("old-salt".to_string()),
            );

            let resolved =
                CIResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

            let cached_fp = compute_secret_fingerprint("api_key", "secret_value", "current-salt");
            assert!(resolved.fingerprint_matches("api_key", &cached_fp, &salt_config));
        });
    }

    #[test]
    fn test_fingerprint_matches_previous_salt() {
        temp_env::with_var("TEST_FP_MATCH_2", Some("secret_value"), || {
            let secrets = HashMap::from([(
                "api_key".to_string(),
                make_secret_config("TEST_FP_MATCH_2", true),
            )]);

            let salt_config = SaltConfig::with_rotation(
                Some("new-salt".to_string()),
                Some("old-salt".to_string()),
            );

            let resolved =
                CIResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

            let cached_fp = compute_secret_fingerprint("api_key", "secret_value", "old-salt");
            assert!(resolved.fingerprint_matches("api_key", &cached_fp, &salt_config));
        });
    }
}
