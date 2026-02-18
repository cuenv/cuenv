//! Environment management for cuenv
//!
//! This module handles environment variables from CUE configurations,
//! including extraction, propagation, and environment-specific overrides.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

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
}

/// Runtime environment variables for task execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Environment {
    /// Map of environment variable names to values
    #[serde(flatten)]
    pub vars: HashMap<String, String>,
}

impl Environment {
    /// Create a new empty environment
    pub fn new() -> Self {
        Self::default()
    }

    /// Create environment from a map
    pub fn from_map(vars: HashMap<String, String>) -> Self {
        Self { vars }
    }

    /// Get an environment variable value
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Set an environment variable
    pub fn set(&mut self, key: String, value: String) {
        self.vars.insert(key, value);
    }

    /// Check if an environment variable exists
    pub fn contains(&self, key: &str) -> bool {
        self.vars.contains_key(key)
    }

    /// Get all environment variables as a vector of key=value strings
    pub fn to_env_vec(&self) -> Vec<String> {
        self.vars
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect()
    }

    /// Merge with system environment variables
    /// CUE environment variables take precedence
    pub fn merge_with_system(&self) -> HashMap<String, String> {
        let mut merged: HashMap<String, String> = env::vars().collect();

        // Override with CUE environment variables
        for (key, value) in &self.vars {
            merged.insert(key.clone(), value.clone());
        }

        merged
    }

    /// Essential system variables to preserve in hermetic mode.
    /// These are required for basic process operation but don't pollute PATH.
    const HERMETIC_ALLOWED_VARS: &'static [&'static str] = &[
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TERM",
        "COLORTERM",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_MESSAGES",
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
    ];

    /// Merge with only essential system environment variables (hermetic mode).
    ///
    /// Unlike `merge_with_system()`, this excludes PATH and other potentially
    /// polluting variables. PATH should come from cuenv tools activation.
    ///
    /// Included variables:
    /// - User identity: HOME, USER, LOGNAME, SHELL
    /// - Terminal: TERM, COLORTERM
    /// - Locale: LANG, LC_* variables
    /// - Temp directories: TMPDIR, TMP, TEMP
    /// - XDG directories: XDG_RUNTIME_DIR, XDG_CONFIG_HOME, etc.
    pub fn merge_with_system_hermetic(&self) -> HashMap<String, String> {
        let mut merged: HashMap<String, String> = HashMap::new();

        // Only include allowed system variables
        for var in Self::HERMETIC_ALLOWED_VARS {
            if let Ok(value) = env::var(var) {
                merged.insert((*var).to_string(), value);
            }
        }

        // Also include any LC_* variables (locale settings)
        for (key, value) in env::vars() {
            if key.starts_with("LC_") {
                merged.insert(key, value);
            }
        }

        // Override with CUE environment variables (including cuenv-constructed PATH)
        for (key, value) in &self.vars {
            merged.insert(key.clone(), value.clone());
        }

        merged
    }

    /// Convert to a vector of key=value strings including system environment
    pub fn to_full_env_vec(&self) -> Vec<String> {
        self.merge_with_system()
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect()
    }

    /// Get the number of environment variables
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    /// Check if the environment is empty
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// Resolve a command to its full path using this environment's PATH.
    /// This is necessary because when spawning a process, the OS looks up
    /// the executable in the current process's PATH, not the environment
    /// that will be set on the child process.
    ///
    /// Returns the full path if found, or the original command if not found
    /// (letting the spawn fail with a proper error).
    pub fn resolve_command(&self, command: &str) -> String {
        // If command is already an absolute path, use it directly
        if command.starts_with('/') {
            tracing::debug!(command = %command, "Command is already absolute path");
            return command.to_string();
        }

        // Get the PATH from this environment, falling back to system PATH
        let path_value = self
            .vars
            .get("PATH")
            .cloned()
            .or_else(|| env::var("PATH").ok())
            .unwrap_or_default();

        tracing::debug!(
            command = %command,
            env_has_path = self.vars.contains_key("PATH"),
            path_len = path_value.len(),
            "Resolving command in PATH"
        );

        // Search for the command in each PATH directory
        for dir in path_value.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = std::path::Path::new(dir).join(command);
            if candidate.is_file() {
                // Check if it's executable (on Unix)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = std::fs::metadata(&candidate) {
                        let permissions = metadata.permissions();
                        if permissions.mode() & 0o111 != 0 {
                            tracing::debug!(
                                command = %command,
                                resolved = %candidate.display(),
                                "Command resolved to path"
                            );
                            return candidate.to_string_lossy().to_string();
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    tracing::debug!(
                        command = %command,
                        resolved = %candidate.display(),
                        "Command resolved to path"
                    );
                    return candidate.to_string_lossy().to_string();
                }
            }
        }

        // Command not found in environment PATH - try system PATH as fallback
        // This is necessary when the environment has tool paths but not system paths,
        // since we still need to find system commands like echo, bash, etc.
        if self.vars.contains_key("PATH")
            && let Ok(system_path) = env::var("PATH")
        {
            tracing::debug!(
                command = %command,
                "Command not found in env PATH, trying system PATH"
            );
            for dir in system_path.split(':') {
                if dir.is_empty() {
                    continue;
                }
                let candidate = std::path::Path::new(dir).join(command);
                if candidate.is_file() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(metadata) = std::fs::metadata(&candidate) {
                            let permissions = metadata.permissions();
                            if permissions.mode() & 0o111 != 0 {
                                tracing::debug!(
                                    command = %command,
                                    resolved = %candidate.display(),
                                    "Command resolved from system PATH"
                                );
                                return candidate.to_string_lossy().to_string();
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        tracing::debug!(
                            command = %command,
                            resolved = %candidate.display(),
                            "Command resolved from system PATH"
                        );
                        return candidate.to_string_lossy().to_string();
                    }
                }
            }
        }

        // Command not found in any PATH, return original (spawn will fail with proper error)
        tracing::warn!(
            command = %command,
            env_path_set = self.vars.contains_key("PATH"),
            "Command not found in PATH, returning original"
        );
        command.to_string()
    }

    /// Iterate over environment variables
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter()
    }

    /// Build environment for a task, filtering based on policies
    pub fn build_for_task(
        task_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> HashMap<String, String> {
        env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_task(task_name))
            .map(|(key, value)| (key.clone(), value.to_string_value()))
            .collect()
    }

    /// Build and resolve environment for a task, filtering based on policies
    ///
    /// Resolves all environment variables including secrets via the
    /// trait-based resolver system.
    pub async fn resolve_for_task(
        task_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<HashMap<String, String>> {
        let (resolved, _secrets) = Self::resolve_for_task_with_secrets(task_name, env_vars).await?;
        Ok(resolved)
    }

    /// Build and resolve environment for a task, also returning secret values
    ///
    /// Returns `(resolved_env_vars, secret_values)` where `secret_values` contains
    /// the resolved values of any secrets, for use in output redaction.
    /// For interpolated values, only the actual secret parts are collected for redaction,
    /// not the full interpolated string.
    pub async fn resolve_for_task_with_secrets(
        task_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        let mut resolved = HashMap::new();
        let mut secrets = Vec::new();

        tracing::debug!(
            task = task_name,
            env_count = env_vars.len(),
            "resolve_for_task_with_secrets"
        );
        for (key, value) in env_vars {
            tracing::debug!(
                key = key,
                is_secret = value.is_secret(),
                accessible = value.is_accessible_by_task(task_name),
                "checking env var"
            );
            if value.is_accessible_by_task(task_name) {
                let (resolved_value, mut value_secrets) = value.resolve_with_secrets().await?;
                if !value_secrets.is_empty() {
                    tracing::debug!(
                        key = key,
                        secret_count = value_secrets.len(),
                        "resolved secrets"
                    );
                }
                secrets.append(&mut value_secrets);
                resolved.insert(key.clone(), resolved_value);
            }
        }
        Ok((resolved, secrets))
    }

    /// Build environment for exec command, filtering based on policies
    pub fn build_for_exec(
        command: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> HashMap<String, String> {
        env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_exec(command))
            .map(|(key, value)| (key.clone(), value.to_string_value()))
            .collect()
    }

    /// Build and resolve environment for exec command, filtering based on policies
    ///
    /// Resolves all environment variables including secrets via the
    /// trait-based resolver system.
    pub async fn resolve_for_exec(
        command: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<HashMap<String, String>> {
        let (resolved, _secrets) = Self::resolve_for_exec_with_secrets(command, env_vars).await?;
        Ok(resolved)
    }

    /// Build and resolve environment for exec command, also returning secret values
    ///
    /// Returns `(resolved_env_vars, secret_values)` where `secret_values` contains
    /// the resolved values of any secrets, for use in output redaction.
    /// For interpolated values, only the actual secret parts are collected for redaction,
    /// not the full interpolated string.
    pub async fn resolve_for_exec_with_secrets(
        command: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        let mut resolved = HashMap::new();
        let mut secrets = Vec::new();

        for (key, value) in env_vars {
            if value.is_accessible_by_exec(command) {
                let (resolved_value, mut value_secrets) = value.resolve_with_secrets().await?;
                secrets.append(&mut value_secrets);
                resolved.insert(key.clone(), resolved_value);
            }
        }
        Ok((resolved, secrets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_basics() {
        let mut env = Environment::new();
        assert!(env.is_empty());

        env.set("FOO".to_string(), "bar".to_string());
        assert_eq!(env.len(), 1);
        assert!(env.contains("FOO"));
        assert_eq!(env.get("FOO"), Some("bar"));
        assert!(!env.contains("BAR"));
    }

    #[test]
    fn test_environment_from_map() {
        let mut vars = HashMap::new();
        vars.insert("KEY1".to_string(), "value1".to_string());
        vars.insert("KEY2".to_string(), "value2".to_string());

        let env = Environment::from_map(vars);
        assert_eq!(env.len(), 2);
        assert_eq!(env.get("KEY1"), Some("value1"));
        assert_eq!(env.get("KEY2"), Some("value2"));
    }

    #[test]
    fn test_environment_to_vec() {
        let mut env = Environment::new();
        env.set("VAR1".to_string(), "val1".to_string());
        env.set("VAR2".to_string(), "val2".to_string());

        let vec = env.to_env_vec();
        assert_eq!(vec.len(), 2);
        assert!(vec.contains(&"VAR1=val1".to_string()));
        assert!(vec.contains(&"VAR2=val2".to_string()));
    }

    #[test]
    fn test_environment_merge_with_system() {
        let mut env = Environment::new();
        env.set("PATH".to_string(), "/custom/path".to_string());
        env.set("CUSTOM_VAR".to_string(), "custom_value".to_string());

        let merged = env.merge_with_system();

        // Custom variables should be present
        assert_eq!(merged.get("PATH"), Some(&"/custom/path".to_string()));
        assert_eq!(merged.get("CUSTOM_VAR"), Some(&"custom_value".to_string()));

        // System variables should still be present (like HOME, USER, etc.)
        // We can't test specific values but we can check that merging happened
        assert!(merged.len() >= 2);
    }

    #[test]
    fn test_environment_iteration() {
        let mut env = Environment::new();
        env.set("A".to_string(), "1".to_string());
        env.set("B".to_string(), "2".to_string());

        let mut count = 0;
        for (key, value) in env.iter() {
            assert!(key == "A" || key == "B");
            assert!(value == "1" || value == "2");
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn test_env_value_types() {
        let str_val = EnvValue::String("test".to_string());
        let int_val = EnvValue::Int(42);
        let bool_val = EnvValue::Bool(true);

        assert_eq!(str_val, EnvValue::String("test".to_string()));
        assert_eq!(int_val, EnvValue::Int(42));
        assert_eq!(bool_val, EnvValue::Bool(true));
    }

    #[test]
    fn test_policy_task_access() {
        // Simple value - always accessible
        let simple_var = EnvValue::String("simple".to_string());
        assert!(simple_var.is_accessible_by_task("any_task"));

        // Variable with no policies - accessible
        let no_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("value".to_string()),
            policies: None,
        });
        assert!(no_policy_var.is_accessible_by_task("any_task"));

        // Variable with empty policies - accessible
        let empty_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("value".to_string()),
            policies: Some(vec![]),
        });
        assert!(empty_policy_var.is_accessible_by_task("any_task"));

        // Variable with task restrictions
        let restricted_var = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("secret".to_string()),
            policies: Some(vec![Policy {
                allow_tasks: Some(vec!["deploy".to_string(), "release".to_string()]),
                allow_exec: None,
            }]),
        });
        assert!(restricted_var.is_accessible_by_task("deploy"));
        assert!(restricted_var.is_accessible_by_task("release"));
        assert!(!restricted_var.is_accessible_by_task("test"));
        assert!(!restricted_var.is_accessible_by_task("build"));
    }

    #[test]
    fn test_policy_exec_access() {
        // Simple value - always accessible
        let simple_var = EnvValue::String("simple".to_string());
        assert!(simple_var.is_accessible_by_exec("bash"));

        // Variable with exec restrictions
        let restricted_var = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("secret".to_string()),
            policies: Some(vec![Policy {
                allow_tasks: None,
                allow_exec: Some(vec!["kubectl".to_string(), "terraform".to_string()]),
            }]),
        });
        assert!(restricted_var.is_accessible_by_exec("kubectl"));
        assert!(restricted_var.is_accessible_by_exec("terraform"));
        assert!(!restricted_var.is_accessible_by_exec("bash"));
        assert!(!restricted_var.is_accessible_by_exec("sh"));
    }

    #[test]
    fn test_multiple_policies() {
        // Variable with multiple policies - should allow if ANY policy allows
        let multi_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("value".to_string()),
            policies: Some(vec![
                Policy {
                    allow_tasks: Some(vec!["task1".to_string()]),
                    allow_exec: None,
                },
                Policy {
                    allow_tasks: Some(vec!["task2".to_string()]),
                    allow_exec: Some(vec!["kubectl".to_string()]),
                },
            ]),
        });

        // Task access - either policy allows
        assert!(multi_policy_var.is_accessible_by_task("task1"));
        assert!(multi_policy_var.is_accessible_by_task("task2"));
        assert!(!multi_policy_var.is_accessible_by_task("task3"));

        // Exec access - only second policy has exec rules
        assert!(multi_policy_var.is_accessible_by_exec("kubectl"));
        assert!(!multi_policy_var.is_accessible_by_exec("bash"));
    }

    #[test]
    fn test_to_string_value() {
        assert_eq!(
            EnvValue::String("test".to_string()).to_string_value(),
            "test"
        );
        assert_eq!(EnvValue::Int(42).to_string_value(), "42");
        assert_eq!(EnvValue::Bool(true).to_string_value(), "true");
        assert_eq!(EnvValue::Bool(false).to_string_value(), "false");

        let with_policies = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("policy_value".to_string()),
            policies: Some(vec![]),
        });
        assert_eq!(with_policies.to_string_value(), "policy_value");
    }

    #[test]
    fn test_build_for_task() {
        let mut env_vars = HashMap::new();

        // Unrestricted variable
        env_vars.insert(
            "PUBLIC".to_string(),
            EnvValue::String("public_value".to_string()),
        );

        // Restricted variable
        env_vars.insert(
            "SECRET".to_string(),
            EnvValue::WithPolicies(EnvVarWithPolicies {
                value: EnvValueSimple::String("secret_value".to_string()),
                policies: Some(vec![Policy {
                    allow_tasks: Some(vec!["deploy".to_string()]),
                    allow_exec: None,
                }]),
            }),
        );

        // Build for deploy task - should get both
        let deploy_env = Environment::build_for_task("deploy", &env_vars);
        assert_eq!(deploy_env.len(), 2);
        assert_eq!(deploy_env.get("PUBLIC"), Some(&"public_value".to_string()));
        assert_eq!(deploy_env.get("SECRET"), Some(&"secret_value".to_string()));

        // Build for test task - should only get public
        let test_env = Environment::build_for_task("test", &env_vars);
        assert_eq!(test_env.len(), 1);
        assert_eq!(test_env.get("PUBLIC"), Some(&"public_value".to_string()));
        assert_eq!(test_env.get("SECRET"), None);
    }

    #[test]
    fn test_build_for_exec() {
        let mut env_vars = HashMap::new();

        // Unrestricted variable
        env_vars.insert(
            "PUBLIC".to_string(),
            EnvValue::String("public_value".to_string()),
        );

        // Restricted variable
        env_vars.insert(
            "SECRET".to_string(),
            EnvValue::WithPolicies(EnvVarWithPolicies {
                value: EnvValueSimple::String("secret_value".to_string()),
                policies: Some(vec![Policy {
                    allow_tasks: None,
                    allow_exec: Some(vec!["kubectl".to_string()]),
                }]),
            }),
        );

        // Build for kubectl - should get both
        let kubectl_env = Environment::build_for_exec("kubectl", &env_vars);
        assert_eq!(kubectl_env.len(), 2);
        assert_eq!(kubectl_env.get("PUBLIC"), Some(&"public_value".to_string()));
        assert_eq!(kubectl_env.get("SECRET"), Some(&"secret_value".to_string()));

        // Build for bash - should only get public
        let bash_env = Environment::build_for_exec("bash", &env_vars);
        assert_eq!(bash_env.len(), 1);
        assert_eq!(bash_env.get("PUBLIC"), Some(&"public_value".to_string()));
        assert_eq!(bash_env.get("SECRET"), None);
    }

    #[test]
    fn test_env_for_environment() {
        let mut base = HashMap::new();
        base.insert("BASE_VAR".to_string(), EnvValue::String("base".to_string()));
        base.insert(
            "OVERRIDE_ME".to_string(),
            EnvValue::String("original".to_string()),
        );

        let mut dev_env = HashMap::new();
        dev_env.insert(
            "OVERRIDE_ME".to_string(),
            EnvValue::String("dev".to_string()),
        );
        dev_env.insert(
            "DEV_VAR".to_string(),
            EnvValue::String("development".to_string()),
        );

        let mut environments = HashMap::new();
        environments.insert("development".to_string(), dev_env);

        let env = Env {
            base,
            environment: Some(environments),
        };

        let dev_vars = env.for_environment("development");
        assert_eq!(
            dev_vars.get("BASE_VAR"),
            Some(&EnvValue::String("base".to_string()))
        );
        assert_eq!(
            dev_vars.get("OVERRIDE_ME"),
            Some(&EnvValue::String("dev".to_string()))
        );
        assert_eq!(
            dev_vars.get("DEV_VAR"),
            Some(&EnvValue::String("development".to_string()))
        );
    }

    #[test]
    fn test_env_deserialize_with_environment_overrides() {
        let json = r#"{
            "API_URL": "https://api.example.com",
            "environment": {
                "production": {
                    "API_URL": "https://api.prod.example.com",
                    "AUTH_SECRET": {"resolver": "exec", "command": "echo", "args": ["token"]}
                }
            }
        }"#;

        let env: Env = serde_json::from_str(json).expect("valid env payload");

        assert!(env.base.contains_key("API_URL"));
        assert!(!env.base.contains_key("environment"));

        let environments = env.environment.expect("environment overrides should deserialize");
        let production = environments
            .get("production")
            .expect("production overrides should exist");
        assert!(production.contains_key("AUTH_SECRET"));
    }

    #[tokio::test]
    async fn test_resolve_plain_string() {
        let env_val = EnvValue::String("plain_value".to_string());
        let resolved = env_val.resolve().await.unwrap();
        assert_eq!(resolved, "plain_value");
    }

    #[tokio::test]
    async fn test_resolve_int() {
        let env_val = EnvValue::Int(42);
        let resolved = env_val.resolve().await.unwrap();
        assert_eq!(resolved, "42");
    }

    #[tokio::test]
    async fn test_resolve_bool() {
        let env_val = EnvValue::Bool(true);
        let resolved = env_val.resolve().await.unwrap();
        assert_eq!(resolved, "true");
    }

    #[tokio::test]
    async fn test_resolve_with_policies_plain_string() {
        let env_val = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("policy_value".to_string()),
            policies: None,
        });
        let resolved = env_val.resolve().await.unwrap();
        assert_eq!(resolved, "policy_value");
    }

    // ==========================================================================
    // Interpolation tests
    // ==========================================================================

    #[test]
    fn test_env_part_literal() {
        let part = EnvPart::Literal("hello".to_string());
        assert!(!part.is_secret());
    }

    #[test]
    fn test_env_part_secret() {
        let secret = crate::secrets::Secret::new("echo".to_string(), vec!["test".to_string()]);
        let part = EnvPart::Secret(secret);
        assert!(part.is_secret());
    }

    #[test]
    fn test_env_part_deserialization_literal() {
        let json = r#""hello""#;
        let part: EnvPart = serde_json::from_str(json).unwrap();
        assert!(matches!(part, EnvPart::Literal(ref s) if s == "hello"));
        assert!(!part.is_secret());
    }

    #[test]
    fn test_env_part_deserialization_secret() {
        let json = r#"{"resolver": "exec", "command": "echo", "args": ["test"]}"#;
        let part: EnvPart = serde_json::from_str(json).unwrap();
        assert!(part.is_secret());
    }

    #[test]
    fn test_env_value_interpolated_deserialization() {
        let json =
            r#"["prefix-", {"resolver": "exec", "command": "gh", "args": ["auth", "token"]}]"#;
        let value: EnvValue = serde_json::from_str(json).unwrap();
        assert!(matches!(value, EnvValue::Interpolated(_)));
        assert!(value.is_secret());
    }

    #[test]
    fn test_interpolated_is_secret_with_no_secrets() {
        let parts = vec![
            EnvPart::Literal("hello".to_string()),
            EnvPart::Literal("world".to_string()),
        ];
        let value = EnvValue::Interpolated(parts);
        assert!(!value.is_secret());
    }

    #[test]
    fn test_interpolated_is_secret_with_secret() {
        let secret = crate::secrets::Secret::new("echo".to_string(), vec![]);
        let parts = vec![
            EnvPart::Literal("prefix".to_string()),
            EnvPart::Secret(secret),
        ];
        let value = EnvValue::Interpolated(parts);
        assert!(value.is_secret());
    }

    #[test]
    fn test_interpolated_to_string_value_redacts_secrets() {
        let secret = crate::secrets::Secret::new(
            "gh".to_string(),
            vec!["auth".to_string(), "token".to_string()],
        );
        let parts = vec![
            EnvPart::Literal("access-tokens = github.com=".to_string()),
            EnvPart::Secret(secret),
        ];
        let value = EnvValue::Interpolated(parts);
        assert_eq!(value.to_string_value(), "access-tokens = github.com=*_*");
    }

    #[test]
    fn test_interpolated_to_string_value_no_secrets() {
        let parts = vec![
            EnvPart::Literal("hello".to_string()),
            EnvPart::Literal("-".to_string()),
            EnvPart::Literal("world".to_string()),
        ];
        let value = EnvValue::Interpolated(parts);
        assert_eq!(value.to_string_value(), "hello-world");
    }

    #[tokio::test]
    async fn test_resolve_with_secrets_collects_only_secret_parts() {
        // Test that only actual secret values are collected for redaction,
        // not the full interpolated string (when there are no secrets)
        let parts = vec![
            EnvPart::Literal("hello-".to_string()),
            EnvPart::Literal("world".to_string()),
        ];
        let value = EnvValue::Interpolated(parts);
        let (resolved, secrets) = value.resolve_with_secrets().await.unwrap();
        assert_eq!(resolved, "hello-world");
        assert!(secrets.is_empty()); // No secrets to redact
    }

    #[tokio::test]
    async fn test_resolve_interpolated_concatenates_parts() {
        let parts = vec![
            EnvPart::Literal("a".to_string()),
            EnvPart::Literal("b".to_string()),
            EnvPart::Literal("c".to_string()),
        ];
        let value = EnvValue::Interpolated(parts);
        let resolved = value.resolve().await.unwrap();
        assert_eq!(resolved, "abc");
    }

    #[test]
    fn test_interpolated_with_policies_is_secret() {
        let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
        let parts = vec![
            EnvPart::Literal("prefix".to_string()),
            EnvPart::Secret(secret),
        ];

        let value = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::Interpolated(parts),
            policies: Some(vec![Policy {
                allow_tasks: Some(vec!["deploy".to_string()]),
                allow_exec: None,
            }]),
        });

        assert!(value.is_secret());
    }

    #[test]
    fn test_interpolated_with_policies_to_string_value() {
        let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
        let parts = vec![
            EnvPart::Literal("before-".to_string()),
            EnvPart::Secret(secret),
            EnvPart::Literal("-after".to_string()),
        ];

        let value = EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::Interpolated(parts),
            policies: None,
        });

        assert_eq!(value.to_string_value(), "before-*_*-after");
    }

    #[test]
    fn test_interpolated_accessible_by_task() {
        let parts = vec![EnvPart::Literal("value".to_string())];
        let value = EnvValue::Interpolated(parts);
        // Interpolated values without policies are always accessible
        assert!(value.is_accessible_by_task("any_task"));
    }

    #[test]
    fn test_extract_static_env_vars_skips_interpolated_secrets() {
        // Simulate the extract_static_env_vars logic
        let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
        let parts = vec![
            EnvPart::Literal("prefix".to_string()),
            EnvPart::Secret(secret),
        ];

        let mut base = HashMap::new();
        base.insert("PLAIN".to_string(), EnvValue::String("value".to_string()));
        base.insert(
            "INTERPOLATED_SECRET".to_string(),
            EnvValue::Interpolated(parts),
        );
        base.insert(
            "INTERPOLATED_PLAIN".to_string(),
            EnvValue::Interpolated(vec![
                EnvPart::Literal("a".to_string()),
                EnvPart::Literal("b".to_string()),
            ]),
        );

        // Filter out secrets (simulating extract_static_env_vars logic)
        let vars: HashMap<_, _> = base
            .iter()
            .filter(|(_, v)| !v.is_secret())
            .map(|(k, v)| (k.clone(), v.to_string_value()))
            .collect();

        assert!(vars.contains_key("PLAIN"));
        assert!(!vars.contains_key("INTERPOLATED_SECRET"));
        assert!(vars.contains_key("INTERPOLATED_PLAIN"));
        assert_eq!(vars.get("INTERPOLATED_PLAIN"), Some(&"ab".to_string()));
    }

    #[test]
    fn test_env_value_simple_interpolated_deserialization() {
        // Test that EnvValueSimple can deserialize interpolated arrays
        let json = r#"["a", "b", "c"]"#;
        let value: EnvValueSimple = serde_json::from_str(json).unwrap();
        assert!(matches!(value, EnvValueSimple::Interpolated(_)));
    }

    #[test]
    fn test_env_value_with_policies_interpolated_deserialization() {
        let json = r#"{
            "value": ["prefix-", {"resolver": "exec", "command": "gh", "args": ["auth", "token"]}],
            "policies": [{"allowTasks": ["deploy"]}]
        }"#;
        let value: EnvValue = serde_json::from_str(json).unwrap();
        assert!(matches!(value, EnvValue::WithPolicies(_)));
        assert!(value.is_secret());
    }

    #[test]
    fn test_interpolated_empty_array() {
        let parts = vec![];
        let value = EnvValue::Interpolated(parts);
        assert_eq!(value.to_string_value(), "");
        assert!(!value.is_secret());
    }

    #[tokio::test]
    async fn test_resolve_interpolated_with_actual_secret() {
        let secret =
            crate::secrets::Secret::new("echo".to_string(), vec!["secret_value".to_string()]);
        let parts = vec![
            EnvPart::Literal("prefix-".to_string()),
            EnvPart::Secret(secret),
            EnvPart::Literal("-suffix".to_string()),
        ];
        let value = EnvValue::Interpolated(parts);
        let (resolved, secrets) = value.resolve_with_secrets().await.unwrap();

        assert!(resolved.contains("prefix-"));
        assert!(resolved.contains("secret_value"));
        assert!(resolved.contains("-suffix"));
        assert_eq!(secrets.len(), 1);
    }
}
