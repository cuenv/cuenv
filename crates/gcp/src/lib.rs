//! GCP integration for cuenv
//!
//! This crate provides GCP service integrations for the cuenv ecosystem.
//! Currently supports:
//! - GCP Secret Manager via the [`secrets`] module

pub mod secrets;

// Re-export main types for convenience
pub use secrets::{GcpResolver, GcpSecretConfig};
