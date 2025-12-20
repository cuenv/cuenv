//! Resolved secrets with fingerprinting support

use crate::{SaltConfig, SecretError, SecretResolver, SecretSpec, compute_secret_fingerprint};
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
