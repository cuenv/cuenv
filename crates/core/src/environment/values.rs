//! Environment schema values and secret-aware resolution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
        matches!(self, EnvPart::Secret(_))
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
    // Value with policies must come first for serde untagged to try it first
    // (it's an object with a specific "value" + "policies" shape)
    WithPolicies(EnvVarWithPolicies),
    // Secret must come before String to parse {"resolver": ...} correctly
    Secret(crate::secrets::Secret),
    // Interpolated array must come before simple types
    Interpolated(Vec<EnvPart>),
    // Simple values (backward compatible)
    String(String),
    Int(i64),
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
    pub fn is_accessible_by_task(&self, task_name: &str) -> bool {
        match self {
            // Simple values are always accessible
            EnvValue::String(_)
            | EnvValue::Int(_)
            | EnvValue::Bool(_)
            | EnvValue::Secret(_)
            | EnvValue::Interpolated(_) => true,

            // Check policies for restricted variables
            EnvValue::WithPolicies(var) => match &var.policies {
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
    pub fn is_accessible_by_exec(&self, command: &str) -> bool {
        match self {
            // Simple values are always accessible
            EnvValue::String(_)
            | EnvValue::Int(_)
            | EnvValue::Bool(_)
            | EnvValue::Secret(_)
            | EnvValue::Interpolated(_) => true,

            // Check policies for restricted variables
            EnvValue::WithPolicies(var) => match &var.policies {
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
    pub fn to_string_value(&self) -> String {
        match self {
            EnvValue::String(s) => s.clone(),
            EnvValue::Int(i) => i.to_string(),
            EnvValue::Bool(b) => b.to_string(),
            EnvValue::Secret(_) => cuenv_events::REDACTED_PLACEHOLDER.to_string(),
            EnvValue::Interpolated(parts) => Self::parts_to_string_value(parts),
            EnvValue::WithPolicies(var) => match &var.value {
                EnvValueSimple::String(s) => s.clone(),
                EnvValueSimple::Int(i) => i.to_string(),
                EnvValueSimple::Bool(b) => b.to_string(),
                EnvValueSimple::Secret(_) => cuenv_events::REDACTED_PLACEHOLDER.to_string(),
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
                EnvPart::Secret(_) => cuenv_events::REDACTED_PLACEHOLDER.to_string(),
            })
            .collect()
    }

    /// Check if this environment value contains any secrets (requires resolution)
    #[must_use]
    pub fn is_secret(&self) -> bool {
        match self {
            EnvValue::Secret(_) => true,
            EnvValue::Interpolated(parts) => parts.iter().any(EnvPart::is_secret),
            EnvValue::WithPolicies(var) => match &var.value {
                EnvValueSimple::Secret(_) => true,
                EnvValueSimple::Interpolated(parts) => parts.iter().any(EnvPart::is_secret),
                _ => false,
            },
            _ => false,
        }
    }

    /// Resolve the environment variable value, executing secrets if necessary
    ///
    /// Secrets with typed resolvers (onepassword, exec, aws, etc.) are resolved
    /// via the trait-based [`SecretResolver`] system.
    pub async fn resolve(&self) -> crate::Result<String> {
        let (resolved, _) = self.resolve_with_secrets().await?;
        Ok(resolved)
    }

    /// Resolve the environment variable, returning both the final value
    /// and a list of resolved secret values (for redaction).
    ///
    /// This is the preferred method when you need to track which values
    /// should be redacted from output.
    pub async fn resolve_with_secrets(&self) -> crate::Result<(String, Vec<String>)> {
        match self {
            EnvValue::String(s) => Ok((s.clone(), vec![])),
            EnvValue::Int(i) => Ok((i.to_string(), vec![])),
            EnvValue::Bool(b) => Ok((b.to_string(), vec![])),
            EnvValue::Secret(s) => {
                let resolved = s.resolve().await?;
                Ok((resolved.clone(), vec![resolved]))
            }
            EnvValue::Interpolated(parts) => Self::resolve_parts_with_secrets(parts).await,
            EnvValue::WithPolicies(var) => Self::resolve_simple_with_secrets(&var.value).await,
        }
    }

    /// Resolve interpolated parts, returning the concatenated result and secret values.
    async fn resolve_parts_with_secrets(parts: &[EnvPart]) -> crate::Result<(String, Vec<String>)> {
        let mut result = String::new();
        let mut secrets = Vec::new();

        for part in parts {
            match part {
                EnvPart::Literal(s) => result.push_str(s),
                EnvPart::Secret(s) => {
                    let resolved = s.resolve().await?;
                    result.push_str(&resolved);
                    secrets.push(resolved);
                }
            }
        }

        Ok((result, secrets))
    }

    /// Resolve a simple value (used by WithPolicies variant).
    async fn resolve_simple_with_secrets(
        value: &EnvValueSimple,
    ) -> crate::Result<(String, Vec<String>)> {
        match value {
            EnvValueSimple::String(s) => Ok((s.clone(), vec![])),
            EnvValueSimple::Int(i) => Ok((i.to_string(), vec![])),
            EnvValueSimple::Bool(b) => Ok((b.to_string(), vec![])),
            EnvValueSimple::Secret(s) => {
                let resolved = s.resolve().await?;
                Ok((resolved.clone(), vec![resolved]))
            }
            EnvValueSimple::Interpolated(parts) => Self::resolve_parts_with_secrets(parts).await,
        }
    }

    /// Collect all secrets from this value, returning them with their part index.
    ///
    /// The part index is used to match resolved values back to their position
    /// during reassembly. For non-interpolated secrets, index 0 is used.
    pub(super) fn collect_secrets(&self) -> Vec<(usize, &crate::secrets::Secret)> {
        match self {
            EnvValue::Secret(s) => vec![(0, s)],
            EnvValue::Interpolated(parts) => Self::collect_secrets_from_parts(parts),
            EnvValue::WithPolicies(var) => match &var.value {
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
    pub(super) fn reassemble_with_resolved(
        &self,
        resolved_secrets: &HashMap<usize, String>,
    ) -> (String, Vec<String>) {
        match self {
            EnvValue::String(s) => (s.clone(), vec![]),
            EnvValue::Int(i) => (i.to_string(), vec![]),
            EnvValue::Bool(b) => (b.to_string(), vec![]),
            EnvValue::Secret(_) => {
                let val = resolved_secrets.get(&0).cloned().unwrap_or_default();
                (val.clone(), vec![val])
            }
            EnvValue::Interpolated(parts) => Self::reassemble_parts(parts, resolved_secrets),
            EnvValue::WithPolicies(var) => match &var.value {
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
