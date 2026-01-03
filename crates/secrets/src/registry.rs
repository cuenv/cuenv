//! Secret provider registry
//!
//! Provides a registry for dynamically registering and looking up secret resolvers.
//! This allows consumers to register providers at runtime without hardcoding them.

use crate::{SecretError, SecretResolver, SecretSpec};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for secret resolvers
///
/// Allows dynamic registration of secret providers by name. Consumers can
/// register their own resolvers and look them up by provider name at runtime.
///
/// # Example
///
/// ```ignore
/// use cuenv_secrets::{SecretRegistry, EnvSecretResolver};
///
/// let mut registry = SecretRegistry::new();
/// registry.register(Arc::new(EnvSecretResolver::new()));
///
/// let resolver = registry.get("env").unwrap();
/// let secret = resolver.resolve("API_KEY", &spec).await?;
/// ```
#[derive(Default)]
pub struct SecretRegistry {
    resolvers: HashMap<&'static str, Arc<dyn SecretResolver>>,
}

impl SecretRegistry {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            resolvers: HashMap::new(),
        }
    }

    /// Register a resolver
    ///
    /// The resolver's `provider_name()` is used as the key. If a resolver
    /// with the same name already exists, it is replaced.
    pub fn register(&mut self, resolver: Arc<dyn SecretResolver>) {
        self.resolvers.insert(resolver.provider_name(), resolver);
    }

    /// Get a resolver by provider name
    ///
    /// Returns `None` if no resolver is registered for the given name.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<Arc<dyn SecretResolver>> {
        self.resolvers.get(provider).cloned()
    }

    /// Check if a resolver is registered for the given provider name
    #[must_use]
    pub fn has(&self, provider: &str) -> bool {
        self.resolvers.contains_key(provider)
    }

    /// Get all registered provider names
    #[must_use]
    pub fn providers(&self) -> Vec<&'static str> {
        self.resolvers.keys().copied().collect()
    }

    /// Resolve a secret using the appropriate resolver
    ///
    /// Looks up the resolver by provider name and delegates resolution.
    ///
    /// # Errors
    ///
    /// Returns `SecretError::UnsupportedResolver` if no resolver is registered
    /// for the given provider name.
    pub async fn resolve(
        &self,
        provider: &str,
        name: &str,
        spec: &SecretSpec,
    ) -> Result<String, SecretError> {
        let resolver = self
            .get(provider)
            .ok_or_else(|| SecretError::UnsupportedResolver {
                resolver: provider.to_string(),
            })?;

        resolver.resolve(name, spec).await
    }
}

impl std::fmt::Debug for SecretRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretRegistry")
            .field("providers", &self.providers())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvers::EnvSecretResolver;

    #[test]
    fn test_registry_new() {
        let registry = SecretRegistry::new();
        assert!(registry.providers().is_empty());
    }

    #[test]
    fn test_registry_default() {
        let registry = SecretRegistry::default();
        assert!(registry.providers().is_empty());
    }

    #[test]
    fn test_registry_register() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));

        assert!(registry.has("env"));
        assert_eq!(registry.providers(), vec!["env"]);
    }

    #[test]
    fn test_registry_get() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));

        let resolver = registry.get("env");
        assert!(resolver.is_some());
        assert_eq!(resolver.unwrap().provider_name(), "env");
    }

    #[test]
    fn test_registry_get_missing() {
        let registry = SecretRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_has() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));

        assert!(registry.has("env"));
        assert!(!registry.has("vault"));
    }

    #[test]
    fn test_registry_replace() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));
        registry.register(Arc::new(EnvSecretResolver::new()));

        // Should still have only one "env" provider
        assert_eq!(registry.providers().len(), 1);
    }

    #[test]
    fn test_registry_debug() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));

        let debug = format!("{registry:?}");
        assert!(debug.contains("SecretRegistry"));
        assert!(debug.contains("env"));
    }

    #[tokio::test]
    async fn test_registry_resolve_unsupported() {
        let registry = SecretRegistry::new();
        let spec = SecretSpec::new("source");

        let result = registry.resolve("unknown", "secret", &spec).await;
        assert!(result.is_err());

        if let Err(SecretError::UnsupportedResolver { resolver }) = result {
            assert_eq!(resolver, "unknown");
        } else {
            panic!("Expected UnsupportedResolver error");
        }
    }

    #[tokio::test]
    async fn test_registry_resolve_env() {
        let mut registry = SecretRegistry::new();
        registry.register(Arc::new(EnvSecretResolver::new()));

        temp_env::async_with_vars([("TEST_SECRET_REGISTRY", Some("test_value"))], async {
            let spec = SecretSpec::new("TEST_SECRET_REGISTRY");

            let result = registry.resolve("env", "TEST_SECRET_REGISTRY", &spec).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "test_value");
        })
        .await;
    }
}
