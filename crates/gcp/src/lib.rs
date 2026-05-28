//! Google Cloud integration for cuenv.
//!
//! This crate provides Google Cloud Secret Manager resolution.

pub mod secrets;

pub use secrets::{GcpSecretConfig, GcpSecretManagerResolver};
