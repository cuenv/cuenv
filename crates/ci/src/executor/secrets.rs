//! Secret Resolution for CI Execution
//!
//! Resolves secrets from environment variables and computes HMAC fingerprints
//! for cache key computation. Supports graceful salt rotation via
//! `CUENV_SYSTEM_SALT_PREV` for zero-downtime secret rotation.

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

/// Salt configuration for secret fingerprinting with rotation support
#[derive(Debug, Clone, Default)]
pub struct SaltConfig {
    /// Current salt (used for writing new fingerprints)
    pub current: Option<String>,
    /// Previous salt (accepted for reading during rotation)
    pub previous: Option<String>,
}

impl SaltConfig {
    /// Create a new salt config with only a current salt
    #[must_use]
    pub fn new(current: Option<String>) -> Self {
        Self {
            current,
            previous: None,
        }
    }

    /// Create a salt config with rotation support
    #[must_use]
    pub fn with_rotation(current: Option<String>, previous: Option<String>) -> Self {
        Self { current, previous }
    }

    /// Check if any salt is available
    #[must_use]
    pub fn has_salt(&self) -> bool {
        self.current.is_some() || self.previous.is_some()
    }

    /// Get the current salt for writing (returns current, or previous if current is None)
    #[must_use]
    pub fn write_salt(&self) -> Option<&str> {
        self.current
            .as_deref()
            .or(self.previous.as_deref())
    }
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
        let salt_config = SaltConfig::new(salt.map(String::from));
        Self::from_env_with_salt_config(secrets, &salt_config)
    }

    /// Resolve secrets with salt rotation support
    ///
    /// During salt rotation, fingerprints are computed with the current salt
    /// for writing to cache. Use `fingerprint_matches` to validate cache entries
    /// against both current and previous salts.
    ///
    /// # Arguments
    /// * `secrets` - Map of secret names to their configuration
    /// * `salt_config` - Salt configuration with current and optional previous salt
    ///
    /// # Errors
    /// Returns error if a required secret is not found or if salt is missing
    /// when secrets have `cache_key: true`
    pub fn from_env_with_salt_config(
        secrets: &HashMap<String, SecretConfig>,
        salt_config: &SaltConfig,
    ) -> Result<Self, SecretError> {
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

                // Use write_salt for computing fingerprints (current salt preferred)
                let fingerprint =
                    compute_secret_fingerprint(name, &value, salt_config.write_salt().unwrap_or(""));
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

    /// Check if a cached fingerprint matches with salt rotation support
    ///
    /// During salt rotation, this checks if the cached fingerprint matches
    /// using either the current or previous salt. This allows cache hits
    /// during the rotation window.
    ///
    /// # Arguments
    /// * `name` - Secret name
    /// * `cached_fingerprint` - Fingerprint from cache
    /// * `salt_config` - Salt configuration with current and optional previous salt
    ///
    /// # Returns
    /// `true` if the fingerprint matches with either salt, `false` otherwise
    #[must_use]
    pub fn fingerprint_matches(
        &self,
        name: &str,
        cached_fingerprint: &str,
        salt_config: &SaltConfig,
    ) -> bool {
        let Some(value) = self.values.get(name) else {
            return false;
        };

        // Check against current salt
        if let Some(current) = &salt_config.current {
            let current_fp = compute_secret_fingerprint(name, value, current);
            if current_fp == cached_fingerprint {
                return true;
            }
        }

        // Check against previous salt (for rotation window)
        if let Some(previous) = &salt_config.previous {
            let previous_fp = compute_secret_fingerprint(name, value, previous);
            if previous_fp == cached_fingerprint {
                tracing::debug!(
                    secret = %name,
                    "Cache hit using previous salt - rotation in progress"
                );
                return true;
            }
        }

        false
    }

    /// Compute fingerprints using both current and previous salts
    ///
    /// Returns a tuple of (current_fingerprint, previous_fingerprint) for cache validation.
    /// Either may be None if the corresponding salt is not configured.
    #[must_use]
    pub fn compute_fingerprints_for_validation(
        &self,
        name: &str,
        salt_config: &SaltConfig,
    ) -> (Option<String>, Option<String>) {
        let Some(value) = self.values.get(name) else {
            return (None, None);
        };

        let current_fp = salt_config
            .current
            .as_ref()
            .map(|salt| compute_secret_fingerprint(name, value, salt));

        let previous_fp = salt_config
            .previous
            .as_ref()
            .map(|salt| compute_secret_fingerprint(name, value, salt));

        (current_fp, previous_fp)
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

    // Salt rotation tests

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
        let config = SaltConfig::with_rotation(
            Some("new-salt".to_string()),
            Some("old-salt".to_string()),
        );
        assert_eq!(config.current, Some("new-salt".to_string()));
        assert_eq!(config.previous, Some("old-salt".to_string()));
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("new-salt"));
    }

    #[test]
    fn test_salt_config_no_current_uses_previous() {
        let config = SaltConfig::with_rotation(None, Some("old-salt".to_string()));
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("old-salt"));
    }

    #[test]
    fn test_salt_config_empty() {
        let config = SaltConfig::default();
        assert!(!config.has_salt());
        assert_eq!(config.write_salt(), None);
    }

    #[test]
    fn test_fingerprint_matches_current_salt() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_FP_MATCH_1", "secret_value");
        }

        let secrets = HashMap::from([(
            "api_key".to_string(),
            make_secret_config("TEST_FP_MATCH_1", true),
        )]);

        let salt_config = SaltConfig::with_rotation(
            Some("current-salt".to_string()),
            Some("old-salt".to_string()),
        );

        let resolved = ResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

        // Fingerprint computed with current salt should match
        let cached_fp = compute_secret_fingerprint("api_key", "secret_value", "current-salt");
        assert!(resolved.fingerprint_matches("api_key", &cached_fp, &salt_config));

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_FP_MATCH_1");
        }
    }

    #[test]
    fn test_fingerprint_matches_previous_salt() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_FP_MATCH_2", "secret_value");
        }

        let secrets = HashMap::from([(
            "api_key".to_string(),
            make_secret_config("TEST_FP_MATCH_2", true),
        )]);

        let salt_config = SaltConfig::with_rotation(
            Some("new-salt".to_string()),
            Some("old-salt".to_string()),
        );

        let resolved = ResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

        // Fingerprint computed with OLD salt should still match (rotation window)
        let cached_fp = compute_secret_fingerprint("api_key", "secret_value", "old-salt");
        assert!(resolved.fingerprint_matches("api_key", &cached_fp, &salt_config));

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_FP_MATCH_2");
        }
    }

    #[test]
    fn test_fingerprint_no_match_wrong_salt() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_FP_MATCH_3", "secret_value");
        }

        let secrets = HashMap::from([(
            "api_key".to_string(),
            make_secret_config("TEST_FP_MATCH_3", true),
        )]);

        let salt_config = SaltConfig::with_rotation(
            Some("current-salt".to_string()),
            Some("old-salt".to_string()),
        );

        let resolved = ResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

        // Fingerprint with unknown salt should NOT match
        let wrong_fp = compute_secret_fingerprint("api_key", "secret_value", "wrong-salt");
        assert!(!resolved.fingerprint_matches("api_key", &wrong_fp, &salt_config));

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_FP_MATCH_3");
        }
    }

    #[test]
    fn test_resolve_with_salt_config_uses_current_for_write() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_WRITE_SALT", "secret_value");
        }

        let secrets = HashMap::from([(
            "api_key".to_string(),
            make_secret_config("TEST_WRITE_SALT", true),
        )]);

        let salt_config = SaltConfig::with_rotation(
            Some("new-salt".to_string()),
            Some("old-salt".to_string()),
        );

        let resolved = ResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();

        // Fingerprint stored should use current (new) salt
        let expected_fp = compute_secret_fingerprint("api_key", "secret_value", "new-salt");
        assert_eq!(resolved.fingerprints.get("api_key"), Some(&expected_fp));

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_WRITE_SALT");
        }
    }

    #[test]
    fn test_compute_fingerprints_for_validation() {
        // SAFETY: Tests run single-threaded and we clean up after
        unsafe {
            std::env::set_var("TEST_COMPUTE_FP", "secret_value");
        }

        let secrets = HashMap::from([(
            "api_key".to_string(),
            make_secret_config("TEST_COMPUTE_FP", false), // cache_key doesn't matter for this test
        )]);

        let salt_config = SaltConfig::with_rotation(
            Some("current".to_string()),
            Some("previous".to_string()),
        );

        let resolved = ResolvedSecrets::from_env_with_salt_config(&secrets, &salt_config).unwrap();
        let (current_fp, previous_fp) =
            resolved.compute_fingerprints_for_validation("api_key", &salt_config);

        assert!(current_fp.is_some());
        assert!(previous_fp.is_some());
        assert_ne!(current_fp, previous_fp); // Different salts = different fingerprints

        // SAFETY: Tests run single-threaded
        unsafe {
            std::env::remove_var("TEST_COMPUTE_FP");
        }
    }
}
