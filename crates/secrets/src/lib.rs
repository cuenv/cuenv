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
//! ```
//! use cuenv_secrets::{EnvSecretResolver, SecretResolver, SecretSpec};
//! use std::collections::HashMap;
//!
//! // Resolve multiple secrets concurrently
//! let resolver = EnvSecretResolver::new();
//! let specs = HashMap::from([("tool-path".to_string(), SecretSpec::new("PATH"))]);
//! let secrets = tokio_test::block_on(resolver.resolve_batch(&specs))?;
//!
//! assert!(secrets.get("tool-path").is_some());
//! // Secrets are zeroed when `secrets` goes out of scope
//! # Ok::<(), cuenv_secrets::SecretError>(())
//! ```

mod batch;
mod credentials;
mod fingerprint;
mod global;
#[cfg(feature = "http")]
pub mod http;
mod registry;
mod resolved;
pub mod resolvers;
mod salt;
mod types;

pub use batch::{BatchConfig, BatchResolver, resolve_batch};
pub use credentials::{
    NamePattern, RESOLVER_ENVIRONMENT_VARIABLES, ResolverVariable, ValueKind,
    is_resolver_credential, is_resolver_environment_variable, resolver_credential_values,
    resolver_environment_variable_names,
};
pub use fingerprint::compute_secret_fingerprint;
pub use global::{global_registry, install_registry_factory};
pub use registry::SecretRegistry;
pub use resolved::ResolvedSecrets;
pub use salt::SaltConfig;
pub use types::{BatchSecrets, SecureSecret};

// Re-export built-in resolvers (no external dependencies)
pub use resolvers::{EnvSecretResolver, ExecSecretResolver};

// Provider implementations are in separate crates:
// - cuenv-1password: OnePasswordResolver, OnePasswordConfig
// - cuenv-infisical: InfisicalResolver, InfisicalConfig

mod resolver;

pub use resolver::{SecretError, SecretResolver, SecretSpec};
