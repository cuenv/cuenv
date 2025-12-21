//! 1Password secret resolver with auto-negotiating dual-mode (HTTP via WASM SDK + CLI)

use crate::{SecretError, SecretResolver, SecretSpec, SecureSecret};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::process::Command;

#[cfg(feature = "onepassword")]
use super::onepassword_core::SharedCore;

/// Configuration for 1Password secret resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OnePasswordConfig {
    /// Secret reference (e.g., `op://vault/item/field`)
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
/// - A simple reference string (e.g., `op://vault/item/field`)
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
    ///
    /// # Errors
    ///
    /// Returns an error if the 1Password WASM client cannot be initialized.
    pub fn new() -> Result<Self, SecretError> {
        #[cfg(feature = "onepassword")]
        let client_id = if Self::http_mode_available() {
            match Self::init_wasm_client() {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(
                        "Failed to initialize 1Password WASM client, falling back to CLI: {e}"
                    );
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
        std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok()
            && crate::wasm::onepassword_wasm_available()
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
        let mut guard = core_mutex
            .lock()
            .map_err(|_| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: "Failed to acquire shared core lock".to_string(),
            })?;

        let core = guard
            .as_mut()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: "SharedCore not initialized".to_string(),
            })?;

        core.init_client(&token)
    }

    /// Check if this resolver can use HTTP mode
    #[allow(clippy::unused_self)] // self is used when feature is enabled
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
    fn resolve_http(&self, name: &str, config: &OnePasswordConfig) -> Result<String, SecretError> {
        let client_id = self
            .client_id
            .as_ref()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "HTTP client not initialized".to_string(),
            })?;

        let core_mutex = SharedCore::get_or_init()?;
        let mut guard = core_mutex
            .lock()
            .map_err(|_| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "Failed to acquire shared core lock".to_string(),
            })?;

        let core = guard
            .as_mut()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: "SharedCore not initialized".to_string(),
            })?;

        // Invoke the Secrets.Resolve method
        let params = serde_json::json!({
            "secret_reference": config.reference
        });

        let result = core.invoke(client_id, "Secrets.Resolve", &params.to_string())?;

        // Parse the response to extract the secret value
        let response: serde_json::Value =
            serde_json::from_str(&result).map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to parse resolve response: {e}"),
            })?;

        // The response format from 1Password SDK
        response["result"]
            .as_str()
            .map(ToString::to_string)
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
            return self.resolve_http(name, config);
        }

        // Fallback to CLI
        self.resolve_cli(name, config).await
    }

    /// Resolve multiple secrets using Secrets.ResolveAll (HTTP mode)
    #[cfg(feature = "onepassword")]
    fn resolve_batch_http(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        let client_id = self
            .client_id
            .as_ref()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: "HTTP client not initialized".to_string(),
            })?;

        let core_mutex = SharedCore::get_or_init()?;
        let mut guard = core_mutex
            .lock()
            .map_err(|_| SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: "Failed to acquire shared core lock".to_string(),
            })?;

        let core = guard
            .as_mut()
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: "SharedCore not initialized".to_string(),
            })?;

        // Build list of references and track mapping back to names
        let mut ref_to_names: HashMap<String, Vec<String>> = HashMap::new();
        let mut references: Vec<String> = Vec::new();

        for (name, spec) in secrets {
            let config = serde_json::from_str::<OnePasswordConfig>(&spec.source)
                .unwrap_or_else(|_| OnePasswordConfig::new(spec.source.clone()));

            ref_to_names
                .entry(config.reference.clone())
                .or_default()
                .push(name.clone());

            if !references.contains(&config.reference) {
                references.push(config.reference);
            }
        }

        // Invoke Secrets.ResolveAll with array of references
        let params = serde_json::json!({
            "secret_references": references
        });

        let result = core.invoke(client_id, "Secrets.ResolveAll", &params.to_string())?;

        // Parse the response
        let response: serde_json::Value =
            serde_json::from_str(&result).map_err(|e| SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: format!("Failed to parse ResolveAll response: {e}"),
            })?;

        // Extract individual responses
        let individual_responses = response["individualResponses"].as_array().ok_or_else(|| {
            SecretError::ResolutionFailed {
                name: "batch".to_string(),
                message: "No individualResponses in response".to_string(),
            }
        })?;

        // Map responses back to original names
        let mut resolved: HashMap<String, SecureSecret> = HashMap::new();

        for (i, resp) in individual_responses.iter().enumerate() {
            let reference = references
                .get(i)
                .ok_or_else(|| SecretError::ResolutionFailed {
                    name: "batch".to_string(),
                    message: "Response index out of bounds".to_string(),
                })?;

            // Check for errors
            if let Some(error) = resp.get("error")
                && !error.is_null()
            {
                let error_type = error["type"].as_str().unwrap_or("Unknown");
                let error_msg = error["message"].as_str().unwrap_or("Unknown error");
                tracing::warn!(
                    reference = %reference,
                    error_type = %error_type,
                    message = %error_msg,
                    "Failed to resolve secret in batch"
                );
                continue;
            }

            // Extract secret value
            let secret = resp["content"]["secret"]
                .as_str()
                .or_else(|| resp["result"].as_str())
                .ok_or_else(|| SecretError::ResolutionFailed {
                    name: reference.clone(),
                    message: "No secret value in response".to_string(),
                })?;

            // Map to all names that use this reference
            if let Some(names) = ref_to_names.get(reference) {
                for name in names {
                    resolved.insert(name.clone(), SecureSecret::new(secret.to_string()));
                }
            }
        }

        Ok(resolved)
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

#[cfg(feature = "onepassword")]
impl Drop for OnePasswordResolver {
    fn drop(&mut self) {
        if let Some(client_id) = &self.client_id
            && let Ok(core_mutex) = SharedCore::get_or_init()
            && let Ok(mut guard) = core_mutex.lock()
            && let Some(core) = guard.as_mut()
        {
            core.release_client(client_id);
        }
    }
}

#[async_trait]
impl SecretResolver for OnePasswordResolver {
    fn provider_name(&self) -> &'static str {
        "onepassword"
    }

    fn supports_native_batch(&self) -> bool {
        // 1Password SDK supports Secrets.ResolveAll
        true
    }

    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON OnePasswordConfig
        if let Ok(config) = serde_json::from_str::<OnePasswordConfig>(&spec.source) {
            return self.resolve_with_config(name, &config).await;
        }

        // Fallback: treat source as a simple reference string
        let config = OnePasswordConfig::new(spec.source.clone());
        self.resolve_with_config(name, &config).await
    }

    async fn resolve_batch(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        if secrets.is_empty() {
            return Ok(HashMap::new());
        }

        // Use Secrets.ResolveAll if HTTP mode is available
        #[cfg(feature = "onepassword")]
        if self.client_id.is_some() {
            return self.resolve_batch_http(secrets);
        }

        // Fallback to concurrent CLI calls
        self.resolve_batch_cli(secrets).await
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
