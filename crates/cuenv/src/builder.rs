//! Builder for configuring cuenv.
//!
//! The [`CuenvBuilder`] allows you to configure cuenv with custom providers
//! before running.
//!
//! # Example
//!
//! ```no_run
//! use cuenv::Cuenv;
//!
//! fn main() -> cuenv::Result<()> {
//!     Cuenv::builder()
//!         .with_defaults()
//!         // .with_sync_provider(my_provider::CustomProvider::new())
//!         .build()
//!         .run()
//! }
//! ```

use crate::Cuenv;
use crate::provider::{RuntimeCapability, SecretCapability, SyncCapability};
use crate::providers::{CiProvider, CubesProvider, RulesProvider};
use crate::registry::ProviderRegistry;

/// Builder for configuring and creating a [`Cuenv`] instance.
///
/// Use [`Cuenv::builder()`] to create a new builder.
pub struct CuenvBuilder {
    registry: ProviderRegistry,
}

impl CuenvBuilder {
    /// Create a new builder with an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: ProviderRegistry::new(),
        }
    }

    /// Register a provider that implements [`SyncCapability`].
    ///
    /// This is type-safe and ensures the provider can be used for sync operations.
    #[must_use]
    pub fn with_sync_provider<P>(mut self, provider: P) -> Self
    where
        P: SyncCapability + 'static,
    {
        self.registry.register_sync(provider);
        self
    }

    /// Register a provider that implements [`RuntimeCapability`].
    ///
    /// This is type-safe and ensures the provider can be used for task execution.
    #[must_use]
    pub fn with_runtime_provider<P>(mut self, provider: P) -> Self
    where
        P: RuntimeCapability + 'static,
    {
        self.registry.register_runtime(provider);
        self
    }

    /// Register a provider that implements [`SecretCapability`].
    ///
    /// This is type-safe and ensures the provider can be used for secret resolution.
    #[must_use]
    pub fn with_secret_provider<P>(mut self, provider: P) -> Self
    where
        P: SecretCapability + 'static,
    {
        self.registry.register_secret(provider);
        self
    }

    /// Add all default providers (ci, cubes, rules).
    ///
    /// This is equivalent to calling:
    /// ```ignore
    /// builder
    ///     .with_sync_provider(CiProvider)
    ///     .with_sync_provider(CubesProvider)
    ///     .with_sync_provider(RulesProvider)
    /// ```
    #[must_use]
    pub fn with_defaults(self) -> Self {
        self.with_sync_provider(CiProvider::new())
            .with_sync_provider(CubesProvider::new())
            .with_sync_provider(RulesProvider::new())
    }

    /// Build the [`Cuenv`] instance.
    #[must_use]
    pub fn build(self) -> Cuenv {
        Cuenv {
            registry: self.registry,
        }
    }
}

impl Default for CuenvBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_new() {
        let builder = CuenvBuilder::new();
        let cuenv = builder.build();
        assert!(cuenv.registry.is_empty());
    }

    #[test]
    fn test_builder_with_defaults() {
        let builder = CuenvBuilder::new().with_defaults();
        let cuenv = builder.build();
        assert_eq!(cuenv.registry.sync_provider_count(), 3);
    }

    #[test]
    fn test_builder_default_trait() {
        let builder = CuenvBuilder::default();
        let cuenv = builder.build();
        assert!(cuenv.registry.is_empty());
    }

    #[test]
    fn test_builder_with_sync_provider() {
        let builder = CuenvBuilder::new().with_sync_provider(CiProvider::new());
        let cuenv = builder.build();
        assert_eq!(cuenv.registry.sync_provider_count(), 1);
    }

    #[test]
    fn test_builder_with_multiple_sync_providers() {
        let builder = CuenvBuilder::new()
            .with_sync_provider(CiProvider::new())
            .with_sync_provider(CubesProvider::new());
        let cuenv = builder.build();
        assert_eq!(cuenv.registry.sync_provider_count(), 2);
    }

    #[test]
    fn test_builder_chaining() {
        // Test that all builder methods return Self for chaining
        let cuenv = CuenvBuilder::new()
            .with_sync_provider(CiProvider::new())
            .with_sync_provider(RulesProvider::new())
            .build();
        assert_eq!(cuenv.registry.sync_provider_count(), 2);
    }

    #[test]
    fn test_builder_defaults_then_more() {
        // Add defaults then additional providers
        let builder = CuenvBuilder::new()
            .with_defaults()
            .with_sync_provider(CiProvider::new()); // Duplicate CI provider
        let cuenv = builder.build();
        // Should have 4 sync providers (3 defaults + 1 more)
        assert_eq!(cuenv.registry.sync_provider_count(), 4);
    }
}
