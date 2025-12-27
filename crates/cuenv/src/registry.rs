//! Provider registry for managing registered providers.
//!
//! The [`ProviderRegistry`] stores all registered providers and provides methods
//! to query providers by their capabilities.

use std::sync::Arc;

use crate::provider::{Provider, RuntimeCapability, SecretCapability, SyncCapability};

/// Storage wrapper for generic providers (base Provider trait only).
///
/// For capability-specific storage, use the typed vectors in ProviderRegistry.
struct ProviderEntry {
    /// The provider instance.
    #[allow(dead_code)] // Reserved for future use with dynamic capability detection
    provider: Box<dyn Provider>,
}

/// Registry for managing providers.
///
/// Providers are registered via [`CuenvBuilder::with_provider()`](crate::CuenvBuilder::with_provider)
/// and can be queried by capability.
///
/// # Example
///
/// ```ignore
/// let registry = ProviderRegistry::new();
///
/// // Get all sync providers
/// for provider in registry.sync_providers() {
///     println!("Sync provider: {}", provider.name());
/// }
/// ```
pub struct ProviderRegistry {
    /// All registered providers with their capabilities.
    entries: Vec<ProviderEntry>,
    /// Sync providers (indexed separately for efficient access).
    sync_providers: Vec<Arc<dyn SyncCapability>>,
    /// Runtime providers (indexed separately for efficient access).
    runtime_providers: Vec<Arc<dyn RuntimeCapability>>,
    /// Secret providers (indexed separately for efficient access).
    secret_providers: Vec<Arc<dyn SecretCapability>>,
}

impl ProviderRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            sync_providers: Vec::new(),
            runtime_providers: Vec::new(),
            secret_providers: Vec::new(),
        }
    }

    /// Register a generic provider (base trait only).
    ///
    /// For capability-specific registration, use `register_sync`, `register_runtime`, or `register_secret`.
    pub fn register<P: Provider>(&mut self, provider: P) {
        self.entries.push(ProviderEntry {
            provider: Box::new(provider),
        });
    }

    /// Register a provider that implements SyncCapability.
    ///
    /// This is a type-safe way to register sync providers.
    pub fn register_sync<P>(&mut self, provider: P)
    where
        P: SyncCapability + 'static,
    {
        self.sync_providers.push(Arc::new(provider));
    }

    /// Register a provider that implements RuntimeCapability.
    ///
    /// This is a type-safe way to register runtime providers.
    pub fn register_runtime<P>(&mut self, provider: P)
    where
        P: RuntimeCapability + 'static,
    {
        self.runtime_providers.push(Arc::new(provider));
    }

    /// Register a provider that implements SecretCapability.
    ///
    /// This is a type-safe way to register secret providers.
    pub fn register_secret<P>(&mut self, provider: P)
    where
        P: SecretCapability + 'static,
    {
        self.secret_providers.push(Arc::new(provider));
    }

    /// Get all registered providers.
    pub fn all(&self) -> impl Iterator<Item = &dyn Provider> {
        self.entries.iter().map(|e| e.provider.as_ref())
    }

    /// Get all providers that implement SyncCapability.
    pub fn sync_providers(&self) -> impl Iterator<Item = &Arc<dyn SyncCapability>> {
        self.sync_providers.iter()
    }

    /// Get all providers that implement RuntimeCapability.
    pub fn runtime_providers(&self) -> impl Iterator<Item = &Arc<dyn RuntimeCapability>> {
        self.runtime_providers.iter()
    }

    /// Get all providers that implement SecretCapability.
    pub fn secret_providers(&self) -> impl Iterator<Item = &Arc<dyn SecretCapability>> {
        self.secret_providers.iter()
    }

    /// Get a sync provider by name.
    #[must_use]
    pub fn get_sync_provider(&self, name: &str) -> Option<&Arc<dyn SyncCapability>> {
        self.sync_providers.iter().find(|p| p.name() == name)
    }

    /// Get a runtime provider by name.
    #[must_use]
    pub fn get_runtime_provider(&self, name: &str) -> Option<&Arc<dyn RuntimeCapability>> {
        self.runtime_providers.iter().find(|p| p.name() == name)
    }

    /// Get a secret provider by name.
    #[must_use]
    pub fn get_secret_provider(&self, name: &str) -> Option<&Arc<dyn SecretCapability>> {
        self.secret_providers.iter().find(|p| p.name() == name)
    }

    /// Get all sync provider names.
    #[must_use]
    pub fn sync_provider_names(&self) -> Vec<&'static str> {
        self.sync_providers.iter().map(|p| p.name()).collect()
    }

    /// Get all runtime provider names.
    #[must_use]
    pub fn runtime_provider_names(&self) -> Vec<&'static str> {
        self.runtime_providers.iter().map(|p| p.name()).collect()
    }

    /// Get all secret provider names.
    #[must_use]
    pub fn secret_provider_names(&self) -> Vec<&'static str> {
        self.secret_providers.iter().map(|p| p.name()).collect()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
            && self.sync_providers.is_empty()
            && self.runtime_providers.is_empty()
            && self.secret_providers.is_empty()
    }

    /// Get the total number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
            + self.sync_providers.len()
            + self.runtime_providers.len()
            + self.secret_providers.len()
    }

    /// Get the number of sync providers.
    #[must_use]
    pub fn sync_provider_count(&self) -> usize {
        self.sync_providers.len()
    }

    /// Get the number of runtime providers.
    #[must_use]
    pub fn runtime_provider_count(&self) -> usize {
        self.runtime_providers.len()
    }

    /// Get the number of secret providers.
    #[must_use]
    pub fn secret_provider_count(&self) -> usize {
        self.secret_providers.len()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = ProviderRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_sync_provider_names() {
        let registry = ProviderRegistry::new();
        let names = registry.sync_provider_names();
        assert!(names.is_empty());
    }
}
