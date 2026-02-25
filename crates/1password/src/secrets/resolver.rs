//! 1Password secret resolver with auto-negotiating dual-mode (HTTP via WASM SDK + CLI)

// Complex WASM+CLI dual-mode resolver with mutex-based shared core management
#![allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::significant_drop_tightening
)]

use super::core::SharedCore;
use super::wasm;
use async_trait::async_trait;
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec, SecureSecret};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::{process::Command, sync::Mutex};

/// Configuration for 1Password secret resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    client_id: Option<u64>,
    /// One-time CLI auth preflight state for `op whoami` in CLI mode.
    cli_auth_state: Mutex<CliAuthState>,
}

#[derive(Debug, Clone)]
enum CliAuthState {
    Unknown,
    Authenticated,
    Failed(String),
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
    /// Returns an error if HTTP mode is detected (WASM + token) but WASM initialization fails.
    /// This prevents silent fallback to CLI mode which masks configuration errors.
    pub fn new() -> Result<Self, SecretError> {
        let client_id = if Self::http_mode_available() {
            // HTTP mode is available (WASM + token), WASM MUST initialize successfully
            // Do NOT silently fall back to CLI - that masks the real error
            let id = Self::init_wasm_client().map_err(|e| SecretError::ResolutionFailed {
                name: "onepassword".to_string(),
                message: format!(
                    "1Password HTTP mode detected (WASM + token) but initialization failed: {e}\n\
                    \n\
                    This indicates a platform/runtime compatibility issue.\n\
                    To use CLI mode instead, unset OP_SERVICE_ACCOUNT_TOKEN or remove the WASM file."
                ),
            })?;
            tracing::debug!("1Password WASM client initialized successfully");
            Some(id)
        } else {
            tracing::debug!("1Password HTTP mode not available, using CLI");
            None
        };

        Ok(Self {
            client_id,
            cli_auth_state: Mutex::new(CliAuthState::Unknown),
        })
    }

    /// Check if HTTP mode is available (token set + WASM installed)
    fn http_mode_available() -> bool {
        let token_set = std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_ok();
        let wasm_available = wasm::onepassword_wasm_available();
        tracing::trace!(
            token_set,
            wasm_available,
            "1Password HTTP mode availability check"
        );
        token_set && wasm_available
    }

    /// Initialize the WASM client and return the client ID
    fn init_wasm_client() -> Result<u64, SecretError> {
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
    const fn can_use_http(&self) -> bool {
        self.client_id.is_some()
    }

    /// Resolve using the 1Password WASM SDK (HTTP mode)
    fn resolve_http(&self, name: &str, config: &OnePasswordConfig) -> Result<String, SecretError> {
        let client_id = self
            .client_id
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

        // Invoke the SecretsResolve method (Go SDK uses this name, not "Secrets.Resolve")
        let mut params = serde_json::Map::new();
        params.insert(
            "secret_reference".to_string(),
            serde_json::Value::String(config.reference.clone()),
        );

        let result = core.invoke(client_id, "SecretsResolve", &params, &config.reference)?;

        // Parse the response - the Go SDK returns a JSON-encoded string
        // The invoke response is the raw string from WASM, which is a JSON-quoted secret value
        let secret: String =
            serde_json::from_str(&result).map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to parse resolve response: {e}"),
            })?;

        Ok(secret)
    }

    /// Resolve using the op CLI
    async fn resolve_cli(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        tracing::debug!(
            name = name,
            reference = config.reference,
            "1Password resolve_cli"
        );
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

    /// Ensure CLI auth is valid once per resolver instance.
    async fn ensure_cli_authenticated(&self, name: &str) -> Result<(), SecretError> {
        let mut state = self.cli_auth_state.lock().await;
        match &*state {
            CliAuthState::Authenticated => return Ok(()),
            CliAuthState::Failed(message) => {
                return Err(SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message: message.clone(),
                });
            }
            CliAuthState::Unknown => {}
        }

        let preflight_result = Command::new("op").arg("whoami").output().await;

        match preflight_result {
            Ok(output) if output.status.success() => {
                *state = CliAuthState::Authenticated;
                Ok(())
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let details = if stderr.is_empty() {
                    "no error output from 1Password CLI".to_string()
                } else {
                    stderr
                };
                let message = format!(
                    "1Password CLI authentication check failed (`op whoami`). \
                    Run `op signin` and retry. Details: {details}"
                );
                *state = CliAuthState::Failed(message.clone());
                Err(SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let message = "1Password CLI not found (`op` command unavailable). \
                    Install the 1Password CLI and retry."
                    .to_string();
                *state = CliAuthState::Failed(message.clone());
                Err(SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message,
                })
            }
            Err(e) => {
                let message = format!(
                    "Failed to execute 1Password CLI authentication check (`op whoami`): {e}. \
                    Run `op signin` and retry."
                );
                *state = CliAuthState::Failed(message.clone());
                Err(SecretError::ResolutionFailed {
                    name: name.to_string(),
                    message,
                })
            }
        }
    }

    /// Resolve a secret - tries HTTP first if available, falls back to CLI
    async fn resolve_with_config(
        &self,
        name: &str,
        config: &OnePasswordConfig,
    ) -> Result<String, SecretError> {
        // Try HTTP mode if available
        if self.client_id.is_some() {
            return self.resolve_http(name, config);
        }

        // Fallback to CLI
        self.ensure_cli_authenticated(name).await?;
        self.resolve_cli(name, config).await
    }

    /// Resolve multiple secrets using Secrets.ResolveAll (HTTP mode)
    fn resolve_batch_http(
        &self,
        secrets: &HashMap<String, SecretSpec>,
    ) -> Result<HashMap<String, SecureSecret>, SecretError> {
        let client_id = self
            .client_id
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

        // Invoke SecretsResolveAll with array of references
        let mut params = serde_json::Map::new();
        params.insert(
            "secret_references".to_string(),
            serde_json::Value::Array(
                references
                    .iter()
                    .map(|r| serde_json::Value::String(r.clone()))
                    .collect(),
            ),
        );

        // Use first reference as context for top-level errors
        let context = references.first().map_or("batch", String::as_str);
        let result = core.invoke(client_id, "SecretsResolveAll", &params, context)?;

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

            // Check for errors - fail immediately with the specific secret reference
            if let Some(error) = resp.get("error")
                && !error.is_null()
            {
                let error_type = error["type"].as_str().unwrap_or("Unknown");
                let error_msg = error["message"].as_str().unwrap_or("Unknown error");
                return Err(SecretError::ResolutionFailed {
                    name: reference.clone(),
                    message: format!("1Password error ({error_type}): {error_msg}"),
                });
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

impl Drop for OnePasswordResolver {
    fn drop(&mut self) {
        if let Some(client_id) = self.client_id
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::{fs, path::Path};

    #[cfg(unix)]
    fn write_fake_op_shim(dir: &Path) -> std::path::PathBuf {
        let op_path = dir.join("op");
        let script = r#"#!/bin/sh
cmd="$1"
shift

case "$cmd" in
  whoami)
    printf "whoami\n" >> "$OP_TEST_LOG"
    if [ "${OP_TEST_FAIL_WHOAMI:-0}" = "1" ]; then
      printf "not signed in\n" >&2
      exit 1
    fi
    printf "test-user@example.com\n"
    exit 0
    ;;
  read)
    printf "read:%s\n" "$1" >> "$OP_TEST_LOG"
    printf "secret-for-%s\n" "$1"
    exit 0
    ;;
  *)
    printf "unsupported op command: %s\n" "$cmd" >&2
    exit 2
    ;;
esac
"#;

        fs::write(&op_path, script).unwrap();
        let mut perms = fs::metadata(&op_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&op_path, perms).unwrap();
        op_path
    }

    #[cfg(unix)]
    fn prepend_path(dir: &Path) -> String {
        let mut parts = vec![dir.to_path_buf()];
        if let Some(current) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&current));
        }
        std::env::join_paths(parts)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    fn read_log_lines(path: &Path) -> Vec<String> {
        let content = fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

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
    fn test_config_new_from_string() {
        let config = OnePasswordConfig::new(String::from("op://vault/item/field"));
        assert_eq!(config.reference, "op://vault/item/field");
    }

    #[test]
    fn test_config_new_from_str_slice() {
        let ref_str = "op://vault/item/field";
        let config = OnePasswordConfig::new(ref_str);
        assert_eq!(config.reference, ref_str);
    }

    #[test]
    fn test_config_equality() {
        let config1 = OnePasswordConfig::new("op://vault/item/field");
        let config2 = OnePasswordConfig::new("op://vault/item/field");
        let config3 = OnePasswordConfig::new("op://other/item/field");

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }

    #[test]
    fn test_config_clone() {
        let config = OnePasswordConfig::new("op://vault/item/field");
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_config_debug() {
        let config = OnePasswordConfig::new("op://vault/item/field");
        let debug = format!("{config:?}");
        assert!(debug.contains("OnePasswordConfig"));
        assert!(debug.contains("op://vault/item/field"));
    }

    #[test]
    fn test_config_deserialization_with_ref_key() {
        let json = r#"{"ref": "op://vault/item/field"}"#;
        let config: OnePasswordConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.reference, "op://vault/item/field");
    }

    #[test]
    fn test_config_deserialization_camel_case() {
        // Since serde uses camelCase, the field is "ref"
        let json = r#"{"ref": "op://example/test/password"}"#;
        let config: OnePasswordConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.reference, "op://example/test/password");
    }

    #[test]
    fn test_config_deserialization_missing_ref() {
        let json = r"{}";
        let result = serde_json::from_str::<OnePasswordConfig>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_with_special_characters() {
        let config = OnePasswordConfig::new("op://My Vault/My Item 2024/api-key_v1");
        assert!(config.reference.contains("My Vault"));
        assert!(config.reference.contains("api-key_v1"));
    }

    #[test]
    fn test_http_mode_available_without_env() {
        // Without OP_SERVICE_ACCOUNT_TOKEN, HTTP mode should not be available
        // (unless already set in environment)
        let result = OnePasswordResolver::http_mode_available();
        // Just verify it returns a boolean without panicking
        let _ = result;
    }

    #[test]
    fn test_resolver_provider_name() {
        // Create a resolver in CLI mode (without WASM)
        // If WASM is not available and token is not set, this should work
        if (!wasm::onepassword_wasm_available()
            || std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_err())
            && let Ok(resolver) = OnePasswordResolver::new()
        {
            assert_eq!(resolver.provider_name(), "onepassword");
        }
    }

    #[test]
    fn test_resolver_supports_native_batch() {
        if (!wasm::onepassword_wasm_available()
            || std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_err())
            && let Ok(resolver) = OnePasswordResolver::new()
        {
            assert!(resolver.supports_native_batch());
        }
    }

    #[test]
    fn test_resolver_can_use_http_false_without_client() {
        // A resolver without client_id should return false for can_use_http
        if (!wasm::onepassword_wasm_available()
            || std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_err())
            && let Ok(resolver) = OnePasswordResolver::new()
        {
            assert!(!resolver.can_use_http());
        }
    }

    #[test]
    fn test_resolver_debug_output() {
        if (!wasm::onepassword_wasm_available()
            || std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_err())
            && let Ok(resolver) = OnePasswordResolver::new()
        {
            let debug = format!("{resolver:?}");
            assert!(debug.contains("OnePasswordResolver"));
            // Should show mode as cli when no WASM client
            assert!(debug.contains("cli") || debug.contains("http"));
        }
    }

    #[tokio::test]
    async fn test_resolve_batch_empty() {
        if (!wasm::onepassword_wasm_available()
            || std::env::var("OP_SERVICE_ACCOUNT_TOKEN").is_err())
            && let Ok(resolver) = OnePasswordResolver::new()
        {
            let empty: HashMap<String, SecretSpec> = HashMap::new();
            let result = resolver.resolve_batch(&empty).await;
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        }
    }

    #[test]
    fn test_config_roundtrip_serialization() {
        let original = OnePasswordConfig::new("op://vault/item/field");
        let json = serde_json::to_string(&original).unwrap();
        let parsed: OnePasswordConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_config_empty_reference() {
        // Empty reference should be allowed at config level
        let config = OnePasswordConfig::new("");
        assert_eq!(config.reference, "");
    }

    #[test]
    fn test_config_unicode_reference() {
        let config = OnePasswordConfig::new("op://vault/项目/密码");
        assert_eq!(config.reference, "op://vault/项目/密码");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_cli_preflight_runs_once_for_parallel_batch_reads() {
        let temp = tempfile::tempdir().unwrap();
        write_fake_op_shim(temp.path());
        let log_path = temp.path().join("op.log");
        let path = prepend_path(temp.path());
        let log_path_str = log_path.to_string_lossy().into_owned();

        temp_env::async_with_vars(
            [
                ("PATH", Some(path.as_str())),
                ("OP_TEST_LOG", Some(log_path_str.as_str())),
                ("OP_SERVICE_ACCOUNT_TOKEN", None),
            ],
            async {
                let resolver = OnePasswordResolver::new().unwrap();
                let secrets = HashMap::from([
                    (
                        "API_KEY".to_string(),
                        SecretSpec::new("op://vault/service/api_key"),
                    ),
                    (
                        "DB_PASSWORD".to_string(),
                        SecretSpec::new("op://vault/service/db_password"),
                    ),
                    (
                        "JWT_SECRET".to_string(),
                        SecretSpec::new("op://vault/service/jwt_secret"),
                    ),
                ]);

                let resolved = resolver.resolve_batch(&secrets).await.unwrap();
                assert_eq!(resolved.len(), 3);
            },
        )
        .await;

        let lines = read_log_lines(&log_path);
        assert_eq!(
            lines.iter().filter(|line| *line == "whoami").count(),
            1,
            "expected exactly one auth preflight, got log lines: {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("read:"))
                .count(),
            3,
            "expected one read per secret, got log lines: {lines:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_cli_preflight_fail_fast_skips_parallel_reads() {
        let temp = tempfile::tempdir().unwrap();
        write_fake_op_shim(temp.path());
        let log_path = temp.path().join("op.log");
        let path = prepend_path(temp.path());
        let log_path_str = log_path.to_string_lossy().into_owned();

        temp_env::async_with_vars(
            [
                ("PATH", Some(path.as_str())),
                ("OP_TEST_LOG", Some(log_path_str.as_str())),
                ("OP_TEST_FAIL_WHOAMI", Some("1")),
                ("OP_SERVICE_ACCOUNT_TOKEN", None),
            ],
            async {
                let resolver = OnePasswordResolver::new().unwrap();
                let secrets = HashMap::from([
                    ("A".to_string(), SecretSpec::new("op://vault/item/a")),
                    ("B".to_string(), SecretSpec::new("op://vault/item/b")),
                ]);

                let result = resolver.resolve_batch(&secrets).await;
                assert!(result.is_err());
                let err = result.unwrap_err().to_string();
                assert!(err.contains("op whoami"), "unexpected error: {err}");
                assert!(err.contains("op signin"), "unexpected error: {err}");
            },
        )
        .await;

        let lines = read_log_lines(&log_path);
        assert_eq!(
            lines.iter().filter(|line| *line == "whoami").count(),
            1,
            "expected single preflight attempt, got log lines: {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("read:"))
                .count(),
            0,
            "read should not run when preflight fails, got log lines: {lines:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_cli_preflight_missing_op_reports_clear_error() {
        let empty_dir = tempfile::tempdir().unwrap();
        let path = empty_dir.path().to_string_lossy().into_owned();

        temp_env::async_with_vars(
            [
                ("PATH", Some(path.as_str())),
                ("OP_SERVICE_ACCOUNT_TOKEN", None),
            ],
            async {
                let resolver = OnePasswordResolver::new().unwrap();
                let spec = SecretSpec::new("op://vault/item/password");

                let result = resolver.resolve("missing-op", &spec).await;
                assert!(result.is_err());
                let err = result.unwrap_err().to_string();
                assert!(err.contains("1Password CLI"), "unexpected error: {err}");
                assert!(err.contains("not found"), "unexpected error: {err}");
            },
        )
        .await;
    }
}
