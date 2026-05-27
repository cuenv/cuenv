//! Provider registry for managing registered providers.
//!
//! The [`ProviderRegistry`] stores all registered sync providers and provides
//! name-based lookup plus iteration.

use std::collections::HashMap;
use std::sync::Arc;

use crate::provider::SyncCapability;

/// Registry for managing sync providers.
///
/// Providers are registered via [`CuenvBuilder::with_sync_provider()`](crate::CuenvBuilder::with_sync_provider)
/// and can be queried by name.
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
    /// Sync providers with O(1) name lookup.
    sync_providers: Vec<Arc<dyn SyncCapability>>,
    sync_by_name: HashMap<&'static str, usize>,
}

impl ProviderRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sync_providers: Vec::new(),
            sync_by_name: HashMap::new(),
        }
    }

    /// Register a provider that implements SyncCapability.
    ///
    /// This is a type-safe way to register sync providers.
    pub fn register_sync<P>(&mut self, provider: P)
    where
        P: SyncCapability + 'static,
    {
        let name = provider.name();
        let index = self.sync_providers.len();
        self.sync_providers.push(Arc::new(provider));
        self.sync_by_name.insert(name, index);
    }

    /// Get all providers that implement SyncCapability.
    pub fn sync_providers(&self) -> impl Iterator<Item = &Arc<dyn SyncCapability>> {
        self.sync_providers.iter()
    }

    /// Get a sync provider by name (O(1) lookup).
    #[must_use]
    pub fn get_sync_provider(&self, name: &str) -> Option<&Arc<dyn SyncCapability>> {
        self.sync_by_name
            .get(name)
            .map(|&idx| &self.sync_providers[idx])
    }

    /// Get all sync provider names.
    #[must_use]
    pub fn sync_provider_names(&self) -> Vec<&'static str> {
        self.sync_by_name.keys().copied().collect()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sync_providers.is_empty()
    }

    /// Get the total number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sync_providers.len()
    }

    /// Get the number of sync providers.
    #[must_use]
    pub fn sync_provider_count(&self) -> usize {
        self.sync_providers.len()
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
    use crate::providers::{CiProvider, CodegenProvider, RulesProvider};

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

    #[test]
    fn test_register_and_retrieve_sync_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register_sync(CiProvider::new());

        assert_eq!(registry.sync_provider_count(), 1);
        assert!(!registry.is_empty());

        let provider = registry.get_sync_provider("ci");
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "ci");
    }

    #[test]
    fn test_get_nonexistent_provider() {
        let registry = ProviderRegistry::new();
        assert!(registry.get_sync_provider("nonexistent").is_none());
    }

    #[test]
    fn test_multiple_sync_providers() {
        let mut registry = ProviderRegistry::new();
        registry.register_sync(CiProvider::new());
        registry.register_sync(CodegenProvider::new());
        registry.register_sync(RulesProvider::new());

        assert_eq!(registry.sync_provider_count(), 3);
        assert_eq!(registry.len(), 3);

        // Verify all can be retrieved by name
        assert!(registry.get_sync_provider("ci").is_some());
        assert!(registry.get_sync_provider("codegen").is_some());
        assert!(registry.get_sync_provider("rules").is_some());
    }

    #[test]
    fn test_sync_provider_names_returns_all() {
        let mut registry = ProviderRegistry::new();
        registry.register_sync(CiProvider::new());
        registry.register_sync(CodegenProvider::new());

        let names = registry.sync_provider_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"ci"));
        assert!(names.contains(&"codegen"));
    }

    #[test]
    fn test_sync_providers_iterator() {
        let mut registry = ProviderRegistry::new();
        registry.register_sync(CiProvider::new());
        registry.register_sync(CodegenProvider::new());

        let providers: Vec<_> = registry.sync_providers().collect();
        assert_eq!(providers.len(), 2);
    }

    #[test]
    fn test_registry_len_tracks_sync_providers() {
        let mut registry = ProviderRegistry::new();
        registry.register_sync(CiProvider::new());
        registry.register_sync(CodegenProvider::new());

        assert_eq!(registry.sync_provider_count(), 2);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_default_registry() {
        let registry = ProviderRegistry::default();
        assert!(registry.is_empty());
    }
}
