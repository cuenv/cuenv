//! Secret Resolution for cuenv
//!
//! Provides a unified interface for resolving secrets from various providers
//! (environment variables, command execution, 1Password, Vault, etc.) with
//! support for cache key fingerprinting and salt rotation.

mod fingerprint;
pub mod resolvers;
mod resolved;
mod salt;

pub use fingerprint::compute_secret_fingerprint;
pub use resolved::ResolvedSecrets;
pub use salt::SaltConfig;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Error types for secret resolution
#[derive(Debug, Error)]
pub enum SecretError {
    /// Secret not found
    #[error("Secret '{name}' not found from source '{secret_source}'")]
    NotFound {
        /// Secret name
        name: String,
        /// Source that was searched (e.g., env var name)
        secret_source: String,
    },

    /// Secret is too short for safe fingerprinting (< 4 chars)
    #[error("Secret '{name}' is too short ({len} chars, minimum 4) for cache key inclusion")]
    TooShort {
        /// Secret name
        name: String,
        /// Actual length of the secret value
        len: usize,
    },

    /// Missing salt when secrets require fingerprinting
    #[error("CUENV_SECRET_SALT required when secrets have cache_key: true")]
    MissingSalt,

    /// Resolver execution failed
    #[error("Failed to resolve secret '{name}': {message}")]
    ResolutionFailed {
        /// Secret name
        name: String,
        /// Error message from the resolver
        message: String,
    },

    /// Unsupported resolver type
    #[error("Unsupported secret resolver: {resolver}")]
    UnsupportedResolver {
        /// The resolver type that was requested
        resolver: String,
    },
}

/// Configuration for a secret to resolve
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretSpec {
    /// Source reference (env var name, 1Password reference, etc.)
    pub source: String,

    /// Include secret in cache key via salted HMAC
    #[serde(default)]
    pub cache_key: bool,
}

impl SecretSpec {
    /// Create a new secret spec
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            cache_key: false,
        }
    }

    /// Create a secret spec that affects cache keys
    #[must_use]
    pub fn with_cache_key(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            cache_key: true,
        }
    }
}

/// Trait for resolving secrets from various providers
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// Resolve a single secret by name and spec
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError>;

    /// Resolve multiple secrets at once
    async fn resolve_all(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, String>, SecretError> {
        let mut result = HashMap::new();
        for (name, spec) in secrets {
            let value = self.resolve(name, spec).await?;
            result.insert(name.clone(), value);
        }
        Ok(result)
    }
}
