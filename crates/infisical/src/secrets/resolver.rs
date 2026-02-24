//! Infisical secret resolver with SDK + CLI support.

use async_trait::async_trait;
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec};
use infisical::secrets::GetSecretRequest;
use infisical::{AuthMethod, Client};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for Infisical secret resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InfisicalSecretConfig {
    /// Full secret path, including secret key (e.g. "/team/app/API_KEY").
    pub path: String,
    /// Infisical environment name (e.g. "development", "production").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Infisical project identifier.
    #[serde(skip_serializing_if = "Option::is_none", rename = "projectId")]
    pub project_id: Option<String>,
}

impl InfisicalSecretConfig {
    /// Create a new Infisical secret config.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            environment: None,
            project_id: None,
        }
    }
}

#[derive(Debug, Clone)]
enum InfisicalMode {
    Sdk {
        client_id: String,
        client_secret: String,
    },
    Cli,
}

/// Resolves secrets from Infisical.
///
/// - SDK mode: used when `INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET` are set.
/// - CLI mode: fallback when SDK credentials are not present.
#[derive(Debug, Clone)]
pub struct InfisicalResolver {
    mode: InfisicalMode,
}

impl Default for InfisicalResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl InfisicalResolver {
    /// Create a new Infisical resolver with auto-detected mode.
    #[must_use]
    pub fn new() -> Self {
        let client_id = std::env::var("INFISICAL_CLIENT_ID").ok();
        let client_secret = std::env::var("INFISICAL_CLIENT_SECRET").ok();

        let mode = match (client_id, client_secret) {
            (Some(id), Some(secret)) if !id.trim().is_empty() && !secret.trim().is_empty() => {
                InfisicalMode::Sdk {
                    client_id: id,
                    client_secret: secret,
                }
            }
            _ => InfisicalMode::Cli,
        };

        Self { mode }
    }

    fn parse_config(
        &self,
        name: &str,
        spec: &SecretSpec,
    ) -> Result<InfisicalSecretConfig, SecretError> {
        if let Ok(config) = serde_json::from_str::<InfisicalSecretConfig>(&spec.source) {
            return Ok(config);
        }

        // Fallback: treat the source as the path directly.
        let source = spec.source.trim();
        if source.is_empty() {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Infisical secret source is empty".to_string(),
            });
        }

        Ok(InfisicalSecretConfig::new(source))
    }

    fn split_path<'a>(&self, name: &str, path: &'a str) -> Result<(&'a str, &'a str), SecretError> {
        let cleaned = path.trim();
        if cleaned.is_empty() {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Infisical secret path is empty".to_string(),
            });
        }

        let cleaned = cleaned.trim_end_matches('/');
        let Some((folder, key)) = cleaned.rsplit_once('/') else {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!(
                    "Infisical path '{}' must include a folder and a key (e.g. /team/app/API_KEY)",
                    path
                ),
            });
        };

        if key.is_empty() {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Infisical path '{}' has an empty key segment", path),
            });
        }

        let folder = if folder.is_empty() { "/" } else { folder };
        Ok((folder, key))
    }

    fn required_environment<'a>(
        &self,
        name: &str,
        config: &'a InfisicalSecretConfig,
    ) -> Result<&'a str, SecretError> {
        let Some(environment) = config.environment.as_deref() else {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message:
                    "Infisical secret is missing 'environment'. Set it explicitly or via cuenv preprocessing."
                        .to_string(),
            });
        };

        if environment.trim().is_empty() {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Infisical 'environment' cannot be empty".to_string(),
            });
        }

        Ok(environment)
    }

    fn required_project_id<'a>(
        &self,
        name: &str,
        config: &'a InfisicalSecretConfig,
    ) -> Result<&'a str, SecretError> {
        let Some(project_id) = config.project_id.as_deref() else {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message:
                    "Infisical secret is missing 'projectId'. Set it explicitly or via cuenv preprocessing."
                        .to_string(),
            });
        };

        if project_id.trim().is_empty() {
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Infisical 'projectId' cannot be empty".to_string(),
            });
        }

        Ok(project_id)
    }

    async fn resolve_sdk(
        &self,
        name: &str,
        config: &InfisicalSecretConfig,
        client_id: &str,
        client_secret: &str,
    ) -> Result<String, SecretError> {
        let (secret_path, secret_name) = self.split_path(name, &config.path)?;
        let environment = self.required_environment(name, config)?;
        let project_id = self.required_project_id(name, config)?;

        let mut client =
            Client::builder()
                .build()
                .await
                .map_err(|e| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: format!("Failed to initialize Infisical SDK client: {e}"),
                })?;

        let auth_method =
            AuthMethod::new_universal_auth(client_id.to_string(), client_secret.to_string());
        client
            .login(auth_method)
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Infisical SDK universal auth login failed: {e}"),
            })?;

        let request = GetSecretRequest::builder(secret_name, project_id, environment)
            .path(secret_path)
            .build();

        let response =
            client
                .secrets()
                .get(request)
                .await
                .map_err(|e| SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: format!("Infisical SDK secret lookup failed: {e}"),
                })?;

        Ok(response.secret_value)
    }

    async fn resolve_cli(
        &self,
        name: &str,
        config: &InfisicalSecretConfig,
    ) -> Result<String, SecretError> {
        let (secret_path, secret_name) = self.split_path(name, &config.path)?;
        let environment = self.required_environment(name, config)?;
        let project_id = self.required_project_id(name, config)?;

        let output = Command::new("infisical")
            .args([
                "secrets",
                "get",
                secret_name,
                "--path",
                secret_path,
                "--env",
                environment,
                "--projectId",
                project_id,
                "--plain",
            ])
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute infisical CLI: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("infisical CLI failed: {stderr}"),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[async_trait]
impl SecretResolver for InfisicalResolver {
    fn provider_name(&self) -> &'static str {
        "infisical"
    }

    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        let config = self.parse_config(name, spec)?;

        match &self.mode {
            InfisicalMode::Sdk {
                client_id,
                client_secret,
            } => {
                self.resolve_sdk(name, &config, client_id, client_secret)
                    .await
            }
            InfisicalMode::Cli => self.resolve_cli(name, &config).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_config() {
        let resolver = InfisicalResolver::new();
        let spec = SecretSpec::new(
            r#"{"path":"/team/app/API_KEY","environment":"development","projectId":"proj_123"}"#,
        );

        let config = resolver.parse_config("API_KEY", &spec).unwrap();
        assert_eq!(config.path, "/team/app/API_KEY");
        assert_eq!(config.environment.as_deref(), Some("development"));
        assert_eq!(config.project_id.as_deref(), Some("proj_123"));
    }

    #[test]
    fn parses_plain_source_as_path() {
        let resolver = InfisicalResolver::new();
        let spec = SecretSpec::new("/team/app/API_KEY");
        let config = resolver.parse_config("API_KEY", &spec).unwrap();
        assert_eq!(config.path, "/team/app/API_KEY");
    }

    #[test]
    fn split_path_extracts_folder_and_key() {
        let resolver = InfisicalResolver::new();
        let (folder, key) = resolver.split_path("API_KEY", "/team/app/API_KEY").unwrap();
        assert_eq!(folder, "/team/app");
        assert_eq!(key, "API_KEY");
    }

    #[test]
    fn split_path_rejects_missing_folder() {
        let resolver = InfisicalResolver::new();
        let err = resolver.split_path("API_KEY", "API_KEY").unwrap_err();
        assert!(matches!(err, SecretError::ResolutionFailed { .. }));
    }

    #[test]
    fn provider_name_is_infisical() {
        let resolver = InfisicalResolver::new();
        assert_eq!(resolver.provider_name(), "infisical");
    }
}
