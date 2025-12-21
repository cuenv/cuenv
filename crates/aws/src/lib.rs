//! AWS integration for cuenv
//!
//! This crate provides AWS service integrations for the cuenv ecosystem.
//! Currently supports:
//! - AWS Secrets Manager via the [`secrets`] module

pub mod secrets;

// Re-export main types for convenience
pub use secrets::{AwsResolver, AwsSecretConfig};
