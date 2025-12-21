//! Secret and resolver types
//!
//! Based on schema/secrets.cue
//!
//! This module provides:
//! - `Secret`: CUE-compatible secret definition with resolver-based resolution
//! - Re-exports from `cuenv_secrets`: Trait-based secret resolution system

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// Re-export core secret resolution types from cuenv-secrets
pub use cuenv_secrets::{
    BatchResolver, ResolvedSecrets, SaltConfig, SecretError, SecretResolver, SecretSpec,
    compute_secret_fingerprint,
};

// Re-export resolver implementations
pub use cuenv_secrets::resolvers::{EnvSecretResolver, ExecSecretResolver};

// Re-export 1Password resolver from its dedicated crate
pub use cuenv_1password::secrets::{OnePasswordConfig, OnePasswordResolver};

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
/// variable resolution. Supports multiple resolver types:
/// - `exec`: Execute a command to get the secret
/// - `onepassword`: Resolve from 1Password using `ref` field
/// - `aws`, `gcp`, `vault`: Cloud provider secrets
///
/// Resolution is delegated to the trait-based [`SecretResolver`] system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Secret {
    /// Resolver type: "exec", "onepassword", "aws", "gcp", "vault"
    pub resolver: String,

    /// Command to execute (for exec resolver)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,

    /// Arguments to pass to the command (for exec resolver)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// 1Password reference (for onepassword resolver), e.g., "op://vault/item/field"
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub op_ref: Option<String>,

    /// Additional fields for extensibility
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Secret {
    /// Create a new exec secret
    #[must_use]
    pub fn new(command: String, args: Vec<String>) -> Self {
        Secret {
            resolver: "exec".to_string(),
            command,
            args,
            op_ref: None,
            extra: HashMap::new(),
        }
    }

    /// Create a 1Password secret
    #[must_use]
    pub fn onepassword(reference: impl Into<String>) -> Self {
        Secret {
            resolver: "onepassword".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: Some(reference.into()),
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
            op_ref: None,
            extra,
        }
    }

    /// Get the resolver/provider name
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.resolver
    }

    /// Convert to a SecretSpec for use with the trait-based resolver system
    #[must_use]
    pub fn to_spec(&self) -> SecretSpec {
        let source = match self.resolver.as_str() {
            "onepassword" => self.op_ref.clone().unwrap_or_default(),
            "exec" => serde_json::json!({
                "command": self.command,
                "args": self.args
            })
            .to_string(),
            // For other resolvers, serialize the whole secret
            _ => serde_json::to_string(self).unwrap_or_default(),
        };
        SecretSpec::new(source)
    }

    /// Resolve the secret value using the trait-based resolver system
    ///
    /// # Errors
    /// Returns error if resolution fails
    pub async fn resolve(&self) -> Result<String> {
        let spec = self.to_spec();

        match self.resolver.as_str() {
            "onepassword" => {
                let resolver = OnePasswordResolver::new().map_err(|e| {
                    Error::configuration(format!("Failed to initialize 1Password resolver: {e}"))
                })?;
                resolver
                    .resolve("secret", &spec)
                    .await
                    .map_err(|e| Error::configuration(format!("{e}")))
            }
            "exec" => {
                let resolver = ExecSecretResolver::new();
                resolver
                    .resolve("secret", &spec)
                    .await
                    .map_err(|e| Error::configuration(format!("{e}")))
            }
            other => Err(Error::configuration(format!(
                "Unsupported secret resolver: {other}"
            ))),
        }
    }
}
