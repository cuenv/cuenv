//! 1Password secret resolver with auto-negotiating dual-mode (HTTP via WASM SDK + CLI)

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[cfg(feature = "onepassword")]
use super::onepassword_core::SharedCore;

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
/// - If `OP_SERVICE_ACCOUNT_TOKEN` is set AND WASM SDK is installed → HTTP mode
/// - Otherwise → CLI mode (uses `op` CLI)
///
/// To enable HTTP mode, run: `cuenv secrets setup onepassword`
///
/// The `source` field in [`SecretSpec`] can be:
/// - A JSON-encoded [`OnePasswordConfig`]
/// - A simple reference string (e.g., "op://vault/item/field")
pub struct OnePasswordResolver {
    /// Client ID for WASM SDK (when using HTTP mode)
    #[cfg(feature = "onepassword")]
    client_id: Option<String>,
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
    /// If 1Password service account token is available AND the WASM SDK is installed,
    /// uses HTTP mode. Otherwise, CLI mode will be used.
    pub fn new() -> Result<Self, SecretError> {
        #[cfg(feature = "onepassword")]
        let client_id = if Self::http_mode_available() {
            match Self::init_wasm_client() {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!("Failed to initialize 1Password WASM client, falling back to CLI: {e}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            #[cfg(feature = "onepassword")]
            client_id,
        })
    }

    /// Check if HTTP mode is available (token set + WASM installed)
    #[cfg(feature = "onepassword")]
    fn http_mode_available() -> bool {
        std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok() && crate::wasm::onepassword_wasm_available()
    }

    /// Check if HTTP credentials are available in environment
    #[allow(dead_code)]
    fn http_credentials_available() -> bool {
        std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok()
    }

    /// Initialize the WASM client and return the client ID
    #[cfg(feature = "onepassword")]
    fn init_wasm_client() -> Result<String, SecretError> {
        let token = std::env::var("OP_SERVICE_ACCOUNT_TOKEN").map_err(|_| {
            SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: "OP_SERVICE_ACCOUNT_TOKEN not set".to_string(),
            }
        })?;

        let core_mutex = SharedCore::get_or_init()?;
        let mut guard = core_mutex.lock().map_err(|_| SecretError::ResolutionFailed {
            name: "onepassword".to_string(),
            message: "Failed to acquire shared core lock".to_string(),
        })?;

        let core = guard.as_mut().ok_or_else(|| SecretError::ResolutionFailed {
            name: "onepassword".to_string(),
            message: "SharedCore not initialized".to_string(),
        })?;

        core.init_client(&token)
    }

    /// Check if this resolver can use HTTP mode
    fn can_use_http(&self) -> bool {
        #[cfg(feature = "onepassword")]
        {
            self.client_id.is_some()
        }
        #[cfg(not(feature = "onepassword"))]
        {
            false
        }
    }

    /// Resolve using the 1Password WASM SDK (HTTP mode)
    #[cfg(feature = "onepassword")]
    async fn resolve_http(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        let client_id = self.client_id.as_ref().ok_or_else(|| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: "HTTP client not initialized".to_string(),
        })?;

        let core_mutex = SharedCore::get_or_init()?;
        let mut guard = core_mutex.lock().map_err(|_| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: "Failed to acquire shared core lock".to_string(),
        })?;

        let core = guard.as_mut().ok_or_else(|| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: "SharedCore not initialized".to_string(),
        })?;

        // Invoke the Secrets.Resolve method
        let params = serde_json::json!({
            "secret_reference": config.reference
        });

        let result = core.invoke(client_id, "Secrets.Resolve", &params.to_string())?;

        // Parse the response to extract the secret value
        let response: serde_json::Value = serde_json::from_str(&result).map_err(|e| {
            SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to parse resolve response: {e}"),
            }
        })?;

        // The response format from 1Password SDK
        response["result"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "No result in response".to_string(),
            })
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
        if self.client_id.is_some() {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }
}

#[cfg(feature = "onepassword")]
impl Drop for OnePasswordResolver {
    fn drop(&mut self) {
        if let Some(client_id) = &self.client_id {
            if let Ok(core_mutex) = SharedCore::get_or_init() {
                if let Ok(mut guard) = core_mutex.lock() {
                    if let Some(core) = guard.as_mut() {
                        core.release_client(client_id);
                    }
                }
            }
        }
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
        let config = OnePasswordConfig::new(spec.source.clone());
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
