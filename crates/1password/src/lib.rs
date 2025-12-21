//! 1Password integration for cuenv
//!
//! This crate provides 1Password service integrations for the cuenv ecosystem.
//! Currently supports:
//! - 1Password Secrets via the [`secrets`] module

pub mod secrets;

// Re-export main types for convenience
pub use secrets::{OnePasswordConfig, OnePasswordResolver};
