//! Secret Resolution for cuenv
//!
//! Provides a unified interface for resolving secrets from various providers
//! (environment variables, command execution, 1Password, Vault, etc.) with
//! support for cache key fingerprinting and salt rotation.
//!
//! # Batch Resolution
//!
//! For resolving multiple secrets efficiently, use the batch resolution API:
//!
//! ```ignore
//! use cuenv_secrets::{BatchSecrets, SecretResolver, SecretSpec};
//!
//! // Resolve multiple secrets concurrently
//! let secrets = resolver.resolve_batch(&specs).await?;
//!
//! // Use secrets during task execution
//! for name in secrets.names() {
//!     if let Some(secret) = secrets.get(name) {
//!         std::env::set_var(name, secret.expose());
//!     }
//! }
//! // Secrets are zeroed when `secrets` goes out of scope
//! ```

mod batch;
mod fingerprint;
mod resolved;
pub mod resolvers;
mod salt;
mod types;

pub use batch::{BatchConfig, BatchResolver, resolve_batch};
pub use fingerprint::compute_secret_fingerprint;
pub use resolved::ResolvedSecrets;
pub use salt::SaltConfig;
pub use types::{BatchSecrets, SecureSecret};

// Re-export built-in resolvers (no external dependencies)
pub use resolvers::{EnvSecretResolver, ExecSecretResolver};

// Provider implementations are in separate crates:
// - cuenv-aws: AwsResolver, AwsSecretConfig
// - cuenv-gcp: GcpResolver, GcpSecretConfig
// - cuenv-vault: VaultResolver, VaultSecretConfig
// - cuenv-1password: OnePasswordResolver, OnePasswordConfig

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Trait for resolving secrets from various providers.
///
/// Implementors must provide:
/// - [`resolve`](SecretResolver::resolve) - Single secret resolution
/// - [`provider_name`](SecretResolver::provider_name) - Provider identifier for grouping
///
/// The trait provides default implementations for batch operations that can be
/// overridden for providers with native batch APIs (e.g., AWS `BatchGetSecretValue`).
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// Resolve a single secret by name and spec.
    ///
    /// This is the primary method that must be implemented by all resolvers.
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError>;

    /// Get the provider name for this resolver.
    ///
    /// Used for grouping secrets by provider in batch resolution.
    /// Examples: `"env"`, `"aws"`, `"vault"`, `"onepassword"`
    fn provider_name(&self) -> &'static str;

    /// Resolve a single secret returning a secure value.
    ///
    /// The returned [`SecureSecret`] will automatically zero its memory on drop.
    async fn resolve_secure(
        &self,
        name: &str,
        spec: &SecretSpec,
    ) -> Result<SecureSecret, SecretError> {
        let value = self.resolve(name, spec).await?;
        Ok(SecureSecret::new(value))
    }

    /// Resolve multiple secrets in batch with concurrent execution.
    ///
    /// Override this method to implement provider-specific batch APIs
    /// (e.g., AWS `BatchGetSecretValue`, 1Password `Secrets.ResolveAll`).
    ///
    /// The default implementation resolves secrets concurrently using
    /// `futures::try_join_all`, which is optimal for providers without
    /// native batch APIs.
    ///
    /// # Returns
    ///
    /// A map of secret names to [`SecureSecret`] values that will be
    /// automatically zeroed on drop.
    async fn resolve_batch(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        use futures::future::try_join_all;

        let futures: Vec<_> = secrets
            .iter()
            .map(|(name, spec)| {
                let name = name.clone();
                let spec = spec.clone();
                async move {
                    let value = self.resolve_secure(&name, &spec).await?;
                    Ok::<_, SecretError>((name, value))
                }
            })
            .collect();

        let results = try_join_all(futures).await?;
        Ok(results.into_iter().collect())
    }

    /// Check if this resolver supports native batch resolution.
    ///
    /// Returns `true` if the provider has a native batch API that is more
    /// efficient than concurrent single calls.
    fn supports_native_batch(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_error_not_found() {
        let err = SecretError::NotFound {
            name: "API_KEY".to_string(),
            secret_source: "env:API_KEY".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("API_KEY"));
        assert!(msg.contains("env:API_KEY"));
    }

    #[test]
    fn test_secret_error_too_short() {
        let err = SecretError::TooShort {
            name: "SHORT_SECRET".to_string(),
            len: 2,
        };
        let msg = err.to_string();
        assert!(msg.contains("SHORT_SECRET"));
        assert!(msg.contains("2 chars"));
        assert!(msg.contains("minimum 4"));
    }

    #[test]
    fn test_secret_error_missing_salt() {
        let err = SecretError::MissingSalt;
        let msg = err.to_string();
        assert!(msg.contains("CUENV_SECRET_SALT"));
        assert!(msg.contains("cache_key: true"));
    }

    #[test]
    fn test_secret_error_resolution_failed() {
        let err = SecretError::ResolutionFailed {
            name: "DATABASE_URL".to_string(),
            message: "connection timeout".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("DATABASE_URL"));
        assert!(msg.contains("connection timeout"));
    }

    #[test]
    fn test_secret_error_unsupported_resolver() {
        let err = SecretError::UnsupportedResolver {
            resolver: "unknown".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("unknown"));
    }

    #[test]
    fn test_secret_error_debug() {
        let err = SecretError::MissingSalt;
        let debug = format!("{err:?}");
        assert!(debug.contains("MissingSalt"));
    }

    #[test]
    fn test_secret_spec_new() {
        let spec = SecretSpec::new("env:API_KEY");
        assert_eq!(spec.source, "env:API_KEY");
        assert!(!spec.cache_key);
    }

    #[test]
    fn test_secret_spec_with_cache_key() {
        let spec = SecretSpec::with_cache_key("env:CACHE_AFFECTING_SECRET");
        assert_eq!(spec.source, "env:CACHE_AFFECTING_SECRET");
        assert!(spec.cache_key);
    }

    #[test]
    fn test_secret_spec_new_with_string() {
        let spec = SecretSpec::new(String::from("vault://path/to/secret"));
        assert_eq!(spec.source, "vault://path/to/secret");
    }

    #[test]
    fn test_secret_spec_equality() {
        let spec1 = SecretSpec::new("source1");
        let spec2 = SecretSpec::new("source1");
        let spec3 = SecretSpec::new("source2");
        let spec4 = SecretSpec::with_cache_key("source1");

        assert_eq!(spec1, spec2);
        assert_ne!(spec1, spec3);
        assert_ne!(spec1, spec4); // Different cache_key
    }

    #[test]
    fn test_secret_spec_clone() {
        let spec = SecretSpec::with_cache_key("important");
        let cloned = spec.clone();
        assert_eq!(spec, cloned);
    }

    #[test]
    fn test_secret_spec_debug() {
        let spec = SecretSpec::new("test-source");
        let debug = format!("{spec:?}");
        assert!(debug.contains("SecretSpec"));
        assert!(debug.contains("test-source"));
    }

    #[test]
    fn test_secret_spec_serialization() {
        let spec = SecretSpec::with_cache_key("op://vault/item/field");
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("op://vault/item/field"));
        assert!(json.contains("cache_key"));

        let parsed: SecretSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, parsed);
    }

    #[test]
    fn test_secret_spec_deserialization_default_cache_key() {
        let json = r#"{"source": "test"}"#;
        let spec: SecretSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.source, "test");
        assert!(!spec.cache_key); // Default is false
    }
}
