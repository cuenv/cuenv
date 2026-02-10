//! Tool provider registry.
//!
//! This module provides a registry for tool providers, allowing multiple
//! providers to be registered and looked up by name or source type.

use std::collections::HashMap;
use std::sync::Arc;

use super::provider::{ToolProvider, ToolSource};

/// Registry of tool providers.
///
/// The registry maintains a collection of tool providers and provides
/// lookup by name or source type.
#[derive(Default)]
pub struct ToolRegistry {
    /// Providers indexed by name.
    providers: HashMap<&'static str, Arc<dyn ToolProvider>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool provider.
    ///
    /// If a provider with the same name already exists, it will be replaced.
    pub fn register<P: ToolProvider + 'static>(&mut self, provider: P) {
        let name = provider.name();
        self.providers.insert(name, Arc::new(provider));
    }

    /// Register a tool provider wrapped in Arc.
    ///
    /// Useful when the same provider instance needs to be shared.
    pub fn register_arc(&mut self, provider: Arc<dyn ToolProvider>) {
        let name = provider.name();
        self.providers.insert(name, provider);
    }

    /// Get a provider by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ToolProvider>> {
        self.providers.get(name)
    }

    /// Find a provider that can handle the given source.
    #[must_use]
    pub fn find_for_source(&self, source: &ToolSource) -> Option<&Arc<dyn ToolProvider>> {
        self.providers.values().find(|p| p.can_handle(source))
    }

    /// Iterate over all registered providers.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn ToolProvider>> {
        self.providers.values()
    }

    /// Get the number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Get all provider names.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.providers.keys().copied().collect()
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("providers", &self.names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::provider::{
        FetchedTool, ResolvedTool, ToolOptions, ToolResolveRequest,
    };
    use async_trait::async_trait;

    struct MockProvider {
        name: &'static str,
    }

    #[async_trait]
    impl ToolProvider for MockProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        fn description(&self) -> &'static str {
            "Mock provider for testing"
        }

        fn can_handle(&self, source: &ToolSource) -> bool {
            matches!(source, ToolSource::GitHub { .. }) && self.name == "github"
        }

        async fn resolve(&self, _request: &ToolResolveRequest<'_>) -> crate::Result<ResolvedTool> {
            unimplemented!()
        }

        async fn fetch(
            &self,
            _resolved: &ResolvedTool,
            _options: &ToolOptions,
        ) -> crate::Result<FetchedTool> {
            unimplemented!()
        }

        fn is_cached(&self, _resolved: &ResolvedTool, _options: &ToolOptions) -> bool {
            false
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });

        assert!(registry.get("github").is_some());
        assert!(registry.get("nix").is_none());
    }

    #[test]
    fn test_registry_find_for_source() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });

        let source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(registry.find_for_source(&source).is_some());

        let source = ToolSource::Nix {
            flake: "nixpkgs".into(),
            package: "jq".into(),
            output: None,
        };
        assert!(registry.find_for_source(&source).is_none());
    }

    #[test]
    fn test_registry_iter() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });
        registry.register(MockProvider { name: "nix" });

        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());

        let names: Vec<_> = registry.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"github"));
        assert!(names.contains(&"nix"));
    }

    #[test]
    fn test_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_default() {
        let registry = ToolRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_register_replaces_existing() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });
        registry.register(MockProvider { name: "github" });

        // Should still have only one provider
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_register_arc() {
        let mut registry = ToolRegistry::new();
        let provider: Arc<dyn ToolProvider> = Arc::new(MockProvider { name: "github" });
        registry.register_arc(provider);

        assert!(registry.get("github").is_some());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_register_arc_replaces_existing() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });

        let provider: Arc<dyn ToolProvider> = Arc::new(MockProvider { name: "github" });
        registry.register_arc(provider);

        // Should still have only one provider
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_names() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });
        registry.register(MockProvider { name: "nix" });
        registry.register(MockProvider { name: "rustup" });

        let names = registry.names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"github"));
        assert!(names.contains(&"nix"));
        assert!(names.contains(&"rustup"));
    }

    #[test]
    fn test_registry_names_empty() {
        let registry = ToolRegistry::new();
        let names = registry.names();
        assert!(names.is_empty());
    }

    #[test]
    fn test_registry_debug() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "github" });

        let debug_str = format!("{:?}", registry);
        assert!(debug_str.contains("ToolRegistry"));
        assert!(debug_str.contains("providers"));
        assert!(debug_str.contains("github"));
    }

    #[test]
    fn test_registry_debug_empty() {
        let registry = ToolRegistry::new();
        let debug_str = format!("{:?}", registry);
        assert!(debug_str.contains("ToolRegistry"));
    }

    #[test]
    fn test_registry_find_for_source_no_match() {
        let mut registry = ToolRegistry::new();
        // Register github provider that only handles GitHub sources
        registry.register(MockProvider { name: "github" });

        // Try to find a provider for a Rustup source - should return None
        let source = ToolSource::Rustup {
            toolchain: "stable".into(),
            profile: None,
            components: vec![],
            targets: vec![],
        };
        assert!(registry.find_for_source(&source).is_none());
    }

    #[test]
    fn test_registry_find_for_source_oci() {
        let registry = ToolRegistry::new();

        let source = ToolSource::Oci {
            image: "alpine:latest".into(),
            path: "bin/sh".into(),
        };
        // Empty registry has no provider
        assert!(registry.find_for_source(&source).is_none());
    }

    #[test]
    fn test_registry_get_nonexistent() {
        let registry = ToolRegistry::new();
        assert!(registry.get("nonexistent").is_none());
        assert!(registry.get("github").is_none());
        assert!(registry.get("").is_none());
    }

    #[test]
    fn test_registry_iter_empty() {
        let registry = ToolRegistry::new();
        let count = registry.iter().count();
        assert_eq!(count, 0);
    }
}
