//! Infisical integration for cuenv.
//!
//! This crate provides Infisical secret resolution over Infisical's REST API.

pub mod secrets;

pub use secrets::{InfisicalConfig, InfisicalResolver};
