//! AWS Secrets Manager secret resolution.

mod resolver;

pub use resolver::{AwsSecretConfig, AwsSecretsManagerResolver};
