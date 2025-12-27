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
//!         // .with_provider(my_provider::CustomProvider)
//!         .build()
//!         .run()
//! }
//! ```

use crate::provider::{Provider, RuntimeCapability, SecretCapability, SyncCapability};
use crate::providers::{CiProvider, CubesProvider, RulesProvider};
use crate::registry::ProviderRegistry;
use crate::Cuenv;

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

    /// Register a provider that implements [`Provider`].
    ///
    /// This is the base registration method. For providers that implement
    /// specific capabilities, use the type-safe registration methods:
    /// - [`with_sync_provider()`](Self::with_sync_provider)
    /// - [`with_runtime_provider()`](Self::with_runtime_provider)
    /// - [`with_secret_provider()`](Self::with_secret_provider)
    #[must_use]
    pub fn with_provider<P: Provider>(mut self, provider: P) -> Self {
        self.registry.register(provider);
        self
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
}
