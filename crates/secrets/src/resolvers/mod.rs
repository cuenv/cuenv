//! Secret resolver implementations
//!
//! This module provides built-in resolvers that require no external dependencies:
//!
//! - [`EnvSecretResolver`] - Environment variables
//! - [`ExecSecretResolver`] - Command execution
//!
//! Additional providers are available via feature flags:
//!
//! - `aws` - AWS Secrets Manager (cuenv-aws crate)
//! - `gcp` - GCP Secret Manager (cuenv-gcp crate)
//! - `vault` - `HashiCorp` Vault (cuenv-vault crate)
//! - `onepassword` - 1Password (cuenv-1password crate)

mod env;
mod exec;

pub use env::EnvSecretResolver;
pub use exec::ExecSecretResolver;
