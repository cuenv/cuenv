//! 1Password WASM SDK SharedCore wrapper
//!
//! This module provides a thread-safe wrapper around the 1Password WASM SDK,
//! following the same pattern as the official Go SDK.

#[cfg(feature = "onepassword")]
use crate::SecretError;

#[cfg(feature = "onepassword")]
use extism::{Manifest, Plugin, Wasm};

#[cfg(feature = "onepassword")]
use once_cell::sync::Lazy;

#[cfg(feature = "onepassword")]
use std::sync::Mutex;

/// Global SharedCore instance, lazily initialized
#[cfg(feature = "onepassword")]
static SHARED_CORE: Lazy<Mutex<Option<SharedCore>>> = Lazy::new(|| Mutex::new(None));

/// SharedCore wraps the 1Password WASM plugin for thread-safe access.
///
/// The WASM runtime is single-threaded, so we use a mutex to serialize access.
/// This follows the same pattern as the official 1Password Go SDK.
#[cfg(feature = "onepassword")]
pub struct SharedCore {
    plugin: Plugin,
}

#[cfg(feature = "onepassword")]
impl SharedCore {
    /// Get or initialize the shared core.
    ///
    /// On first call, loads the WASM from disk and initializes the plugin.
    /// Subsequent calls return the cached instance.
    pub fn get_or_init() -> Result<&'static Mutex<Option<SharedCore>>, SecretError> {
        let mut guard = SHARED_CORE
            .lock()
            .map_err(|_| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: "Failed to acquire shared core lock".to_string(),
            })?;

        if guard.is_none() {
            let wasm_bytes = crate::wasm::load_onepassword_wasm()?;

            let manifest = Manifest::new([Wasm::data(wasm_bytes)]).with_allowed_hosts(
                ["*.1password.com".to_string(), "*.b5dev.com".to_string()].into_iter(),
            );

            let plugin =
                Plugin::new(&manifest, [], true).map_err(|e| SecretError::ResolutionFailed {
                    name: "onepassword".to_string(),
                    message: format!("Failed to initialize WASM plugin: {e}"),
                })?;

            *guard = Some(SharedCore { plugin });
        }

        // Drop guard before returning static reference
        drop(guard);
        Ok(&SHARED_CORE)
    }

    /// Initialize a new 1Password client.
    ///
    /// Returns a client ID that can be used for subsequent `invoke` calls.
    pub fn init_client(&mut self, token: &str) -> Result<String, SecretError> {
        let config = serde_json::json!({
            "serviceAccountToken": token,
            "programmingLanguage": "Rust",
            "sdkVersion": env!("CARGO_PKG_VERSION"),
            "integrationName": "cuenv",
            "integrationVersion": env!("CARGO_PKG_VERSION"),
        });

        let result = self
            .plugin
            .call::<_, String>("init_client", config.to_string())
            .map_err(|e| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("Failed to initialize client: {e}"),
            })?;

        // Parse the response to check for errors
        let response: serde_json::Value =
            serde_json::from_str(&result).map_err(|e| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("Failed to parse init_client response: {e}"),
            })?;

        // Check for error in response
        if let Some(error) = response.get("error") {
            return Err(SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("1Password client init failed: {error}"),
            });
        }

        // Extract client ID
        response["clientId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: "No clientId in response".to_string(),
            })
    }

    /// Invoke a method on the 1Password client.
    ///
    /// The method name and parameters depend on the specific operation.
    /// For resolving secrets, use method "Secrets.Resolve" with the secret reference.
    pub fn invoke(
        &mut self,
        client_id: &str,
        method: &str,
        params: &str,
    ) -> Result<String, SecretError> {
        let request = serde_json::json!({
            "clientId": client_id,
            "invocation": {
                "methodName": method,
                "parameters": params
            }
        });

        let result = self
            .plugin
            .call::<_, String>("invoke", request.to_string())
            .map_err(|e| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("Invoke failed: {e}"),
            })?;

        // Parse response to check for errors
        let response: serde_json::Value =
            serde_json::from_str(&result).map_err(|e| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("Failed to parse invoke response: {e}"),
            })?;

        if let Some(error) = response.get("error") {
            return Err(SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!("1Password invoke failed: {error}"),
            });
        }

        Ok(result)
    }

    /// Release a 1Password client.
    ///
    /// This should be called when the client is no longer needed.
    pub fn release_client(&mut self, client_id: &str) {
        let _ = self.plugin.call::<_, String>("release_client", client_id);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "onepassword")]
    use super::*;

    #[test]
    #[cfg(feature = "onepassword")]
    fn test_shared_core_lazy_init() {
        // This test just verifies the lazy static compiles
        // Actual WASM loading requires the file to exist
        let _ = &SHARED_CORE;
    }
}
