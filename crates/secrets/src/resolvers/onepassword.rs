//! 1Password secret resolver with auto-negotiating dual-mode (HTTP via WASM SDK + CLI)

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for 1Password secret resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OnePasswordConfig {
    /// Secret reference (e.g., "op://vault/item/field")
    #[serde(rename = "ref")]
    pub reference: String,
}

impl OnePasswordConfig {
    /// Create a new 1Password secret config
    #[must_use]
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
        }
    }
}

/// Resolves secrets from 1Password
///
/// Mode is auto-negotiated based on environment:
/// - If `OP_SERVICE_ACCOUNT_TOKEN` is set → HTTP mode (via WASM SDK)
/// - Otherwise → CLI mode (uses `op` CLI)
///
/// The `source` field in [`SecretSpec`] can be:
/// - A JSON-encoded [`OnePasswordConfig`]
/// - A simple reference string (e.g., "op://vault/item/field")
pub struct OnePasswordResolver {
    #[cfg(feature = "onepassword")]
    use_http: bool,
}

impl std::fmt::Debug for OnePasswordResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnePasswordResolver")
            .field("mode", &if self.can_use_http() { "http" } else { "cli" })
            .finish()
    }
}

impl OnePasswordResolver {
    /// Create a new 1Password resolver with auto-detected mode
    ///
    /// If 1Password service account token is available, uses HTTP mode.
    /// Otherwise, CLI mode will be used.
    pub fn new() -> Result<Self, SecretError> {
        #[cfg(feature = "onepassword")]
        let use_http = Self::http_credentials_available();

        Ok(Self {
            #[cfg(feature = "onepassword")]
            use_http,
        })
    }

    /// Check if HTTP credentials are available in environment
    fn http_credentials_available() -> bool {
        std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    fn can_use_http(&self) -> bool {
        #[cfg(feature = "onepassword")]
        {
            self.use_http
        }
        #[cfg(not(feature = "onepassword"))]
        {
            false
        }
    }

    /// Resolve using the 1Password WASM SDK (HTTP mode)
    ///
    /// This uses the 1Password WASM core directly via extism.
    #[cfg(feature = "onepassword")]
    async fn resolve_http(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        // TODO: Implement using extism to load 1Password WASM core
        // For now, fall back to CLI mode until the WASM integration is implemented.
        tracing::warn!(
            "1Password HTTP mode not yet implemented, falling back to CLI for secret '{}'",
            name
        );
        self.resolve_cli(name, config).await
    }

    /// Resolve using the op CLI
    async fn resolve_cli(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        let output = Command::new("op")
            .args(["read", &config.reference])
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute op CLI: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("op CLI failed: {stderr}"),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Resolve a secret - tries HTTP first if available, falls back to CLI
    async fn resolve_with_config(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        // Try HTTP mode if available
        #[cfg(feature = "onepassword")]
        if self.use_http {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }
}

#[async_trait]
impl SecretResolver for OnePasswordResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON OnePasswordConfig
        if let Ok(config) = serde_json::from_str::<OnePasswordConfig>(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        // Fallback: treat source as a simple reference string
        // Accept both "op://..." format and plain reference
        let reference = if spec.source.starts_with("op://") {
            spec.source.clone()
        } else {
            spec.source.clone()
        };

        let config = OnePasswordConfig::new(reference);
        self.resolve_with_config(name, &config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_onepassword_config_serialization() {
        let config = OnePasswordConfig {
            reference: "op://vault/item/password".to_string(),
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"ref\""));

        let parsed: OnePasswordConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_simple_config() {
        let config = OnePasswordConfig::new("op://Personal/GitHub/token");
        assert_eq!(config.reference, "op://Personal/GitHub/token");
    }

    #[test]
    fn test_http_credentials_check() {
        // This test just ensures the function exists and doesn't panic
        let _ = OnePasswordResolver::http_credentials_available();
    }
}
