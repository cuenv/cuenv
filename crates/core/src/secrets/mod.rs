//! Secret and resolver types
//!
//! Based on schema/secrets.cue
//!
//! This module provides:
//! - `Secret`: CUE-compatible secret definition with resolver-based resolution
//! - `SecretRegistry`: Dynamic resolver registration and lookup
//! - `create_default_registry()`: Creates a registry with built-in resolvers
//! - Re-exports from `cuenv_secrets`: Trait-based secret resolution system

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// Re-export core secret resolution types from cuenv-secrets
pub use cuenv_secrets::{
    BatchResolver, ResolvedSecrets, SaltConfig, SecretError, SecretRegistry, SecretResolver,
    SecretSpec, compute_secret_fingerprint,
};

// Re-export resolver implementations
pub use cuenv_secrets::resolvers::{EnvSecretResolver, ExecSecretResolver};

// Conditionally re-export 1Password resolver when feature is enabled
#[cfg(feature = "1password")]
pub use cuenv_1password::secrets::{OnePasswordConfig, OnePasswordResolver};

/// Create a default secret registry with all built-in resolvers
///
/// This registers:
/// - `env`: Environment variable resolver
/// - `exec`: Command execution resolver
/// - `onepassword`: 1Password resolver (when `1password` feature is enabled)
///
/// # Errors
///
/// Returns an error if 1Password resolver initialization fails (when enabled).
#[allow(clippy::unnecessary_wraps)]
pub fn create_default_registry() -> Result<SecretRegistry> {
    let mut registry = SecretRegistry::new();

    // Register built-in resolvers
    registry.register(Arc::new(EnvSecretResolver::new()));
    registry.register(Arc::new(ExecSecretResolver::new()));

    // Register 1Password resolver if feature is enabled
    #[cfg(feature = "1password")]
    {
        let op_resolver = OnePasswordResolver::new().map_err(|e| {
            Error::configuration(format!("Failed to initialize 1Password resolver: {e}"))
        })?;
        registry.register(Arc::new(op_resolver));
    }

    Ok(registry)
}

/// Resolver for executing commands to retrieve secret values
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Uses the default registry with all built-in resolvers.
    ///
    /// # Errors
    /// Returns error if resolution fails
    pub async fn resolve(&self) -> Result<String> {
        tracing::debug!(resolver = %self.resolver, op_ref = ?self.op_ref, "Secret::resolve() called");
        let registry = create_default_registry()?;
        self.resolve_with_registry(&registry).await
    }

    /// Resolve the secret value using a custom registry
    ///
    /// # Errors
    /// Returns error if resolution fails
    pub async fn resolve_with_registry(&self, registry: &SecretRegistry) -> Result<String> {
        let spec = self.to_spec();

        registry
            .resolve(&self.resolver, "secret", &spec)
            .await
            .map_err(|e| Error::configuration(format!("{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ==========================================================================
    // ExecResolver tests
    // ==========================================================================

    #[test]
    fn test_exec_resolver_new() {
        let resolver = ExecResolver {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
        };

        assert_eq!(resolver.command, "echo");
        assert_eq!(resolver.args, vec!["hello"]);
    }

    #[test]
    fn test_exec_resolver_serde_roundtrip() {
        let resolver = ExecResolver {
            command: "/usr/bin/vault".to_string(),
            args: vec![
                "read".to_string(),
                "-field=value".to_string(),
                "secret/data".to_string(),
            ],
        };

        let json = serde_json::to_string(&resolver).unwrap();
        let parsed: ExecResolver = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.command, resolver.command);
        assert_eq!(parsed.args, resolver.args);
    }

    #[test]
    fn test_exec_resolver_clone() {
        let resolver = ExecResolver {
            command: "cmd".to_string(),
            args: vec!["arg1".to_string()],
        };

        let cloned = resolver.clone();
        assert_eq!(cloned.command, resolver.command);
        assert_eq!(cloned.args, resolver.args);
    }

    #[test]
    fn test_exec_resolver_eq() {
        let r1 = ExecResolver {
            command: "cmd".to_string(),
            args: vec!["arg".to_string()],
        };
        let r2 = ExecResolver {
            command: "cmd".to_string(),
            args: vec!["arg".to_string()],
        };
        let r3 = ExecResolver {
            command: "other".to_string(),
            args: vec![],
        };

        assert_eq!(r1, r2);
        assert_ne!(r1, r3);
    }

    // ==========================================================================
    // Secret construction tests
    // ==========================================================================

    #[test]
    fn test_secret_new_exec() {
        let secret = Secret::new("echo".to_string(), vec!["hello".to_string()]);

        assert_eq!(secret.resolver, "exec");
        assert_eq!(secret.command, "echo");
        assert_eq!(secret.args, vec!["hello"]);
        assert!(secret.op_ref.is_none());
        assert!(secret.extra.is_empty());
    }

    #[test]
    fn test_secret_onepassword() {
        let secret = Secret::onepassword("op://vault/item/field");

        assert_eq!(secret.resolver, "onepassword");
        assert_eq!(secret.op_ref, Some("op://vault/item/field".to_string()));
        assert!(secret.command.is_empty());
        assert!(secret.args.is_empty());
    }

    #[test]
    fn test_secret_onepassword_with_into() {
        let secret = Secret::onepassword(String::from("op://my-vault/my-item/password"));

        assert_eq!(secret.resolver, "onepassword");
        assert_eq!(
            secret.op_ref,
            Some("op://my-vault/my-item/password".to_string())
        );
    }

    #[test]
    fn test_secret_with_extra() {
        let mut extra = HashMap::new();
        extra.insert("region".to_string(), json!("us-east-1"));
        extra.insert("version".to_string(), json!(2));

        let secret = Secret::with_extra(
            "aws".to_string(),
            vec!["secretsmanager".to_string(), "get-secret-value".to_string()],
            extra.clone(),
        );

        assert_eq!(secret.resolver, "exec");
        assert_eq!(secret.command, "aws");
        assert_eq!(secret.extra, extra);
    }

    // ==========================================================================
    // Secret::provider tests
    // ==========================================================================

    #[test]
    fn test_secret_provider_exec() {
        let secret = Secret::new("cmd".to_string(), vec![]);
        assert_eq!(secret.provider(), "exec");
    }

    #[test]
    fn test_secret_provider_onepassword() {
        let secret = Secret::onepassword("op://vault/item/field");
        assert_eq!(secret.provider(), "onepassword");
    }

    #[test]
    fn test_secret_provider_custom() {
        let secret = Secret {
            resolver: "vault".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None,
            extra: HashMap::new(),
        };
        assert_eq!(secret.provider(), "vault");
    }

    // ==========================================================================
    // Secret::to_spec tests
    // ==========================================================================

    #[test]
    fn test_secret_to_spec_onepassword() {
        let secret = Secret::onepassword("op://vault/item/field");
        let spec = secret.to_spec();

        assert_eq!(spec.source, "op://vault/item/field");
    }

    #[test]
    fn test_secret_to_spec_exec() {
        let secret = Secret::new("echo".to_string(), vec!["hello".to_string()]);
        let spec = secret.to_spec();

        let source = &spec.source;
        assert!(source.contains("echo"));
        assert!(source.contains("hello"));
    }

    #[test]
    fn test_secret_to_spec_onepassword_empty_ref() {
        let secret = Secret {
            resolver: "onepassword".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None, // Missing ref
            extra: HashMap::new(),
        };
        let spec = secret.to_spec();

        // Should return empty string for missing ref
        assert_eq!(spec.source, "");
    }

    #[test]
    fn test_secret_to_spec_other_resolver() {
        let mut extra = HashMap::new();
        extra.insert("path".to_string(), json!("secret/data/myapp"));

        let secret = Secret {
            resolver: "vault".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None,
            extra,
        };
        let spec = secret.to_spec();

        // For other resolvers, the whole secret is serialized
        let source = &spec.source;
        assert!(source.contains("vault"));
        assert!(source.contains("path"));
    }

    // ==========================================================================
    // Secret serde tests
    // ==========================================================================

    #[test]
    fn test_secret_serde_exec_roundtrip() {
        let secret = Secret::new("echo".to_string(), vec!["test".to_string()]);
        let json = serde_json::to_string(&secret).unwrap();
        let parsed: Secret = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.resolver, secret.resolver);
        assert_eq!(parsed.command, secret.command);
        assert_eq!(parsed.args, secret.args);
    }

    #[test]
    fn test_secret_serde_onepassword_roundtrip() {
        let secret = Secret::onepassword("op://vault/item/field");
        let json = serde_json::to_string(&secret).unwrap();
        let parsed: Secret = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.resolver, secret.resolver);
        assert_eq!(parsed.op_ref, secret.op_ref);
    }

    #[test]
    fn test_secret_serde_from_json() {
        let json = r#"{
            "resolver": "exec",
            "command": "vault",
            "args": ["read", "-field=value", "secret/data"]
        }"#;

        let secret: Secret = serde_json::from_str(json).unwrap();
        assert_eq!(secret.resolver, "exec");
        assert_eq!(secret.command, "vault");
        assert_eq!(secret.args.len(), 3);
    }

    #[test]
    fn test_secret_serde_onepassword_ref_field() {
        // Test that "ref" field is properly deserialized
        let json = r#"{
            "resolver": "onepassword",
            "ref": "op://vault/item/password"
        }"#;

        let secret: Secret = serde_json::from_str(json).unwrap();
        assert_eq!(secret.resolver, "onepassword");
        assert_eq!(secret.op_ref, Some("op://vault/item/password".to_string()));
    }

    #[test]
    fn test_secret_serde_extra_fields() {
        let json = r#"{
            "resolver": "aws",
            "command": "",
            "secret_id": "arn:aws:secretsmanager:us-east-1:123456789:secret:myapp",
            "region": "us-east-1"
        }"#;

        let secret: Secret = serde_json::from_str(json).unwrap();
        assert_eq!(secret.resolver, "aws");
        assert!(secret.extra.contains_key("secret_id"));
        assert!(secret.extra.contains_key("region"));
    }

    #[test]
    fn test_secret_serde_skip_empty_command() {
        let secret = Secret::onepassword("op://vault/item/field");
        let json = serde_json::to_string(&secret).unwrap();

        // Empty command should be skipped
        assert!(!json.contains("\"command\":"));
    }

    #[test]
    fn test_secret_serde_skip_empty_args() {
        let secret = Secret::onepassword("op://vault/item/field");
        let json = serde_json::to_string(&secret).unwrap();

        // Empty args should be skipped
        assert!(!json.contains("\"args\":"));
    }

    // ==========================================================================
    // Secret clone/eq tests
    // ==========================================================================

    #[test]
    fn test_secret_clone() {
        let secret = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
        let cloned = secret.clone();

        assert_eq!(cloned.resolver, secret.resolver);
        assert_eq!(cloned.command, secret.command);
        assert_eq!(cloned.args, secret.args);
    }

    #[test]
    fn test_secret_eq() {
        let s1 = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
        let s2 = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
        let s3 = Secret::onepassword("op://v/i/f");

        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_secret_debug() {
        let secret = Secret::new("echo".to_string(), vec!["test".to_string()]);
        let debug = format!("{:?}", secret);

        assert!(debug.contains("Secret"));
        assert!(debug.contains("exec"));
        assert!(debug.contains("echo"));
    }
}
