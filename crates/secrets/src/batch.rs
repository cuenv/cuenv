//! Batch secret resolution with concurrent provider execution
//!
//! This module provides efficient batch resolution of secrets across multiple
//! providers with:
//! - Concurrent resolution across different providers
//! - Provider-specific batch APIs where available (AWS, 1Password)
//! - Automatic fingerprinting for cache key inclusion
//! - Secure memory handling via [`SecureSecret`]

use crate::{
    BatchSecrets, SaltConfig, SecretError, SecretResolver, SecretSpec, compute_secret_fingerprint,
};
use futures::future::try_join_all;
use std::collections::HashMap;

/// Configuration for batch resolution.
#[derive(Debug, Clone, Default)]
pub struct BatchConfig {
    /// Salt configuration for fingerprinting secrets with `cache_key: true`.
    pub salt_config: SaltConfig,
}

impl BatchConfig {
    /// Create a new batch config with the given salt configuration.
    #[must_use]
    pub fn new(salt_config: SaltConfig) -> Self {
        Self { salt_config }
    }

    /// Create a batch config from an optional salt string.
    #[must_use]
    pub fn from_salt(salt: Option<String>) -> Self {
        Self {
            salt_config: SaltConfig::new(salt),
        }
    }
}

/// Multi-provider batch resolver.
///
/// Groups secrets by provider type and resolves concurrently across providers,
/// while using each provider's optimal batch strategy internally.
///
/// # Example
///
/// ```ignore
/// use cuenv_secrets::{BatchResolver, BatchConfig, SaltConfig};
///
/// let config = BatchConfig::new(SaltConfig::new(Some("my-salt".to_string())));
/// let mut resolver = BatchResolver::new(config);
///
/// // Register resolvers
/// resolver.add_resolver(&env_resolver);
/// resolver.add_resolver(&aws_resolver);
///
/// // Resolve all secrets
/// let secrets = resolver.resolve_all(&secret_specs).await?;
/// ```
pub struct BatchResolver<'a> {
    /// Provider name -> resolver
    resolvers: HashMap<&'static str, &'a dyn SecretResolver>,
    /// Configuration for batch resolution
    config: BatchConfig,
}

impl<'a> BatchResolver<'a> {
    /// Create a new batch resolver with the given configuration.
    #[must_use]
    pub fn new(config: BatchConfig) -> Self {
        Self {
            resolvers: HashMap::new(),
            config,
        }
    }

    /// Register a resolver for its provider.
    ///
    /// The resolver's [`provider_name`](SecretResolver::provider_name) is used
    /// as the key for grouping secrets.
    pub fn add_resolver(&mut self, resolver: &'a dyn SecretResolver) {
        self.resolvers.insert(resolver.provider_name(), resolver);
    }

    /// Get the number of registered resolvers.
    #[must_use]
    pub fn resolver_count(&self) -> usize {
        self.resolvers.len()
    }

    /// Resolve all secrets, grouping by provider for optimal batch handling.
    ///
    /// # Arguments
    ///
    /// * `secrets` - Map of secret names to (spec, `provider_name`) tuples
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - A required provider is not registered
    /// - Salt is missing when secrets have `cache_key: true`
    /// - Any secret resolution fails
    pub async fn resolve_all(
        &self,
        secrets: &HashMap<String, (SecretSpec, &'static str)>,
    ) -> Result<BatchSecrets, SecretError> {
        // Check salt requirements upfront
        let needs_salt = secrets.values().any(|(spec, _)| spec.cache_key);
        if needs_salt && !self.config.salt_config.has_salt() {
            return Err(SecretError::MissingSalt);
        }

        // Group secrets by provider
        let mut by_provider: HashMap<&'static str, HashMap<String, SecretSpec>> = HashMap::new();
        for (name, (spec, provider)) in secrets {
            by_provider
                .entry(*provider)
                .or_default()
                .insert(name.clone(), spec.clone());
        }

        // Resolve each provider's secrets concurrently
        let provider_futures: Vec<_> = by_provider
            .into_iter()
            .map(|(provider, provider_secrets)| async move {
                let secret_resolver = self.resolvers.get(provider).ok_or_else(|| {
                    SecretError::UnsupportedResolver {
                        resolver: provider.to_string(),
                    }
                })?;
                let batch_results = secret_resolver.resolve_batch(&provider_secrets).await?;
                Ok::<_, SecretError>((provider, batch_results))
            })
            .collect();

        let provider_results = try_join_all(provider_futures).await?;

        // Merge results and compute fingerprints
        let mut batch = BatchSecrets::with_capacity(secrets.len());
        for (_provider, batch_result) in provider_results {
            for (name, secure_value) in batch_result {
                // Compute fingerprint if this secret affects cache keys
                let fingerprint = if let Some((spec, _)) = secrets.get(&name) {
                    if spec.cache_key {
                        // Warn if secret is too short
                        if secure_value.len() < 4 {
                            tracing::warn!(
                                secret = %name,
                                len = secure_value.len(),
                                "Secret is too short for safe cache key inclusion"
                            );
                        }
                        Some(compute_secret_fingerprint(
                            &name,
                            secure_value.expose(),
                            self.config.salt_config.write_salt().unwrap_or(""),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };

                batch.insert(name, secure_value, fingerprint);
            }
        }

        Ok(batch)
    }
}

/// Convenience function for single-provider batch resolution.
///
/// Resolves all secrets using a single resolver and computes fingerprints
/// for secrets with `cache_key: true`.
///
/// # Arguments
///
/// * `resolver` - The secret resolver to use
/// * `secrets` - Map of secret names to their specifications
/// * `salt_config` - Salt configuration for fingerprinting
///
/// # Errors
///
/// Returns error if:
/// - Salt is missing when secrets have `cache_key: true`
/// - Any secret resolution fails
///
/// # Example
///
/// ```ignore
/// use cuenv_secrets::{resolve_batch, SaltConfig, EnvSecretResolver};
///
/// let resolver = EnvSecretResolver::new();
/// let salt = SaltConfig::new(Some("my-salt".to_string()));
///
/// let secrets = resolve_batch(&resolver, &specs, &salt).await?;
/// ```
#[allow(clippy::implicit_hasher)]
pub async fn resolve_batch<R: SecretResolver>(
    resolver: &R,
    secrets: &HashMap<String, SecretSpec>,
    salt_config: &SaltConfig,
) -> Result<BatchSecrets, SecretError> {
    // Check salt requirements
    let needs_salt = secrets.values().any(|s| s.cache_key);
    if needs_salt && !salt_config.has_salt() {
        return Err(SecretError::MissingSalt);
    }

    // Resolve all secrets using the resolver's batch method
    let batch_results = resolver.resolve_batch(secrets).await?;

    // Build BatchSecrets with fingerprints
    let mut batch = BatchSecrets::with_capacity(secrets.len());
    for (name, secure_value) in batch_results {
        let fingerprint = if let Some(spec) = secrets.get(&name) {
            if spec.cache_key {
                // Warn if secret is too short
                if secure_value.len() < 4 {
                    tracing::warn!(
                        secret = %name,
                        len = secure_value.len(),
                        "Secret is too short for safe cache key inclusion"
                    );
                }
                Some(compute_secret_fingerprint(
                    &name,
                    secure_value.expose(),
                    salt_config.write_salt().unwrap_or(""),
                ))
            } else {
                None
            }
        } else {
            None
        };

        batch.insert(name, secure_value, fingerprint);
    }

    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSecretResolver;

    #[tokio::test]
    async fn test_resolve_batch_empty() {
        let resolver = EnvSecretResolver::new();
        let secrets = HashMap::new();
        let salt = SaltConfig::default();

        let result = resolve_batch(&resolver, &secrets, &salt).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_batch_missing_salt() {
        let resolver = EnvSecretResolver::new();
        let mut secrets = HashMap::new();
        secrets.insert("TEST".to_string(), SecretSpec::with_cache_key("TEST_VAR"));
        let salt = SaltConfig::default(); // No salt

        let result = resolve_batch(&resolver, &secrets, &salt).await;
        assert!(matches!(result, Err(SecretError::MissingSalt)));
    }

    #[tokio::test]
    async fn test_batch_resolver_missing_provider() {
        let config = BatchConfig::default();
        let resolver = BatchResolver::new(config);

        let mut secrets = HashMap::new();
        secrets.insert(
            "TEST".to_string(),
            (SecretSpec::new("test"), "unknown_provider"),
        );

        let result = resolver.resolve_all(&secrets).await;
        assert!(matches!(
            result,
            Err(SecretError::UnsupportedResolver { .. })
        ));
    }

    #[tokio::test]
    async fn test_batch_config_from_salt() {
        let config = BatchConfig::from_salt(Some("my-salt".to_string()));
        assert!(config.salt_config.has_salt());
        assert_eq!(config.salt_config.write_salt(), Some("my-salt"));
    }
}
