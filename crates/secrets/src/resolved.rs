//! Resolved secrets with fingerprinting support

use crate::{
    SaltConfig, SecretError, SecretResolver, SecretSpec, compute_secret_fingerprint,
};
use std::collections::HashMap;

/// Resolved secrets ready for injection
#[derive(Debug, Clone, Default)]
pub struct ResolvedSecrets {
    /// Secret name -> resolved value
    pub values: HashMap<String, String>,
    /// Secret name -> HMAC fingerprint (for cache keys)
    pub fingerprints: HashMap<String, String>,
}

impl ResolvedSecrets {
    /// Create empty resolved secrets
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve secrets using a resolver with salt configuration
    ///
    /// # Arguments
    /// * `resolver` - The secret resolver to use
    /// * `secrets` - Map of secret names to their configuration
    /// * `salt_config` - Salt configuration for fingerprinting
    ///
    /// # Errors
    /// Returns error if a secret cannot be resolved or if salt is missing
    /// when secrets have `cache_key: true`
    pub async fn resolve<R: SecretResolver>(
        resolver: &R,
        secrets: &HashMap<String, SecretSpec>,
        salt_config: &SaltConfig,
    ) -> Result<Self, SecretError> {
        let mut values = HashMap::new();
        let mut fingerprints = HashMap::new();

        // Check if any secret requires cache key and salt is missing
        let needs_salt = secrets.values().any(|c| c.cache_key);
        if needs_salt && !salt_config.has_salt() {
            return Err(SecretError::MissingSalt);
        }

        for (name, spec) in secrets {
            let value = resolver.resolve(name, spec).await?;

            // Compute fingerprint if secret affects cache
            if spec.cache_key {
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
            values,
            fingerprints,
        })
    }

    /// Resolve secrets using batch resolution with a resolver.
    ///
    /// This is the preferred method for resolving multiple secrets efficiently.
    /// It uses the resolver's batch resolution method which may use native
    /// batch APIs (e.g., AWS `BatchGetSecretValue`, 1Password `Secrets.ResolveAll`).
    ///
    /// # Arguments
    /// * `resolver` - The secret resolver to use
    /// * `secrets` - Map of secret names to their configuration
    /// * `salt_config` - Salt configuration for fingerprinting
    ///
    /// # Errors
    /// Returns error if a secret cannot be resolved or if salt is missing
    /// when secrets have `cache_key: true`
    pub async fn resolve_batch<R: SecretResolver>(
        resolver: &R,
        secrets: &HashMap<String, SecretSpec>,
        salt_config: &SaltConfig,
    ) -> Result<Self, SecretError> {
        let batch = crate::batch::resolve_batch(resolver, secrets, salt_config).await?;
        Ok(batch.into_resolved_secrets())
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
    /// Returns a tuple of (`current_fingerprint`, `previous_fingerprint`) for cache validation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolved_secrets_new_is_empty() {
        let secrets = ResolvedSecrets::new();
        assert!(secrets.is_empty());
        assert!(secrets.values.is_empty());
        assert!(secrets.fingerprints.is_empty());
    }

    #[test]
    fn test_resolved_secrets_default_is_empty() {
        let secrets = ResolvedSecrets::default();
        assert!(secrets.is_empty());
    }

    #[test]
    fn test_resolved_secrets_get_existing() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("API_KEY".to_string(), "secret123".to_string());

        assert_eq!(secrets.get("API_KEY"), Some("secret123"));
        assert!(!secrets.is_empty());
    }

    #[test]
    fn test_resolved_secrets_get_missing() {
        let secrets = ResolvedSecrets::new();
        assert_eq!(secrets.get("NONEXISTENT"), None);
    }

    #[test]
    fn test_fingerprint_matches_with_current_salt() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("API_KEY".to_string(), "secret123".to_string());

        let salt_config = SaltConfig::new(Some("my-salt".to_string()));
        let fingerprint = compute_secret_fingerprint("API_KEY", "secret123", "my-salt");

        assert!(secrets.fingerprint_matches("API_KEY", &fingerprint, &salt_config));
    }

    #[test]
    fn test_fingerprint_matches_with_previous_salt() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("API_KEY".to_string(), "secret123".to_string());

        // Salt config with new salt but old fingerprint should still match
        let salt_config =
            SaltConfig::with_rotation(Some("new-salt".to_string()), Some("old-salt".to_string()));
        let old_fingerprint = compute_secret_fingerprint("API_KEY", "secret123", "old-salt");

        assert!(secrets.fingerprint_matches("API_KEY", &old_fingerprint, &salt_config));
    }

    #[test]
    fn test_fingerprint_matches_no_match() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("API_KEY".to_string(), "secret123".to_string());

        let salt_config = SaltConfig::new(Some("my-salt".to_string()));
        let wrong_fingerprint = compute_secret_fingerprint("API_KEY", "wrong-secret", "my-salt");

        assert!(!secrets.fingerprint_matches("API_KEY", &wrong_fingerprint, &salt_config));
    }

    #[test]
    fn test_fingerprint_matches_missing_secret() {
        let secrets = ResolvedSecrets::new();
        let salt_config = SaltConfig::new(Some("my-salt".to_string()));

        assert!(!secrets.fingerprint_matches("NONEXISTENT", "any-fingerprint", &salt_config));
    }

    #[test]
    fn test_fingerprint_matches_no_salt_configured() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("API_KEY".to_string(), "secret123".to_string());

        let salt_config = SaltConfig::default();

        // With no salt configured, no fingerprint should match
        assert!(!secrets.fingerprint_matches("API_KEY", "any-fingerprint", &salt_config));
    }

    #[test]
    fn test_compute_fingerprints_for_validation_both_salts() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("DB_PASS".to_string(), "password".to_string());

        let salt_config = SaltConfig::with_rotation(
            Some("current-salt".to_string()),
            Some("previous-salt".to_string()),
        );

        let (current_fp, previous_fp) =
            secrets.compute_fingerprints_for_validation("DB_PASS", &salt_config);

        assert!(current_fp.is_some());
        assert!(previous_fp.is_some());
        assert_ne!(current_fp, previous_fp);

        // Verify fingerprints are correct
        let expected_current = compute_secret_fingerprint("DB_PASS", "password", "current-salt");
        let expected_previous = compute_secret_fingerprint("DB_PASS", "password", "previous-salt");
        assert_eq!(current_fp.unwrap(), expected_current);
        assert_eq!(previous_fp.unwrap(), expected_previous);
    }

    #[test]
    fn test_compute_fingerprints_for_validation_only_current() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("TOKEN".to_string(), "abc123".to_string());

        let salt_config = SaltConfig::new(Some("only-current".to_string()));

        let (current_fp, previous_fp) =
            secrets.compute_fingerprints_for_validation("TOKEN", &salt_config);

        assert!(current_fp.is_some());
        assert!(previous_fp.is_none());
    }

    #[test]
    fn test_compute_fingerprints_for_validation_only_previous() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("TOKEN".to_string(), "abc123".to_string());

        let salt_config = SaltConfig::with_rotation(None, Some("only-previous".to_string()));

        let (current_fp, previous_fp) =
            secrets.compute_fingerprints_for_validation("TOKEN", &salt_config);

        assert!(current_fp.is_none());
        assert!(previous_fp.is_some());
    }

    #[test]
    fn test_compute_fingerprints_for_validation_missing_secret() {
        let secrets = ResolvedSecrets::new();
        let salt_config = SaltConfig::new(Some("salt".to_string()));

        let (current_fp, previous_fp) =
            secrets.compute_fingerprints_for_validation("MISSING", &salt_config);

        assert!(current_fp.is_none());
        assert!(previous_fp.is_none());
    }

    #[test]
    fn test_compute_fingerprints_for_validation_no_salt() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("KEY".to_string(), "value".to_string());

        let salt_config = SaltConfig::default();

        let (current_fp, previous_fp) =
            secrets.compute_fingerprints_for_validation("KEY", &salt_config);

        assert!(current_fp.is_none());
        assert!(previous_fp.is_none());
    }

    #[test]
    fn test_resolved_secrets_clone() {
        let mut secrets = ResolvedSecrets::new();
        secrets.values.insert("K1".to_string(), "V1".to_string());
        secrets
            .fingerprints
            .insert("K1".to_string(), "FP1".to_string());

        let cloned = secrets.clone();
        assert_eq!(cloned.values.get("K1"), Some(&"V1".to_string()));
        assert_eq!(cloned.fingerprints.get("K1"), Some(&"FP1".to_string()));
    }

    #[test]
    fn test_resolved_secrets_debug() {
        let secrets = ResolvedSecrets::new();
        let debug = format!("{secrets:?}");
        assert!(debug.contains("ResolvedSecrets"));
    }

    #[test]
    fn test_multiple_secrets() {
        let mut secrets = ResolvedSecrets::new();
        secrets
            .values
            .insert("KEY1".to_string(), "value1".to_string());
        secrets
            .values
            .insert("KEY2".to_string(), "value2".to_string());
        secrets
            .values
            .insert("KEY3".to_string(), "value3".to_string());

        assert_eq!(secrets.values.len(), 3);
        assert!(!secrets.is_empty());
        assert_eq!(secrets.get("KEY1"), Some("value1"));
        assert_eq!(secrets.get("KEY2"), Some("value2"));
        assert_eq!(secrets.get("KEY3"), Some("value3"));
    }
}
