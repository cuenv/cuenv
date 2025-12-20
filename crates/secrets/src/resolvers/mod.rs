//! Secret resolver implementations
//!
//! This module provides resolvers for various secret providers:
//!
//! - [`EnvSecretResolver`] - Environment variables
//! - [`ExecSecretResolver`] - Command execution
//! - [`AwsResolver`] - AWS Secrets Manager (HTTP + CLI modes)
//! - [`GcpResolver`] - GCP Secret Manager (HTTP + CLI modes)
//! - [`VaultResolver`] - `HashiCorp` Vault (HTTP + CLI modes)
//! - [`OnePasswordResolver`] - 1Password (HTTP via SDK + CLI modes)

mod aws;
mod env;
mod exec;
mod gcp;
mod onepassword;
#[cfg(feature = "onepassword")]
mod onepassword_core;
mod vault;

pub use aws::{AwsResolver, AwsSecretConfig};
pub use env::EnvSecretResolver;
pub use exec::ExecSecretResolver;
pub use gcp::{GcpResolver, GcpSecretConfig};
pub use onepassword::{OnePasswordConfig, OnePasswordResolver};
pub use vault::{VaultResolver, VaultSecretConfig};
