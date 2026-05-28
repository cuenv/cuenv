//! AWS Secrets Manager resolver.

use async_trait::async_trait;
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for resolving a single AWS Secrets Manager secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AwsSecretConfig {
    /// Secret ARN or friendly name.
    pub secret_id: String,

    /// Optional version ID to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,

    /// Optional version stage to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_stage: Option<String>,

    /// Optional key to extract from a JSON `SecretString`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_key: Option<String>,
}

impl AwsSecretConfig {
    /// Create a new AWS Secrets Manager secret config.
    #[must_use]
    pub fn new(secret_id: impl Into<String>) -> Self {
        Self {
            secret_id: secret_id.into(),
            version_id: None,
            version_stage: None,
            json_key: None,
        }
    }

    fn validate(&self, name: &str) -> Result<(), SecretError> {
        if self.secret_id.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "AWS secretId cannot be empty",
            ));
        }
        Ok(())
    }
}

/// Resolves secrets from AWS Secrets Manager.
///
/// The resolver shells out to the AWS CLI, which uses the standard AWS
/// credential and region provider chain (`AWS_*` environment variables,
/// shared config files, profiles, and instance/task roles).
#[derive(Debug, Clone, Default)]
pub struct AwsSecretsManagerResolver;

impl AwsSecretsManagerResolver {
    /// Create a new AWS Secrets Manager resolver.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn parse_config(name: &str, spec: &SecretSpec) -> Result<AwsSecretConfig, SecretError> {
        let config: AwsSecretConfig = serde_json::from_str(&spec.source).map_err(|e| {
            SecretError::resolution_failed(
                name,
                format!("AWS resolver requires structured config: {e}"),
            )
        })?;
        config.validate(name)?;
        Ok(config)
    }

    fn args(config: &AwsSecretConfig) -> Vec<String> {
        let mut args = vec![
            "secretsmanager".to_string(),
            "get-secret-value".to_string(),
            "--secret-id".to_string(),
            config.secret_id.clone(),
            "--output".to_string(),
            "json".to_string(),
        ];

        if let Some(version_id) = &config.version_id {
            args.extend(["--version-id".to_string(), version_id.clone()]);
        }
        if let Some(version_stage) = &config.version_stage {
            args.extend(["--version-stage".to_string(), version_stage.clone()]);
        }

        args
    }

    #[tracing::instrument(name = "aws_secrets_get", level = "debug", skip(config), fields(secret_id = %config.secret_id))]
    async fn execute_aws(name: &str, config: &AwsSecretConfig) -> Result<String, SecretError> {
        tracing::debug!("reading secret from AWS Secrets Manager");
        let output = Command::new("aws")
            .args(Self::args(config))
            .env("AWS_PAGER", "")
            .output()
            .await
            .map_err(|e| {
                SecretError::resolution_failed(name, format!("Failed to execute AWS CLI: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::resolution_failed(
                name,
                format!("AWS Secrets Manager read failed: {stderr}"),
            ));
        }

        String::from_utf8(output.stdout).map_err(|e| {
            SecretError::resolution_failed(name, format!("AWS CLI returned invalid UTF-8: {e}"))
        })
    }

    fn extract_secret(
        name: &str,
        response: &str,
        config: &AwsSecretConfig,
    ) -> Result<String, SecretError> {
        let body: GetSecretValueResponse = serde_json::from_str(response).map_err(|e| {
            SecretError::resolution_failed(name, format!("Failed to parse AWS CLI response: {e}"))
        })?;

        // `jsonKey` extraction only makes sense for a textual `SecretString`.
        // `SecretBinary` is returned as the CLI's base64 string as-is.
        match (body.secret_string, body.secret_binary, config.json_key.as_deref()) {
            (Some(secret), _, Some(key)) => extract_json_key(name, &secret, key),
            (Some(secret), _, None) => Ok(secret),
            (None, Some(_), Some(_)) => Err(SecretError::resolution_failed(
                name,
                "AWS jsonKey requires a SecretString, but the secret only has a SecretBinary value",
            )),
            (None, Some(binary), None) => Ok(binary),
            (None, None, _) => Err(SecretError::resolution_failed(
                name,
                "AWS response did not include a secret value",
            )),
        }
    }
}

#[async_trait]
impl SecretResolver for AwsSecretsManagerResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        let config = Self::parse_config(name, spec)?;
        let response = Self::execute_aws(name, &config).await?;
        Self::extract_secret(name, &response, &config)
    }

    fn provider_name(&self) -> &'static str {
        "aws"
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct GetSecretValueResponse {
    secret_string: Option<String>,
    secret_binary: Option<String>,
}

fn extract_json_key(name: &str, secret: &str, key: &str) -> Result<String, SecretError> {
    let value: serde_json::Value = serde_json::from_str(secret).map_err(|e| {
        SecretError::resolution_failed(name, format!("AWS SecretString is not valid JSON: {e}"))
    })?;

    let extracted = value.get(key).ok_or_else(|| {
        SecretError::resolution_failed(
            name,
            format!("AWS SecretString JSON key '{key}' was not found"),
        )
    })?;

    if extracted.is_null() {
        return Err(SecretError::resolution_failed(
            name,
            format!("AWS SecretString JSON key '{key}' is null"),
        ));
    }

    Ok(extracted
        .as_str()
        .map_or_else(|| extracted.to_string(), ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::{fs, path::Path};

    #[test]
    fn config_defaults() {
        let config = AwsSecretConfig::new("prod/api");

        assert_eq!(config.secret_id, "prod/api");
        assert!(config.version_id.is_none());
        assert!(config.version_stage.is_none());
        assert!(config.json_key.is_none());
    }

    #[test]
    fn config_deserializes_cue_shape() -> Result<(), Box<dyn Error>> {
        let json = r#"{
            "resolver": "aws",
            "secretId": "prod/api",
            "versionId": "version-1",
            "versionStage": "AWSCURRENT",
            "jsonKey": "password"
        }"#;

        let config: AwsSecretConfig = serde_json::from_str(json)?;

        assert_eq!(config.secret_id, "prod/api");
        assert_eq!(config.version_id.as_deref(), Some("version-1"));
        assert_eq!(config.version_stage.as_deref(), Some("AWSCURRENT"));
        assert_eq!(config.json_key.as_deref(), Some("password"));
        Ok(())
    }

    #[test]
    fn extracts_plain_secret_string() -> Result<(), Box<dyn Error>> {
        let config = AwsSecretConfig::new("prod/api");
        let response = r#"{"SecretString":"plain-secret"}"#;

        let secret = AwsSecretsManagerResolver::extract_secret("API_KEY", response, &config)?;

        assert_eq!(secret, "plain-secret");
        Ok(())
    }

    #[test]
    fn extracts_json_key() -> Result<(), Box<dyn Error>> {
        let mut config = AwsSecretConfig::new("prod/api");
        config.json_key = Some("password".to_string());
        let response = r#"{"SecretString":"{\"username\":\"app\",\"password\":\"secret\"}"}"#;

        let secret = AwsSecretsManagerResolver::extract_secret("DATABASE", response, &config)?;

        assert_eq!(secret, "secret");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secure_resolution_redacts_debug_output() -> Result<(), Box<dyn Error>> {
        let resolver = AwsSecretsManagerResolver::new();
        let spec = SecretSpec::new(serde_json::to_string(&AwsSecretConfig::new("prod/api"))?);

        let temp_dir = tempfile::tempdir()?;
        write_fake_aws_shim(temp_dir.path())?;
        let path = prepend_path(temp_dir.path())?;

        temp_env::async_with_vars([("PATH", Some(path.as_str()))], async {
            let secret = resolver.resolve_secure("API_KEY", &spec).await?;
            assert_eq!(format!("{secret:?}"), "[REDACTED]");
            assert_eq!(secret.expose(), "cli-secret");
            Ok::<_, SecretError>(())
        })
        .await?;

        Ok(())
    }

    #[cfg(unix)]
    fn write_fake_aws_shim(dir: &Path) -> Result<(), Box<dyn Error>> {
        let aws_path = dir.join("aws");
        let script = r#"#!/bin/sh
if [ "$1" != "secretsmanager" ] || [ "$2" != "get-secret-value" ]; then
  printf "unexpected command\n" >&2
  exit 2
fi
printf '{"SecretString":"cli-secret"}\n'
"#;

        fs::write(&aws_path, script)?;
        let mut perms = fs::metadata(&aws_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&aws_path, perms)?;
        Ok(())
    }

    #[cfg(unix)]
    fn prepend_path(dir: &Path) -> Result<String, Box<dyn Error>> {
        let mut parts = vec![dir.to_path_buf()];
        if let Some(current) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&current));
        }
        Ok(std::env::join_paths(parts)?
            .to_string_lossy()
            .into_owned())
    }
}
