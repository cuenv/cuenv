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
    pub const fn new(salt_config: SaltConfig) -> Self {
        Self { salt_config }
    }

    /// Create a batch config from an optional salt string.
    #[must_use]
    pub const fn from_salt(salt: Option<String>) -> Self {
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
                let fingerprint = secrets.get(&name).and_then(|(spec, _)| {
                    if !spec.cache_key {
                        return None;
                    }
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
                });

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
        let fingerprint = secrets.get(&name).and_then(|spec| {
            if !spec.cache_key {
                return None;
            }
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
        });

        batch.insert(name, secure_value, fingerprint);
    }

    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSecretResolver;

    // ==========================================================================
    // BatchConfig tests
    // ==========================================================================

    #[test]
    fn test_batch_config_default() {
        let config = BatchConfig::default();
        assert!(!config.salt_config.has_salt());
    }

    #[test]
    fn test_batch_config_new() {
        let salt_config = SaltConfig::new(Some("test-salt".to_string()));
        let config = BatchConfig::new(salt_config);
        assert!(config.salt_config.has_salt());
        assert_eq!(config.salt_config.write_salt(), Some("test-salt"));
    }

    #[test]
    fn test_batch_config_from_salt() {
        let config = BatchConfig::from_salt(Some("my-salt".to_string()));
        assert!(config.salt_config.has_salt());
        assert_eq!(config.salt_config.write_salt(), Some("my-salt"));
    }

    #[test]
    fn test_batch_config_from_salt_none() {
        let config = BatchConfig::from_salt(None);
        assert!(!config.salt_config.has_salt());
    }

    #[test]
    fn test_batch_config_clone() {
        let config = BatchConfig::from_salt(Some("cloned-salt".to_string()));
        let cloned = config.clone();
        assert!(cloned.salt_config.has_salt());
    }

    #[test]
    fn test_batch_config_debug() {
        let config = BatchConfig::default();
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("BatchConfig"));
    }

    // ==========================================================================
    // BatchResolver tests
    // ==========================================================================

    #[test]
    fn test_batch_resolver_new() {
        let config = BatchConfig::default();
        let resolver = BatchResolver::new(config);
        assert_eq!(resolver.resolver_count(), 0);
    }

    #[test]
    fn test_batch_resolver_add_resolver() {
        let config = BatchConfig::default();
        let mut resolver = BatchResolver::new(config);
        let env_resolver = EnvSecretResolver::new();

        resolver.add_resolver(&env_resolver);
        assert_eq!(resolver.resolver_count(), 1);
    }

    #[test]
    fn test_batch_resolver_add_multiple_resolvers() {
        let config = BatchConfig::default();
        let mut resolver = BatchResolver::new(config);
        let env_resolver = EnvSecretResolver::new();

        // Adding the same resolver type replaces (uses same provider_name key)
        resolver.add_resolver(&env_resolver);
        resolver.add_resolver(&env_resolver);
        assert_eq!(resolver.resolver_count(), 1);
    }

    #[tokio::test]
    async fn test_batch_resolver_resolve_all_empty() {
        let config = BatchConfig::default();
        let resolver = BatchResolver::new(config);
        let secrets: HashMap<String, (SecretSpec, &'static str)> = HashMap::new();

        let result = resolver.resolve_all(&secrets).await.unwrap();
        assert!(result.is_empty());
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
    async fn test_batch_resolver_missing_salt_for_cache_key() {
        let config = BatchConfig::default(); // No salt
        let resolver = BatchResolver::new(config);

        let mut secrets = HashMap::new();
        secrets.insert(
            "TEST".to_string(),
            (SecretSpec::with_cache_key("test"), "env"),
        );

        let result = resolver.resolve_all(&secrets).await;
        assert!(matches!(result, Err(SecretError::MissingSalt)));
    }

    // ==========================================================================
    // resolve_batch function tests
    // ==========================================================================

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
    async fn test_resolve_batch_no_cache_key_no_salt_ok() {
        let resolver = EnvSecretResolver::new();
        let mut secrets = HashMap::new();
        // SecretSpec without cache_key doesn't require salt
        secrets.insert("TEST".to_string(), SecretSpec::new("NONEXISTENT_VAR"));
        let salt = SaltConfig::default();

        // This may fail if the env var doesn't exist, but it shouldn't fail due to missing salt
        let result = resolve_batch(&resolver, &secrets, &salt).await;
        // Either succeeds or fails for other reasons (missing env var), not missing salt
        match result {
            Err(SecretError::MissingSalt) => panic!("Should not require salt for non-cache-key secrets"),
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_resolve_batch_with_salt_and_cache_key() {
        // Set an env var for testing
        // SAFETY: Test runs in isolation
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("BATCH_TEST_SECRET", "test_value");
        }

        let resolver = EnvSecretResolver::new();
        let mut secrets = HashMap::new();
        secrets.insert(
            "my_secret".to_string(),
            SecretSpec::with_cache_key("BATCH_TEST_SECRET"),
        );
        let salt = SaltConfig::new(Some("test-salt".to_string()));

        let result = resolve_batch(&resolver, &secrets, &salt).await.unwrap();
        assert!(!result.is_empty());

        // Cleanup
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("BATCH_TEST_SECRET");
        }
    }

    #[tokio::test]
    async fn test_resolve_batch_without_cache_key() {
        // Set an env var for testing
        // SAFETY: Test runs in isolation
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("BATCH_TEST_NO_CACHE", "another_value");
        }

        let resolver = EnvSecretResolver::new();
        let mut secrets = HashMap::new();
        // Without cache_key, no fingerprint is computed
        secrets.insert("my_secret".to_string(), SecretSpec::new("BATCH_TEST_NO_CACHE"));
        let salt = SaltConfig::default();

        let result = resolve_batch(&resolver, &secrets, &salt).await.unwrap();
        assert!(!result.is_empty());

        // Cleanup
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("BATCH_TEST_NO_CACHE");
        }
    }

    #[tokio::test]
    async fn test_resolve_batch_multiple_secrets() {
        // SAFETY: Test runs in isolation
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("BATCH_MULTI_1", "value1");
            std::env::set_var("BATCH_MULTI_2", "value2");
        }

        let resolver = EnvSecretResolver::new();
        let mut secrets = HashMap::new();
        secrets.insert("secret1".to_string(), SecretSpec::new("BATCH_MULTI_1"));
        secrets.insert("secret2".to_string(), SecretSpec::new("BATCH_MULTI_2"));
        let salt = SaltConfig::default();

        let result = resolve_batch(&resolver, &secrets, &salt).await.unwrap();
        assert_eq!(result.len(), 2);

        // Cleanup
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("BATCH_MULTI_1");
            std::env::remove_var("BATCH_MULTI_2");
        }
    }
}
