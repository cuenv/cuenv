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
    use crate::tools::provider::{FetchedTool, Platform, ResolvedTool, ToolOptions};
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
            matches!(source, ToolSource::Homebrew { .. }) && self.name == "homebrew"
        }

        async fn resolve(
            &self,
            _tool_name: &str,
            _version: &str,
            _platform: &Platform,
            _config: &serde_json::Value,
        ) -> crate::Result<ResolvedTool> {
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
        registry.register(MockProvider { name: "homebrew" });

        assert!(registry.get("homebrew").is_some());
        assert!(registry.get("github").is_none());
    }

    #[test]
    fn test_registry_find_for_source() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "homebrew" });

        let source = ToolSource::Homebrew {
            formula: "jq".into(),
            image_ref: "test".into(),
        };
        assert!(registry.find_for_source(&source).is_some());

        let source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(registry.find_for_source(&source).is_none());
    }

    #[test]
    fn test_registry_iter() {
        let mut registry = ToolRegistry::new();
        registry.register(MockProvider { name: "homebrew" });
        registry.register(MockProvider { name: "github" });

        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());

        let names: Vec<_> = registry.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"homebrew"));
        assert!(names.contains(&"github"));
    }
}
