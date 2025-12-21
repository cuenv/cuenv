//! `HashiCorp` Vault integration for cuenv
//!
//! This crate provides `HashiCorp` Vault service integrations for the cuenv ecosystem.
//! Currently supports:
//! - Vault KV v2 secrets via the [`secrets`] module

pub mod secrets;

// Re-export main types for convenience
pub use secrets::{VaultResolver, VaultSecretConfig};
