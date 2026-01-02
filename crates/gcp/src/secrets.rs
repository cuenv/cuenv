//! GCP Secret Manager secret resolver with auto-negotiating dual-mode (HTTP + CLI)

use async_trait::async_trait;
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for GCP Secret Manager resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GcpSecretConfig {
    /// GCP project ID
    pub project: String,

    /// Secret name
    pub secret: String,

    /// Version (defaults to "latest")
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "latest".to_string()
}

impl GcpSecretConfig {
    /// Create a new GCP secret config
    #[must_use]
    pub fn new(project: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            secret: secret.into(),
            version: "latest".to_string(),
        }
    }

    /// Get the full resource name for the secret version
    #[must_use]
    pub fn resource_name(&self) -> String {
        format!(
            "projects/{}/secrets/{}/versions/{}",
            self.project, self.secret, self.version
        )
    }
}

/// Resolves secrets from GCP Secret Manager
///
/// Mode is auto-negotiated based on environment:
/// - If `GOOGLE_APPLICATION_CREDENTIALS` is set → HTTP mode
/// - Otherwise → CLI mode (uses `gcloud` CLI)
///
/// The `source` field in [`SecretSpec`] can be:
/// - A JSON-encoded [`GcpSecretConfig`]
/// - A resource name like "projects/PROJECT/secrets/SECRET/versions/VERSION"
pub struct GcpResolver {
    use_http: bool,
}

impl std::fmt::Debug for GcpResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcpResolver")
            .field("mode", &if self.can_use_http() { "http" } else { "cli" })
            .finish()
    }
}

impl GcpResolver {
    /// Create a new GCP resolver with auto-detected mode
    ///
    /// If GCP credentials are available in environment, uses HTTP mode.
    /// Otherwise, CLI mode will be used.
    ///
    /// # Errors
    /// Returns error if GCP credentials cannot be loaded.
    pub fn new() -> Result<Self, SecretError> {
        let use_http = Self::http_credentials_available();
        Ok(Self { use_http })
    }

    /// Check if HTTP credentials are available in environment
    fn http_credentials_available() -> bool {
        std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    const fn can_use_http(&self) -> bool {
        self.use_http
    }

    /// Resolve using the GCP Secret Manager API (HTTP mode)
    async fn resolve_http(
        &self,
        name: &str,
        config: &GcpSecretConfig,
    ) -> Result<String, SecretError> {
        // TODO: Implement using google-secretmanager1 crate
        // For now, fall back to CLI
        tracing::warn!(
            "GCP HTTP mode not yet fully implemented, falling back to CLI for secret '{}'",
            name
        );
        self.resolve_cli(name, config).await
    }

    /// Resolve using the gcloud CLI
    async fn resolve_cli(
        &self,
        name: &str,
        config: &GcpSecretConfig,
    ) -> Result<String, SecretError> {
        let output = Command::new("gcloud")
            .args([
                "secrets",
                "versions",
                "access",
                &config.version,
                "--secret",
                &config.secret,
                "--project",
                &config.project,
            ])
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute gcloud CLI: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("gcloud CLI failed: {stderr}"),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Parse a resource name into a config
    fn parse_resource_name(resource_name: &str) -> Option<GcpSecretConfig> {
        // Format: projects/PROJECT/secrets/SECRET/versions/VERSION
        let parts: Vec<&str> = resource_name.split('/').collect();
        if parts.len() >= 6
            && parts[0] == "projects"
            && parts[2] == "secrets"
            && parts[4] == "versions"
        {
            Some(GcpSecretConfig {
                project: parts[1].to_string(),
                secret: parts[3].to_string(),
                version: parts[5].to_string(),
            })
        } else {
            None
        }
    }

    /// Resolve a secret - tries HTTP first if available, falls back to CLI
    async fn resolve_with_config(
        &self,
        name: &str,
        config: &GcpSecretConfig,
    ) -> Result<String, SecretError> {
        // Try HTTP mode if available
        if self.use_http {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }
}

#[async_trait]
impl SecretResolver for GcpResolver {
    fn provider_name(&self) -> &'static str {
        "gcp"
    }

    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON GcpSecretConfig
        if let Ok(config) = serde_json::from_str::<GcpSecretConfig>(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        // Try to parse as a resource name
        if let Some(config) = Self::parse_resource_name(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        Err(SecretError::ResolutionFailed {
            name: name.to_string(),
            message: format!(
                "Invalid GCP secret source. Expected JSON config or resource name, got: {}",
                spec.source
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcp_config_serialization() {
        let config = GcpSecretConfig {
            project: "my-project".to_string(),
            secret: "my-secret".to_string(),
            version: "latest".to_string(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: GcpSecretConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_resource_name() {
        let config = GcpSecretConfig::new("my-project", "my-secret");
        assert_eq!(
            config.resource_name(),
            "projects/my-project/secrets/my-secret/versions/latest"
        );
    }

    #[test]
    fn test_parse_resource_name() {
        let config =
            GcpResolver::parse_resource_name("projects/my-project/secrets/my-secret/versions/5")
                .unwrap();
        assert_eq!(config.project, "my-project");
        assert_eq!(config.secret, "my-secret");
        assert_eq!(config.version, "5");
    }

    #[test]
    fn test_parse_invalid_resource_name() {
        assert!(GcpResolver::parse_resource_name("invalid/path").is_none());
    }

    #[test]
    fn test_http_credentials_check() {
        // This test just ensures the function exists and doesn't panic
        let _ = GcpResolver::http_credentials_available();
    }

    #[test]
    fn test_gcp_config_new() {
        let config = GcpSecretConfig::new("my-project", "api-key");
        assert_eq!(config.project, "my-project");
        assert_eq!(config.secret, "api-key");
        assert_eq!(config.version, "latest");
    }

    #[test]
    fn test_gcp_config_new_with_string_types() {
        let config = GcpSecretConfig::new(String::from("project-id"), String::from("secret-name"));
        assert_eq!(config.project, "project-id");
        assert_eq!(config.secret, "secret-name");
    }

    #[test]
    fn test_default_version_function() {
        assert_eq!(default_version(), "latest");
    }

    #[test]
    fn test_resource_name_with_specific_version() {
        let config = GcpSecretConfig {
            project: "prod-project".to_string(),
            secret: "database-password".to_string(),
            version: "42".to_string(),
        };
        assert_eq!(
            config.resource_name(),
            "projects/prod-project/secrets/database-password/versions/42"
        );
    }

    #[test]
    fn test_gcp_config_clone() {
        let config = GcpSecretConfig {
            project: "my-project".to_string(),
            secret: "my-secret".to_string(),
            version: "latest".to_string(),
        };
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_gcp_config_debug() {
        let config = GcpSecretConfig::new("test-project", "test-secret");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("GcpSecretConfig"));
        assert!(debug_str.contains("test-project"));
        assert!(debug_str.contains("test-secret"));
    }

    #[test]
    fn test_parse_resource_name_with_latest() {
        let config = GcpResolver::parse_resource_name(
            "projects/my-project/secrets/my-secret/versions/latest",
        )
        .unwrap();
        assert_eq!(config.project, "my-project");
        assert_eq!(config.secret, "my-secret");
        assert_eq!(config.version, "latest");
    }

    #[test]
    fn test_parse_resource_name_missing_projects() {
        assert!(
            GcpResolver::parse_resource_name("my-project/secrets/my-secret/versions/1").is_none()
        );
    }

    #[test]
    fn test_parse_resource_name_missing_secrets() {
        assert!(
            GcpResolver::parse_resource_name("projects/my-project/my-secret/versions/1").is_none()
        );
    }

    #[test]
    fn test_parse_resource_name_missing_versions() {
        assert!(
            GcpResolver::parse_resource_name("projects/my-project/secrets/my-secret/1").is_none()
        );
    }

    #[test]
    fn test_parse_resource_name_too_short() {
        assert!(GcpResolver::parse_resource_name("projects/my-project/secrets").is_none());
    }

    #[test]
    fn test_gcp_config_deserialization_with_defaults() {
        // When version is missing, it should default to "latest"
        let json = r#"{"project": "my-project", "secret": "my-secret"}"#;
        let config: GcpSecretConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.project, "my-project");
        assert_eq!(config.secret, "my-secret");
        assert_eq!(config.version, "latest");
    }

    #[test]
    fn test_gcp_config_full_serialization() {
        let config = GcpSecretConfig {
            project: "prod".to_string(),
            secret: "api-key".to_string(),
            version: "5".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        // Verify camelCase serialization
        assert!(json.contains("\"project\":\"prod\""));
        assert!(json.contains("\"secret\":\"api-key\""));
        assert!(json.contains("\"version\":\"5\""));
    }

    #[test]
    fn test_gcp_config_equality() {
        let config1 = GcpSecretConfig::new("project", "secret");
        let config2 = GcpSecretConfig::new("project", "secret");
        let config3 = GcpSecretConfig::new("project", "other-secret");

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }

    #[test]
    fn test_gcp_resolver_new() {
        let resolver = GcpResolver::new();
        assert!(resolver.is_ok());
    }

    #[test]
    fn test_gcp_resolver_debug() {
        let resolver = GcpResolver::new().unwrap();
        let debug_str = format!("{resolver:?}");
        assert!(debug_str.contains("GcpResolver"));
        // Should show either "http" or "cli" mode
        assert!(debug_str.contains("mode"));
    }

    #[test]
    fn test_gcp_resolver_provider_name() {
        let resolver = GcpResolver::new().unwrap();
        assert_eq!(resolver.provider_name(), "gcp");
    }
}
