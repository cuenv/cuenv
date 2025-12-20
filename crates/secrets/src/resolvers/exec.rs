//! Command execution secret resolver

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::process::Command;

/// Configuration for exec-based secret resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecSecretConfig {
    /// Command to execute
    pub command: String,

    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Additional fields for extensibility
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl ExecSecretConfig {
    /// Create a new exec secret config
    #[must_use]
    #[allow(dead_code)] // Public API
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            extra: HashMap::new(),
        }
    }
}

/// Resolves secrets by executing commands
///
/// The `source` field in [`SecretSpec`] is interpreted as a JSON-encoded
/// [`ExecSecretConfig`], or as a simple command string if parsing fails.
#[derive(Debug, Clone, Default)]
pub struct ExecSecretResolver;

impl ExecSecretResolver {
    /// Create a new command execution resolver
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Execute a command and return its output
    async fn execute_command(
        &self,
        name: &str,
        command: &str,
        args: &[String],
    ) -> Result<String, SecretError> {
        let output = Command::new(command)
            .args(args)
            .output()
            .await
            .map_err(|e| SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Failed to execute command '{command}': {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!("Command '{command}' failed: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    }
}

#[async_trait]
impl SecretResolver for ExecSecretResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        // Try to parse source as JSON ExecSecretConfig
        if let Ok(config) = serde_json::from_str::<ExecSecretConfig>(&spec.source) {
            return self
                .execute_command(name, &config.command, &config.args)
                .await;
        }

        // Fallback: treat source as a simple command (shell expansion)
        self.execute_command(name, "sh", &["-c".to_string(), spec.source.clone()])
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exec_simple_command() {
        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new("echo test_value");
        let result = resolver.resolve("test", &spec).await;

        assert_eq!(result.unwrap(), "test_value");
    }

    #[tokio::test]
    async fn test_exec_json_config() {
        let config = ExecSecretConfig::new("echo", vec!["json_value".to_string()]);
        let json_source = serde_json::to_string(&config).unwrap();

        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new(json_source);
        let result = resolver.resolve("test", &spec).await;

        assert_eq!(result.unwrap(), "json_value");
    }

    #[tokio::test]
    async fn test_exec_command_failure() {
        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new("exit 1");
        let result = resolver.resolve("test", &spec).await;

        assert!(matches!(result, Err(SecretError::ResolutionFailed { .. })));
    }
}
