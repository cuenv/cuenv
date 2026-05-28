//! AWS integration for cuenv.
//!
//! This crate provides AWS Secrets Manager resolution via the AWS CLI.

pub mod secrets;

pub use secrets::{AwsSecretConfig, AwsSecretsManagerResolver};
