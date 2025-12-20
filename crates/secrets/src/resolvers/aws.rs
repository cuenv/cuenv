//! AWS Secrets Manager secret resolver with auto-negotiating dual-mode (HTTP + CLI)

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[cfg(feature = "aws")]
use aws_sdk_secretsmanager::Client;

/// Configuration for AWS Secrets Manager resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    #[cfg(feature = "aws")]
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
    pub async fn new() -> Result<Self, SecretError> {
        #[cfg(feature = "aws")]
        let http_client = if Self::http_credentials_available() {
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .load()
                .await;
            Some(Client::new(&config))
        } else {
            None
        };

        Ok(Self {
            #[cfg(feature = "aws")]
            http_client,
        })
    }

    /// Check if HTTP credentials are available in environment
    fn http_credentials_available() -> bool {
        std::env::var("AWS_ACCESS_KEY_ID").is_ok()
            && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    fn can_use_http(&self) -> bool {
        #[cfg(feature = "aws")]
        {
            self.http_client.is_some()
        }
        #[cfg(not(feature = "aws"))]
        {
            false
        }
    }

    /// Resolve using the AWS SDK (HTTP mode)
    #[cfg(feature = "aws")]
    async fn resolve_http(
        &self,
        name: &str,
        config: &AwsSecretConfig,
    ) -> Result<String, SecretError> {
        let client = self.http_client.as_ref().ok_or_else(|| SecretError::ResolutionFailed {
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

        let response = request.send().await.map_err(|e| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: format!("AWS Secrets Manager error: {e}"),
        })?;

        let secret_string = response
            .secret_string()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Secret has no string value (may be binary)".to_string(),
            })?;

        self.extract_json_key(name, secret_string, &config.json_key)
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
        self.extract_json_key(name, &secret_string, &config.json_key)
    }

    /// Extract a specific key from JSON secret value
    fn extract_json_key(
        &self,
        name: &str,
        secret_string: &str,
        json_key: &Option<String>,
    ) -> Result<String, SecretError> {
        if let Some(key) = json_key {
            let parsed: serde_json::Value =
                serde_json::from_str(secret_string).map_err(|e| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: format!("Secret is not valid JSON: {e}"),
                })?;

            let value = parsed.get(key).ok_or_else(|| SecretError::ResolutionFailed {
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
        #[cfg(feature = "aws")]
        if self.http_client.is_some() {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }
}

#[async_trait]
impl SecretResolver for AwsResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON AwsSecretConfig
        if let Ok(config) = serde_json::from_str::<AwsSecretConfig>(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        // Fallback: treat source as a simple secret ID
        let config = AwsSecretConfig::new(&spec.source);
        self.resolve_with_config(name, &config).await
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
}
