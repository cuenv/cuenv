//! Command execution secret resolver

use crate::{SecretError, SecretResolver, SecretSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::process::Command;

/// Configuration for exec-based secret resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ExecSecretConfig {
    /// Command to execute
    command: String,

    /// Arguments to pass to the command
    #[serde(default)]
    args: Vec<String>,

    /// Additional fields for extensibility
    #[serde(flatten)]
    extra: HashMap<String, Value>,
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
    pub const fn new() -> Self {
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
            // The command's error output is deliberately not quoted: a failing
            // secret command (a vault CLI, a token helper) may print the
            // secret, or the credentials it was given, on standard error, and
            // nothing is registered for redaction yet, because the secret was
            // never resolved. Run the command yourself to see its output.
            let exit = output.status.code().map_or_else(
                || "was terminated by a signal".to_string(),
                |code| format!("exited with status {code}"),
            );
            return Err(SecretError::ResolutionFailed {
                name: name.to_string(),
                message: format!(
                    "Command '{command}' {exit}; its error output is not shown because it can \
                     contain secret material (run the command yourself to see it)"
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    }
}

#[async_trait]
impl SecretResolver for ExecSecretResolver {
    fn provider_name(&self) -> &'static str {
        "exec"
    }

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
        let config = ExecSecretConfig {
            command: "echo".to_string(),
            args: vec!["json_value".to_string()],
            extra: HashMap::new(),
        };
        let json_source = serde_json::to_string(&config).unwrap();

        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new(json_source);
        let result = resolver.resolve("test", &spec).await;

        assert_eq!(result.unwrap(), "json_value");
    }

    #[tokio::test]
    async fn a_failing_command_never_echoes_its_error_output() {
        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new("echo token=TOPSECRET-VALUE-9 >&2; exit 3");
        let error = resolver.resolve("test", &spec).await.unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("TOPSECRET"), "{message}");
        assert!(message.contains("exited with status 3"), "{message}");
    }

    #[tokio::test]
    async fn test_exec_command_failure() {
        let resolver = ExecSecretResolver::new();
        let spec = SecretSpec::new("exit 1");
        let result = resolver.resolve("test", &spec).await;

        assert!(matches!(result, Err(SecretError::ResolutionFailed { .. })));
    }
}
