//! GCP Secret Manager secret resolver with auto-negotiating dual-mode (HTTP + CLI)

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for GCP Secret Manager resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    #[cfg(feature = "gcp")]
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
    #[cfg(feature = "gcp")]
    pub fn new() -> Result<Self, SecretError> {
        let use_http = Self::http_credentials_available();
        Ok(Self { use_http })
    }

    /// Create a new GCP resolver (CLI mode only)
    ///
    /// # Errors
    ///
    /// This function is infallible when the `gcp` feature is disabled.
    #[cfg(not(feature = "gcp"))]
    pub fn new() -> Result<Self, SecretError> {
        Ok(Self {})
    }

    /// Check if HTTP credentials are available in environment
    #[cfg(feature = "gcp")]
    fn http_credentials_available() -> bool {
        std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    #[allow(clippy::unused_self)] // self is used when feature is enabled
    fn can_use_http(&self) -> bool {
        #[cfg(feature = "gcp")]
        {
            self.use_http
        }
        #[cfg(not(feature = "gcp"))]
        {
            false
        }
    }

    /// Resolve using the GCP Secret Manager API (HTTP mode)
    #[cfg(feature = "gcp")]
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
        #[cfg(feature = "gcp")]
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
    #[cfg(feature = "gcp")]
    fn test_http_credentials_check() {
        // This test just ensures the function exists and doesn't panic
        let _ = GcpResolver::http_credentials_available();
    }
}
