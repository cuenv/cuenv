//! Infisical integration for cuenv
//!
//! This crate provides Infisical secret resolution for the cuenv ecosystem.

pub mod secrets;

pub use secrets::{InfisicalResolver, InfisicalSecretConfig};
