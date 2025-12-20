//! Secret and resolver types
//!
//! Based on schema/secrets.cue
//!
//! This module provides:
//! - `Secret`: CUE-compatible secret definition with exec-based resolution (for Dagger)
//! - Re-exports from `cuenv_secrets`: Trait-based secret resolution system

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::process::Command;

// Re-export core secret resolution types from cuenv-secrets
pub use cuenv_secrets::{
    compute_secret_fingerprint, ResolvedSecrets, SaltConfig, SecretError, SecretResolver,
    SecretSpec,
};

// Re-export resolver implementations
pub use cuenv_secrets::resolvers::{EnvSecretResolver, ExecSecretResolver};

/// Resolver for executing commands to retrieve secret values
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecResolver {
    /// Command to execute
    pub command: String,

    /// Arguments to pass to the command
    pub args: Vec<String>,
}

/// Secret definition with resolver
///
/// This is the CUE-compatible secret type used for Dagger secrets and environment
/// variable resolution. For the trait-based resolver system, see [`SecretResolver`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Secret {
    /// Resolver type (currently only "exec" is supported)
    pub resolver: String,

    /// Command to execute
    pub command: String,

    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Additional fields for extensibility
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Secret {
    /// Create a new secret with a resolver
    #[must_use]
    pub fn new(command: String, args: Vec<String>) -> Self {
        Secret {
            resolver: "exec".to_string(),
            command,
            args,
            extra: HashMap::new(),
        }
    }

    /// Create a secret with additional fields
    #[must_use]
    pub fn with_extra(command: String, args: Vec<String>, extra: HashMap<String, Value>) -> Self {
        Secret {
            resolver: "exec".to_string(),
            command,
            args,
            extra,
        }
    }

    /// Resolve the secret value
    ///
    /// # Errors
    /// Returns error if the command fails to execute or returns non-zero exit code
    pub async fn resolve(&self) -> Result<String> {
        match self.resolver.as_str() {
            "exec" => {
                let output = Command::new(&self.command)
                    .args(&self.args)
                    .output()
                    .await
                    .map_err(|e| {
                        Error::configuration(format!(
                            "Failed to execute secret resolver command '{}': {}",
                            self.command, e
                        ))
                    })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(Error::configuration(format!(
                        "Secret resolver command '{}' failed: {}",
                        self.command, stderr
                    )));
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(stdout.trim().to_string())
            }
            other => Err(Error::configuration(format!(
                "Unsupported secret resolver: {}",
                other
            ))),
        }
    }

    /// Convert to a SecretSpec for use with the trait-based resolver system
    #[must_use]
    pub fn to_spec(&self) -> SecretSpec {
        // Encode as JSON for the exec resolver
        let source = serde_json::json!({
            "command": self.command,
            "args": self.args
        })
        .to_string();

        SecretSpec::new(source)
    }
}
