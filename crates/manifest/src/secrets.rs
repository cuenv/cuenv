//! Secret definition DTO.
//!
//! Based on schema/secrets.cue. Resolution is delegated to the trait-based
//! resolver system in `cuenv-secrets`; the registry wiring and resolution
//! extension methods live in `cuenv-core`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Secret definition with resolver
///
/// This is the CUE-compatible secret type used for environment variable
/// resolution. Supports multiple resolver types:
/// - `exec`: Execute a command to get the secret
/// - `onepassword`: Resolve from 1Password using `ref` field
/// - `infisical`: Resolve from Infisical using explicit project/environment/secret fields
/// - `aws`, `gcp`, `vault`: Cloud provider secrets
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Secret {
    /// Resolver type: "exec", "onepassword", "infisical", "aws", "gcp", "vault"
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
    #[serde(flatten, deserialize_with = "ordinary_secret_fields")]
    pub extra: HashMap<String, Value>,
}

impl Secret {
    /// Create a new exec secret
    #[must_use]
    pub fn new(command: String, args: Vec<String>) -> Self {
        Self {
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
        Self {
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
        Self {
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
}

fn ordinary_secret_fields<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, Value>, D::Error> {
    let fields = HashMap::<String, Value>::deserialize(deserializer)?;
    if fields.contains_key("writeOnly") {
        return Err(serde::de::Error::custom(
            "writeOnly is only supported on infrastructure managed-resource arguments",
        ));
    }
    Ok(fields)
}

#[cfg(test)]
mod write_only_tests {
    use super::Secret;
    use serde_json::json;

    #[test]
    fn ordinary_secrets_reject_write_only_metadata() {
        for marker in [json!(true), json!(false), json!(null), json!("true")] {
            let result = serde_json::from_value::<Secret>(
                json!({"resolver":"exec", "command":"unused", "writeOnly":marker}),
            );
            assert!(result.is_err());
        }
    }
}
