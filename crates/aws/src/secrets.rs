//! AWS Secrets Manager secret resolver with auto-negotiating dual-mode (HTTP + CLI)

// AWS SDK and CLI dual-mode resolver with complex batch operations
#![allow(clippy::cognitive_complexity, clippy::too_many_lines)]

use async_trait::async_trait;
use aws_sdk_secretsmanager::Client;
use aws_smithy_http_client::{Builder as SmithyHttpClientBuilder, tls};
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec, SecureSecret};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::process::Command;

/// Configuration for AWS Secrets Manager resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AwsSecretConfig {
    /// Secret ID - can be ARN or secret name
    pub secret_id: String,

    /// Version ID (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,

    /// Version stage (optional, defaults to AWSCURRENT)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_stage: Option<String>,

    /// JSON key to extract (if secret value is JSON)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_key: Option<String>,
}

impl AwsSecretConfig {
    /// Create a new AWS secret config with just the secret ID
    #[must_use]
    pub fn new(secret_id: impl Into<String>) -> Self {
        Self {
            secret_id: secret_id.into(),
            version_id: None,
            version_stage: None,
            json_key: None,
        }
    }
}

/// Resolves secrets from AWS Secrets Manager
///
/// Mode is auto-negotiated based on environment:
/// - If `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` are set → HTTP mode
/// - Otherwise → CLI mode (uses `aws` CLI)
///
/// The `source` field in [`SecretSpec`] can be:
/// - A simple secret ID (name or ARN)
/// - A JSON-encoded [`AwsSecretConfig`] for advanced options
pub struct AwsResolver {
    http_client: Option<Client>,
}

impl std::fmt::Debug for AwsResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsResolver")
            .field("mode", &if self.can_use_http() { "http" } else { "cli" })
            .finish()
    }
}

impl AwsResolver {
    /// Create a new AWS resolver with auto-detected mode
    ///
    /// If AWS credentials are available in environment, initializes HTTP client.
    /// Otherwise, CLI mode will be used.
    ///
    /// # Errors
    /// Returns error if AWS configuration cannot be loaded.
    pub async fn new() -> Result<Self, SecretError> {
        let http_client = if Self::http_credentials_available() {
            // Force the ring-backed rustls provider to avoid aws-lc in zig builds.
            let http_client = SmithyHttpClientBuilder::new()
                .tls_provider(tls::Provider::Rustls(
                    tls::rustls_provider::CryptoMode::Ring,
                ))
                .build_https();
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .http_client(http_client)
                .load()
                .await;
            Some(Client::new(&config))
        } else {
            None
        };

        Ok(Self { http_client })
    }

    /// Check if HTTP credentials are available in environment
    fn http_credentials_available() -> bool {
        std::env::var("AWS_ACCESS_KEY_ID").is_ok() && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    const fn can_use_http(&self) -> bool {
        self.http_client.is_some()
    }

    /// Resolve using the AWS SDK (HTTP mode)
    async fn resolve_http(
        &self,
        name: &str,
        config: &AwsSecretConfig,
    ) -> Result<String, SecretError> {
        let client = self
            .http_client
            .as_ref()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "HTTP client not available".to_string(),
            })?;

        let mut request = client.get_secret_value().secret_id(&config.secret_id);

        if let Some(version_id) = &config.version_id {
            request = request.version_id(version_id);
        }

        if let Some(version_stage) = &config.version_stage {
            request = request.version_stage(version_stage);
        }

        let response = request
            .send()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("AWS Secrets Manager error: {e}"),
            })?;

        let secret_string =
            response
                .secret_string()
                .ok_or_else(|| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: "Secret has no string value (may be binary)".to_string(),
                })?;

        Self::extract_json_key(name, secret_string, config.json_key.as_ref())
    }

    /// Resolve using the AWS CLI
    async fn resolve_cli(
        &self,
        name: &str,
        config: &AwsSecretConfig,
    ) -> Result<String, SecretError> {
        let mut args = vec![
            "secretsmanager".to_string(),
            "get-secret-value".to_string(),
            "--secret-id".to_string(),
            config.secret_id.clone(),
            "--query".to_string(),
            "SecretString".to_string(),
            "--output".to_string(),
            "text".to_string(),
        ];

        if let Some(version_id) = &config.version_id {
            args.push("--version-id".to_string());
            args.push(version_id.clone());
        }

        if let Some(version_stage) = &config.version_stage {
            args.push("--version-stage".to_string());
            args.push(version_stage.clone());
        }

        let output = Command::new("aws")
            .args(&args)
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute aws CLI: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("aws CLI failed: {stderr}"),
            });
        }

        let secret_string = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Self::extract_json_key(name, &secret_string, config.json_key.as_ref())
    }

    /// Extract a specific key from JSON secret value
    fn extract_json_key(
        name: &str,
        secret_string: &str,
        json_key: Option<&String>,
    ) -> Result<String, SecretError> {
        if let Some(key) = json_key {
            let parsed: serde_json::Value =
                serde_json::from_str(secret_string).map_err(|e| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: format!("Secret is not valid JSON: {e}"),
                })?;

            let value = parsed
                .get(key)
                .ok_or_else(|| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: format!("JSON key '{key}' not found in secret"),
                })?;

            return match value {
                serde_json::Value::String(s) => Ok(s.clone()),
                other => Ok(other.to_string()),
            };
        }

        Ok(secret_string.to_string())
    }

    /// Resolve a secret - tries HTTP first if available, falls back to CLI
    async fn resolve_with_config(
        &self,
        name: &str,
        config: &AwsSecretConfig,
    ) -> Result<String, SecretError> {
        // Try HTTP mode if available
        if self.http_client.is_some() {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }

    /// Resolve multiple secrets using `BatchGetSecretValue` (HTTP mode only)
    async fn resolve_batch_http(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        use futures::future::try_join_all;

        let client = self
            .http_client
            .as_ref()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: "HTTP client not available".to_string(),
            })?;

        // Parse all configs and group by secret_id
        // Build mapping: secret_id -> Vec<(name, config)>
        let mut id_to_names: HashMap<String, Vec<(String, AwsSecretConfig)>> = HashMap::new();
        for (name, spec) in secrets {
            let config = serde_json::from_str::<AwsSecretConfig>(&spec.source)
                .unwrap_or_else(|_| AwsSecretConfig::new(&spec.source));
            id_to_names
                .entry(config.secret_id.clone())
                .or_default()
                .push((name.clone(), config));
        }

        // Extract unique secret IDs
        let secret_ids: Vec<String> = id_to_names.keys().cloned().collect();

        // AWS BatchGetSecretValue can fetch up to 20 secrets per call
        let mut all_values: HashMap<String, String> = HashMap::new();

        for chunk in secret_ids.chunks(20) {
            let response = client
                .batch_get_secret_value()
                .set_secret_id_list(Some(chunk.to_vec()))
                .send()
                .await
                .map_err(|e| SecretError::ResolutionFailed {
                    name: "batch".to_string(),
                    message: format!("AWS BatchGetSecretValue failed: {e}"),
                })?;

            // Process successful responses
            for sv in response.secret_values() {
                if let Some(secret_string) = sv.secret_string() {
                    // Use name or ARN as key
                    if let Some(secret_name) = sv.name() {
                        all_values.insert(secret_name.to_string(), secret_string.to_string());
                    }
                    if let Some(arn) = sv.arn() {
                        all_values.insert(arn.to_string(), secret_string.to_string());
                    }
                }
            }

            // Log any errors
            for err in response.errors() {
                tracing::warn!(
                    secret_id = ?err.secret_id(),
                    error_code = ?err.error_code(),
                    message = ?err.message(),
                    "Failed to retrieve secret in batch"
                );
            }
        }

        // Map batch results back to original names with JSON key extraction
        let extract_futures: Vec<_> = secrets
            .iter()
            .map(|(name, spec)| {
                let name = name.clone();
                let all_values = &all_values;
                async move {
                    let config = serde_json::from_str::<AwsSecretConfig>(&spec.source)
                        .unwrap_or_else(|_| AwsSecretConfig::new(&spec.source));

                    // Find the secret value by ID
                    let secret_string = all_values.get(&config.secret_id).ok_or_else(|| {
                        SecretError::ResolutionFailed {
                            name: name.clone(),
                            message: format!(
                                "Secret '{}' not found in batch response",
                                config.secret_id
                            ),
                        }
                    })?;

                    // Extract JSON key if specified
                    let value =
                        Self::extract_json_key(&name, secret_string, config.json_key.as_ref())?;
                    Ok::<_, SecretError>((name, SecureSecret::new(value)))
                }
            })
            .collect();

        try_join_all(extract_futures)
            .await
            .map(|v| v.into_iter().collect())
    }

    /// Resolve multiple secrets using CLI (fallback, concurrent)
    async fn resolve_batch_cli(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        use futures::future::try_join_all;

        let futures: Vec<_> = secrets
            .iter()
            .map(|(name, spec)| {
                let name = name.clone();
                let spec = spec.clone();
                async move {
                    let value = self.resolve(&name, &spec).await?;
                    Ok::<_, SecretError>((name, SecureSecret::new(value)))
                }
            })
            .collect();

        try_join_all(futures).await.map(|v| v.into_iter().collect())
    }
}

#[async_trait]
impl SecretResolver for AwsResolver {
    fn provider_name(&self) -> &'static str {
        "aws"
    }

    fn supports_native_batch(&self) -> bool {
        // AWS Secrets Manager supports BatchGetSecretValue
        true
    }

    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON AwsSecretConfig
        if let Ok(config) = serde_json::from_str::<AwsSecretConfig>(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        // Fallback: treat source as a simple secret ID
        let config = AwsSecretConfig::new(&spec.source);
        self.resolve_with_config(name, &config).await
    }

    async fn resolve_batch(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        if secrets.is_empty() {
            return Ok(HashMap::new());
        }

        // Use BatchGetSecretValue if HTTP mode is available
        if self.http_client.is_some() {
            return self.resolve_batch_http(secrets).await;
        }

        // Fallback to concurrent CLI calls
        self.resolve_batch_cli(secrets).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aws_config_serialization() {
        let config = AwsSecretConfig {
            secret_id: "my-secret".to_string(),
            version_id: Some("v1".to_string()),
            version_stage: None,
            json_key: Some("password".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: AwsSecretConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_simple_config() {
        let config = AwsSecretConfig::new("arn:aws:secretsmanager:us-east-1:123456:secret:test");
        assert_eq!(
            config.secret_id,
            "arn:aws:secretsmanager:us-east-1:123456:secret:test"
        );
        assert!(config.version_id.is_none());
        assert!(config.json_key.is_none());
    }

    #[test]
    fn test_http_credentials_check() {
        // This test just ensures the function exists and doesn't panic
        let _ = AwsResolver::http_credentials_available();
    }

    #[test]
    fn test_aws_config_new_with_string_slice() {
        let config = AwsSecretConfig::new("my-secret");
        assert_eq!(config.secret_id, "my-secret");
        assert!(config.version_id.is_none());
        assert!(config.version_stage.is_none());
        assert!(config.json_key.is_none());
    }

    #[test]
    fn test_aws_config_full_serialization() {
        let config = AwsSecretConfig {
            secret_id: "my-secret".to_string(),
            version_id: Some("abc123".to_string()),
            version_stage: Some("AWSCURRENT".to_string()),
            json_key: Some("api_key".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"secretId\":\"my-secret\""));
        assert!(json.contains("\"versionId\":\"abc123\""));
        assert!(json.contains("\"versionStage\":\"AWSCURRENT\""));
        assert!(json.contains("\"jsonKey\":\"api_key\""));
    }

    #[test]
    fn test_aws_config_minimal_serialization() {
        let config = AwsSecretConfig::new("simple-secret");
        let json = serde_json::to_string(&config).unwrap();
        // Optional fields should not be present
        assert!(!json.contains("versionId"));
        assert!(!json.contains("versionStage"));
        assert!(!json.contains("jsonKey"));
    }

    #[test]
    fn test_extract_json_key_string_value() {
        let secret = r#"{"username": "admin", "password": "secret123"}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"password".to_string()));
        assert_eq!(result.unwrap(), "secret123");
    }

    #[test]
    fn test_extract_json_key_number_value() {
        let secret = r#"{"port": 5432, "host": "localhost"}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"port".to_string()));
        assert_eq!(result.unwrap(), "5432");
    }

    #[test]
    fn test_extract_json_key_boolean_value() {
        let secret = r#"{"enabled": true, "debug": false}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"enabled".to_string()));
        assert_eq!(result.unwrap(), "true");
    }

    #[test]
    fn test_extract_json_key_no_key_returns_full_secret() {
        let secret = r#"{"username": "admin"}"#;
        let result = AwsResolver::extract_json_key("test", secret, None);
        assert_eq!(result.unwrap(), secret);
    }

    #[test]
    fn test_extract_json_key_plain_string_no_key() {
        let secret = "plain-text-secret";
        let result = AwsResolver::extract_json_key("test", secret, None);
        assert_eq!(result.unwrap(), "plain-text-secret");
    }

    #[test]
    fn test_extract_json_key_missing_key_error() {
        let secret = r#"{"username": "admin"}"#;
        let result =
            AwsResolver::extract_json_key("test", secret, Some(&"nonexistent".to_string()));
        assert!(result.is_err());
        if let Err(SecretError::ResolutionFailed { message, .. }) = result {
            assert!(message.contains("JSON key 'nonexistent' not found"));
        } else {
            panic!("Expected ResolutionFailed error");
        }
    }

    #[test]
    fn test_extract_json_key_invalid_json_error() {
        let secret = "not-valid-json";
        let result = AwsResolver::extract_json_key("test", secret, Some(&"key".to_string()));
        assert!(result.is_err());
        if let Err(SecretError::ResolutionFailed { message, .. }) = result {
            assert!(message.contains("Secret is not valid JSON"));
        } else {
            panic!("Expected ResolutionFailed error");
        }
    }

    #[test]
    fn test_extract_json_key_nested_object() {
        let secret = r#"{"database": {"host": "localhost", "port": 5432}}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"database".to_string()));
        // Should return the object as a string
        let value = result.unwrap();
        assert!(value.contains("host"));
        assert!(value.contains("localhost"));
    }

    #[test]
    fn test_extract_json_key_array_value() {
        let secret = r#"{"hosts": ["host1", "host2", "host3"]}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"hosts".to_string()));
        let value = result.unwrap();
        assert!(value.contains("host1"));
        assert!(value.contains("host2"));
    }

    #[test]
    fn test_extract_json_key_null_value() {
        let secret = r#"{"value": null}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"value".to_string()));
        assert_eq!(result.unwrap(), "null");
    }

    #[test]
    fn test_aws_config_clone() {
        let config = AwsSecretConfig {
            secret_id: "my-secret".to_string(),
            version_id: Some("v1".to_string()),
            version_stage: Some("AWSCURRENT".to_string()),
            json_key: Some("key".to_string()),
        };
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_aws_config_debug() {
        let config = AwsSecretConfig::new("test-secret");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("AwsSecretConfig"));
        assert!(debug_str.contains("test-secret"));
    }

    #[test]
    fn test_aws_config_equality() {
        let config1 = AwsSecretConfig::new("secret-1");
        let config2 = AwsSecretConfig::new("secret-1");
        let config3 = AwsSecretConfig::new("secret-2");

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }

    #[test]
    fn test_aws_config_with_version_id_equality() {
        let mut config1 = AwsSecretConfig::new("secret");
        config1.version_id = Some("v1".to_string());
        let mut config2 = AwsSecretConfig::new("secret");
        config2.version_id = Some("v1".to_string());
        let mut config3 = AwsSecretConfig::new("secret");
        config3.version_id = Some("v2".to_string());

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }

    #[test]
    fn test_aws_config_deserialization_from_json() {
        let json = r#"{"secretId": "my-secret", "versionId": "abc", "jsonKey": "password"}"#;
        let config: AwsSecretConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.secret_id, "my-secret");
        assert_eq!(config.version_id, Some("abc".to_string()));
        assert_eq!(config.json_key, Some("password".to_string()));
        assert!(config.version_stage.is_none());
    }

    #[test]
    fn test_aws_config_deserialization_minimal() {
        let json = r#"{"secretId": "just-the-id"}"#;
        let config: AwsSecretConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.secret_id, "just-the-id");
        assert!(config.version_id.is_none());
        assert!(config.version_stage.is_none());
        assert!(config.json_key.is_none());
    }

    #[test]
    fn test_aws_config_deserialization_missing_secret_id() {
        let json = r#"{"versionId": "v1"}"#;
        let result = serde_json::from_str::<AwsSecretConfig>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_aws_config_with_arn() {
        let arn = "arn:aws:secretsmanager:us-west-2:123456789012:secret:my-secret-abc123";
        let config = AwsSecretConfig::new(arn);
        assert_eq!(config.secret_id, arn);
    }

    #[test]
    fn test_aws_config_roundtrip() {
        let original = AwsSecretConfig {
            secret_id: "test-secret".to_string(),
            version_id: Some("v1".to_string()),
            version_stage: Some("AWSPREVIOUS".to_string()),
            json_key: Some("key".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: AwsSecretConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_extract_json_key_empty_string_value() {
        let secret = r#"{"key": ""}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"key".to_string()));
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_extract_json_key_special_characters() {
        let secret = r#"{"key": "value with \"quotes\" and \n newlines"}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"key".to_string()));
        assert!(result.is_ok());
        let value = result.unwrap();
        assert!(value.contains("quotes"));
    }

    #[test]
    fn test_extract_json_key_unicode() {
        let secret = r#"{"密码": "秘密值"}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"密码".to_string()));
        assert_eq!(result.unwrap(), "秘密值");
    }

    #[test]
    fn test_extract_json_key_numeric_string() {
        let secret = r#"{"key": "12345"}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"key".to_string()));
        assert_eq!(result.unwrap(), "12345");
    }

    #[test]
    fn test_extract_json_key_float_value() {
        let secret = r#"{"rate": 3.14159}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"rate".to_string()));
        let value = result.unwrap();
        assert!(value.starts_with("3.14"));
    }

    #[tokio::test]
    async fn test_resolver_new_without_credentials() {
        // Without AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, should use CLI mode
        // This test just verifies the resolver can be created
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await;
            assert!(resolver.is_ok());
            let resolver = resolver.unwrap();
            assert!(!resolver.can_use_http());
        }
    }

    #[tokio::test]
    async fn test_resolver_provider_name() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await.unwrap();
            assert_eq!(resolver.provider_name(), "aws");
        }
    }

    #[tokio::test]
    async fn test_resolver_supports_native_batch() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await.unwrap();
            assert!(resolver.supports_native_batch());
        }
    }

    #[tokio::test]
    async fn test_resolver_debug_output() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await.unwrap();
            let debug = format!("{resolver:?}");
            assert!(debug.contains("AwsResolver"));
            assert!(debug.contains("cli") || debug.contains("http"));
        }
    }

    #[tokio::test]
    async fn test_resolve_batch_empty() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await.unwrap();
            let empty: HashMap<String, SecretSpec> = HashMap::new();
            let result = resolver.resolve_batch(&empty).await;
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        }
    }

    #[test]
    fn test_http_credentials_available_logic() {
        // Test the logic directly
        let key_id = std::env::var("AWS_ACCESS_KEY_ID").is_ok();
        let secret = std::env::var("AWS_SECRET_ACCESS_KEY").is_ok();
        let expected = key_id && secret;
        assert_eq!(AwsResolver::http_credentials_available(), expected);
    }

    #[test]
    fn test_aws_config_from_string_type() {
        let owned_string = String::from("my-secret-id");
        let config = AwsSecretConfig::new(owned_string);
        assert_eq!(config.secret_id, "my-secret-id");
    }

    #[test]
    fn test_aws_config_serialization_skips_none() {
        let config = AwsSecretConfig {
            secret_id: "secret".to_string(),
            version_id: None,
            version_stage: None,
            json_key: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        // None fields should be skipped
        assert!(!json.contains("versionId"));
        assert!(!json.contains("versionStage"));
        assert!(!json.contains("jsonKey"));
        // Only secretId should be present
        assert!(json.contains("secretId"));
    }

    #[test]
    fn test_aws_config_with_all_version_options() {
        let config = AwsSecretConfig {
            secret_id: "prod/db/password".to_string(),
            version_id: Some("version-1234".to_string()),
            version_stage: Some("AWSPREVIOUS".to_string()),
            json_key: Some("connection_string".to_string()),
        };

        // Verify all fields are set
        assert_eq!(config.secret_id, "prod/db/password");
        assert_eq!(config.version_id.as_deref(), Some("version-1234"));
        assert_eq!(config.version_stage.as_deref(), Some("AWSPREVIOUS"));
        assert_eq!(config.json_key.as_deref(), Some("connection_string"));
    }

    #[test]
    fn test_extract_json_key_deeply_nested() {
        let secret = r#"{"level1": {"level2": {"level3": "deep_value"}}}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"level1".to_string()));
        let value = result.unwrap();
        // Should return the entire level1 object as JSON
        assert!(value.contains("level2"));
        assert!(value.contains("level3"));
        assert!(value.contains("deep_value"));
    }

    #[test]
    fn test_extract_json_key_mixed_types_array() {
        let secret = r#"{"mixed": [1, "two", true, null]}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"mixed".to_string()));
        let value = result.unwrap();
        assert!(value.contains('1'));
        assert!(value.contains("two"));
        assert!(value.contains("true"));
        assert!(value.contains("null"));
    }

    #[test]
    fn test_extract_json_key_empty_object() {
        let secret = r#"{"empty": {}}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"empty".to_string()));
        assert_eq!(result.unwrap(), "{}");
    }

    #[test]
    fn test_extract_json_key_empty_array() {
        let secret = r#"{"empty": []}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"empty".to_string()));
        assert_eq!(result.unwrap(), "[]");
    }

    #[test]
    fn test_extract_json_key_whitespace_in_key() {
        let secret = r#"{"key with spaces": "value"}"#;
        let result =
            AwsResolver::extract_json_key("test", secret, Some(&"key with spaces".to_string()));
        assert_eq!(result.unwrap(), "value");
    }

    #[test]
    fn test_aws_config_deserialization_extra_fields_ignored() {
        let json = r#"{"secretId": "my-secret", "unknownField": "ignored", "anotherUnknown": 42}"#;
        let config: AwsSecretConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.secret_id, "my-secret");
    }

    #[test]
    fn test_aws_config_empty_secret_id() {
        let config = AwsSecretConfig::new("");
        assert_eq!(config.secret_id, "");
    }

    #[test]
    fn test_aws_config_secret_id_with_slashes() {
        let config = AwsSecretConfig::new("prod/database/credentials");
        assert_eq!(config.secret_id, "prod/database/credentials");
    }

    #[test]
    fn test_extract_json_key_negative_number() {
        let secret = r#"{"count": -42}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"count".to_string()));
        assert_eq!(result.unwrap(), "-42");
    }

    #[test]
    fn test_extract_json_key_large_number() {
        let secret = r#"{"large": 9999999999999999999}"#;
        let result = AwsResolver::extract_json_key("test", secret, Some(&"large".to_string()));
        // Large numbers might be represented differently
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolver_can_use_http_matches_client_presence() {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err()
            || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
        {
            let resolver = AwsResolver::new().await.unwrap();
            // When no credentials, http_client is None, can_use_http should be false
            assert!(!resolver.can_use_http());
            assert!(resolver.http_client.is_none());
        }
    }
}
