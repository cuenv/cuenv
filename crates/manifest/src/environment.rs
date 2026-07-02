//! Environment schema value DTOs.
//!
//! Async secret resolution for these values lives in `cuenv-core`
//! (`EnvValueExt`); this module holds only the serde types and pure helpers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Placeholder for redacted secrets; mirrors `cuenv_events::REDACTED_PLACEHOLDER`.
const REDACTED_PLACEHOLDER: &str = "*_*";

/// A part of an interpolated environment variable value.
/// Can be a literal string or a secret that needs runtime resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EnvPart {
    /// A secret that needs runtime resolution (must come first for serde untagged)
    Secret(crate::secrets::Secret),
    /// A literal string value
    Literal(String),
}

impl EnvPart {
    /// Check if this part is a secret
    #[must_use]
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Secret(_))
    }
}

/// Policy for controlling environment variable access
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Policy {
    /// Allowlist of task names that can access this variable
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowTasks")]
    pub allow_tasks: Option<Vec<String>>,

    /// Allowlist of exec commands that can access this variable
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowExec")]
    pub allow_exec: Option<Vec<String>>,
}

/// Environment variable with optional access policies
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvVarWithPolicies {
    /// The actual value
    pub value: EnvValueSimple,

    /// Optional access policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<Policy>>,
}

/// Simple environment variable values (non-recursive)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EnvValueSimple {
    /// A secret that needs runtime resolution
    Secret(crate::secrets::Secret),
    /// An interpolated value composed of literal strings and secrets
    Interpolated(Vec<EnvPart>),
    /// A simple string value
    String(String),
    /// An integer value
    Int(i64),
    /// A boolean value
    Bool(bool),
}

/// Environment variable values can be strings, integers, booleans, secrets,
/// interpolated arrays, or values with policies.
/// When exported to actual environment, these will always be strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EnvValue {
    /// Value with access policies. Must come first for serde untagged to try
    /// it first (it's an object with a specific "value" + "policies" shape).
    WithPolicies(EnvVarWithPolicies),
    /// A secret that needs runtime resolution. Must come before `String` to
    /// parse `{"resolver": ...}` correctly.
    Secret(crate::secrets::Secret),
    /// An interpolated value composed of literals and secrets. Must come
    /// before the simple types.
    Interpolated(Vec<EnvPart>),
    /// A simple string value
    String(String),
    /// An integer value
    Int(i64),
    /// A boolean value
    Bool(bool),
}

/// Environment configuration with environment-specific overrides
/// Based on schema/env.cue
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Env {
    /// Environment-specific overrides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, HashMap<String, EnvValue>>>,

    /// Base environment variables
    /// Keys must match pattern: ^[A-Z][A-Z0-9_]*$
    #[serde(flatten)]
    pub base: HashMap<String, EnvValue>,
}

impl Env {
    /// Get environment variables for a specific environment
    #[must_use]
    pub fn for_environment(&self, env_name: &str) -> HashMap<String, EnvValue> {
        let mut result = self.base.clone();

        if let Some(environments) = &self.environment
            && let Some(env_overrides) = environments.get(env_name)
        {
            result.extend(env_overrides.clone());
        }

        result
    }
}

impl EnvValue {
    /// Check if a task has access to this environment variable
    #[must_use]
    pub fn is_accessible_by_task(&self, task_name: &str) -> bool {
        match self {
            // Simple values are always accessible
            Self::String(_)
            | Self::Int(_)
            | Self::Bool(_)
            | Self::Secret(_)
            | Self::Interpolated(_) => true,

            // Check policies for restricted variables
            Self::WithPolicies(var) => match &var.policies {
                None => true,                                  // No policies means accessible
                Some(policies) if policies.is_empty() => true, // Empty policies means accessible
                Some(policies) => {
                    // Check if any policy allows this task
                    policies.iter().any(|policy| {
                        policy
                            .allow_tasks
                            .as_ref()
                            .is_some_and(|tasks| tasks.iter().any(|t| t == task_name))
                    })
                }
            },
        }
    }

    /// Check if an exec command has access to this environment variable
    #[must_use]
    pub fn is_accessible_by_exec(&self, command: &str) -> bool {
        match self {
            // Simple values are always accessible
            Self::String(_)
            | Self::Int(_)
            | Self::Bool(_)
            | Self::Secret(_)
            | Self::Interpolated(_) => true,

            // Check policies for restricted variables
            Self::WithPolicies(var) => match &var.policies {
                None => true,                                  // No policies means accessible
                Some(policies) if policies.is_empty() => true, // Empty policies means accessible
                Some(policies) => {
                    // Check if any policy allows this exec command
                    policies.iter().any(|policy| {
                        policy
                            .allow_exec
                            .as_ref()
                            .is_some_and(|execs| execs.iter().any(|e| e == command))
                    })
                }
            },
        }
    }

    /// Get the actual string value of the environment variable.
    /// Secrets are redacted as `*_*` placeholders.
    #[must_use]
    pub fn to_string_value(&self) -> String {
        match self {
            Self::String(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Secret(_) => REDACTED_PLACEHOLDER.to_string(),
            Self::Interpolated(parts) => Self::parts_to_string_value(parts),
            Self::WithPolicies(var) => match &var.value {
                EnvValueSimple::String(s) => s.clone(),
                EnvValueSimple::Int(i) => i.to_string(),
                EnvValueSimple::Bool(b) => b.to_string(),
                EnvValueSimple::Secret(_) => REDACTED_PLACEHOLDER.to_string(),
                EnvValueSimple::Interpolated(parts) => Self::parts_to_string_value(parts),
            },
        }
    }

    /// Convert interpolated parts to a string value with secrets redacted.
    fn parts_to_string_value(parts: &[EnvPart]) -> String {
        parts
            .iter()
            .map(|p| match p {
                EnvPart::Literal(s) => s.clone(),
                EnvPart::Secret(_) => REDACTED_PLACEHOLDER.to_string(),
            })
            .collect()
    }

    /// Check if this environment value contains any secrets (requires resolution)
    #[must_use]
    pub fn is_secret(&self) -> bool {
        match self {
            Self::Secret(_) => true,
            Self::Interpolated(parts) => parts.iter().any(EnvPart::is_secret),
            Self::WithPolicies(var) => match &var.value {
                EnvValueSimple::Secret(_) => true,
                EnvValueSimple::Interpolated(parts) => parts.iter().any(EnvPart::is_secret),
                _ => false,
            },
            _ => false,
        }
    }

    /// Collect all secrets from this value, returning them with their part index.
    ///
    /// The part index is used to match resolved values back to their position
    /// during reassembly. For non-interpolated secrets, index 0 is used.
    #[doc(hidden)]
    #[must_use]
    pub fn collect_secrets(&self) -> Vec<(usize, &crate::secrets::Secret)> {
        match self {
            Self::Secret(s) => vec![(0, s)],
            Self::Interpolated(parts) => Self::collect_secrets_from_parts(parts),
            Self::WithPolicies(var) => match &var.value {
                EnvValueSimple::Secret(s) => vec![(0, s)],
                EnvValueSimple::Interpolated(parts) => Self::collect_secrets_from_parts(parts),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// Collect secrets from interpolated parts with their indices.
    fn collect_secrets_from_parts(parts: &[EnvPart]) -> Vec<(usize, &crate::secrets::Secret)> {
        parts
            .iter()
            .enumerate()
            .filter_map(|(i, part)| match part {
                EnvPart::Secret(s) => Some((i, s)),
                EnvPart::Literal(_) => None,
            })
            .collect()
    }

    /// Reassemble the resolved string value given pre-resolved secret values.
    ///
    /// `resolved_secrets` maps part indices to their resolved string values.
    /// Returns the final concatenated string and the list of secret values for redaction.
    #[doc(hidden)]
    #[must_use]
    pub fn reassemble_with_resolved(
        &self,
        resolved_secrets: &HashMap<usize, String>,
    ) -> (String, Vec<String>) {
        match self {
            Self::String(s) => (s.clone(), vec![]),
            Self::Int(i) => (i.to_string(), vec![]),
            Self::Bool(b) => (b.to_string(), vec![]),
            Self::Secret(_) => {
                let val = resolved_secrets.get(&0).cloned().unwrap_or_default();
                (val.clone(), vec![val])
            }
            Self::Interpolated(parts) => Self::reassemble_parts(parts, resolved_secrets),
            Self::WithPolicies(var) => match &var.value {
                EnvValueSimple::String(s) => (s.clone(), vec![]),
                EnvValueSimple::Int(i) => (i.to_string(), vec![]),
                EnvValueSimple::Bool(b) => (b.to_string(), vec![]),
                EnvValueSimple::Secret(_) => {
                    let val = resolved_secrets.get(&0).cloned().unwrap_or_default();
                    (val.clone(), vec![val])
                }
                EnvValueSimple::Interpolated(parts) => {
                    Self::reassemble_parts(parts, resolved_secrets)
                }
            },
        }
    }

    /// Reassemble interpolated parts using pre-resolved secret values.
    fn reassemble_parts(
        parts: &[EnvPart],
        resolved_secrets: &HashMap<usize, String>,
    ) -> (String, Vec<String>) {
        let mut result = String::new();
        let mut secrets = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            match part {
                EnvPart::Literal(s) => result.push_str(s),
                EnvPart::Secret(_) => {
                    if let Some(val) = resolved_secrets.get(&i) {
                        result.push_str(val);
                        secrets.push(val.clone());
                    }
                }
            }
        }
        (result, secrets)
    }
}
