//! Environment variable secret resolver

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;

/// Resolves secrets from environment variables
///
/// The `source` field in [`SecretSpec`] is interpreted as the environment variable name.
#[derive(Debug, Clone, Default)]
pub struct EnvSecretResolver;

impl EnvSecretResolver {
    /// Create a new environment variable resolver
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecretResolver for EnvSecretResolver {
    fn provider_name(&self) -> &'static str {
        "env"
    }

    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        std::env::var(&spec.source).map_err(|_| SecretError::NotFound {
            name: name.to_string(),
            secret_source: spec.source.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_resolve_from_env() {
        temp_env::async_with_vars([("TEST_SECRET_ENV_1", Some("value1"))], async {
            let resolver = EnvSecretResolver::new();
            let spec = SecretSpec::new("TEST_SECRET_ENV_1");
            let result = resolver.resolve("secret1", &spec).await;

            assert_eq!(result.unwrap(), "value1");
        })
        .await;
    }

    #[tokio::test]
    async fn test_missing_env_var() {
        let resolver = EnvSecretResolver::new();
        let spec = SecretSpec::new("NONEXISTENT_ENV_VAR_12345");
        let result = resolver.resolve("missing", &spec).await;

        assert!(matches!(result, Err(SecretError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_resolve_all() {
        temp_env::async_with_vars(
            [
                ("TEST_SECRET_ENV_2", Some("value2")),
                ("TEST_SECRET_ENV_3", Some("value3")),
            ],
            async {
                let resolver = EnvSecretResolver::new();
                let secrets = HashMap::from([
                    ("secret2".to_string(), SecretSpec::new("TEST_SECRET_ENV_2")),
                    ("secret3".to_string(), SecretSpec::new("TEST_SECRET_ENV_3")),
                ]);

                let result = resolver.resolve_all(&secrets).await.unwrap();

                assert_eq!(result.get("secret2"), Some(&"value2".to_string()));
                assert_eq!(result.get("secret3"), Some(&"value3".to_string()));
            },
        )
        .await;
    }
}
