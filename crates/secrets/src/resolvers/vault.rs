//! HashiCorp Vault secret resolver with auto-negotiating dual-mode (HTTP + CLI)

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Configuration for HashiCorp Vault resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VaultSecretConfig {
    /// Path to the secret (e.g., "secret/data/myapp/config")
    pub path: String,

    /// Key within the secret to extract
    pub key: String,

    /// Secret engine mount point (defaults to "secret")
    #[serde(default = "default_mount")]
    pub mount: String,
}

fn default_mount() -> String {
    "secret".to_string()
}

impl VaultSecretConfig {
    /// Create a new Vault secret config
    #[must_use]
    pub fn new(path: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            key: key.into(),
            mount: "secret".to_string(),
        }
    }

    /// Get the full path including mount point
    #[must_use]
    pub fn full_path(&self) -> String {
        // KV v2 uses /data/ in the path
        if self.path.starts_with(&self.mount) {
            self.path.clone()
        } else {
            format!("{}/data/{}", self.mount, self.path)
        }
    }
}

/// Resolves secrets from HashiCorp Vault
///
/// Mode is auto-negotiated based on environment:
/// - If `VAULT_TOKEN` and `VAULT_ADDR` are set → HTTP mode
/// - Otherwise → CLI mode (uses `vault` CLI)
///
/// The `source` field in [`SecretSpec`] can be:
/// - A JSON-encoded [`VaultSecretConfig`]
pub struct VaultResolver {
    #[cfg(feature = "vault")]
    client: Option<vaultrs::client::VaultClient>,
}

impl std::fmt::Debug for VaultResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultResolver")
            .field("mode", &if self.can_use_http() { "http" } else { "cli" })
            .finish()
    }
}

impl VaultResolver {
    /// Create a new Vault resolver with auto-detected mode
    ///
    /// If Vault credentials are available in environment, uses HTTP mode.
    /// Otherwise, CLI mode will be used.
    pub fn new() -> Result<Self, SecretError> {
        #[cfg(feature = "vault")]
        let client = if Self::http_credentials_available() {
            let addr =
                std::env::var("VAULT_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".to_string());
            let token = std::env::var("VAULT_TOKEN").map_err(|_| SecretError::ResolutionFailed {
                name: "vault".to_string(),
                message: "VAULT_TOKEN environment variable not set".to_string(),
            })?;

            Some(
                vaultrs::client::VaultClient::new(
                    vaultrs::client::VaultClientSettingsBuilder::default()
                        .address(addr)
                        .token(token)
                        .build()
                        .map_err(|e| SecretError::ResolutionFailed {
                            name: "vault".to_string(),
                            message: format!("Failed to build Vault client: {e}"),
                        })?,
                )
                .map_err(|e| SecretError::ResolutionFailed {
                    name: "vault".to_string(),
                    message: format!("Failed to create Vault client: {e}"),
                })?,
            )
        } else {
            None
        };

        Ok(Self {
            #[cfg(feature = "vault")]
            client,
        })
    }

    /// Check if HTTP credentials are available in environment
    fn http_credentials_available() -> bool {
        std::env::var("VAULT_TOKEN").is_ok() && std::env::var("VAULT_ADDR").is_ok()
    }

    /// Check if this resolver can use HTTP mode
    fn can_use_http(&self) -> bool {
        #[cfg(feature = "vault")]
        {
            self.client.is_some()
        }
        #[cfg(not(feature = "vault"))]
        {
            false
        }
    }

    /// Resolve using the Vault HTTP API
    #[cfg(feature = "vault")]
    async fn resolve_http(
        &self,
        name: &str,
        config: &VaultSecretConfig,
    ) -> Result<String, SecretError> {
        let client = self.client.as_ref().ok_or_else(|| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: "Vault HTTP client not initialized".to_string(),
        })?;

        // Read secret from KV v2
        let secret: std::collections::HashMap<String, String> = vaultrs::kv2::read(
            client,
            &config.mount,
            &config.path,
        )
        .await
        .map_err(|e| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: format!("Vault read error: {e}"),
        })?;

        secret.get(&config.key).cloned().ok_or_else(|| SecretError::ResolutionFailed {
            name: name.to_string(),
            message: format!("Key '{}' not found in secret", config.key),
        })
    }

    /// Resolve using the vault CLI
    async fn resolve_cli(
        &self,
        name: &str,
        config: &VaultSecretConfig,
    ) -> Result<String, SecretError> {
        let output = Command::new("vault")
            .args([
                "kv",
                "get",
                "-mount",
                &config.mount,
                "-field",
                &config.key,
                &config.path,
            ])
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute vault CLI: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("vault CLI failed: {stderr}"),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Resolve a secret - tries HTTP first if available, falls back to CLI
    async fn resolve_with_config(
        &self,
        name: &str,
        config: &VaultSecretConfig,
    ) -> Result<String, SecretError> {
        // Try HTTP mode if available
        #[cfg(feature = "vault")]
        if self.client.is_some() {
            return self.resolve_http(name, config).await;
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }
}

#[async_trait]
impl SecretResolver for VaultResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Parse source as JSON VaultSecretConfig
        let config: VaultSecretConfig =
            serde_json::from_str(&spec.source).map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Invalid Vault secret config: {e}"),
            })?;

        self.resolve_with_config(name, &config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_config_serialization() {
        let config = VaultSecretConfig {
            path: "myapp/config".to_string(),
            key: "password".to_string(),
            mount: "secret".to_string(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: VaultSecretConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_full_path() {
        let config = VaultSecretConfig::new("myapp/config", "password");
        assert_eq!(config.full_path(), "secret/data/myapp/config");
    }

    #[test]
    fn test_full_path_with_mount() {
        let config = VaultSecretConfig {
            path: "myapp/config".to_string(),
            key: "password".to_string(),
            mount: "kv".to_string(),
        };
        assert_eq!(config.full_path(), "kv/data/myapp/config");
    }

    #[test]
    fn test_http_credentials_check() {
        // This test just ensures the function exists and doesn't panic
        let _ = VaultResolver::http_credentials_available();
    }
}
