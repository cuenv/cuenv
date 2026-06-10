//! Environment management for cuenv
//!
//! This module handles environment variables from CUE configurations,
//! including extraction, propagation, and environment-specific overrides.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;

mod values;

pub use values::{Env, EnvPart, EnvValue, EnvValueSimple, EnvVarWithPolicies, Policy};

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

    const HERMETIC_TEMP_VARS: &'static [&'static str] = &["TMPDIR", "TMP", "TEMP"];

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
            if let Ok(value) = env::var(var)
                && Self::should_preserve_system_var(var, &value)
            {
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

    fn should_preserve_system_var(var: &str, value: &str) -> bool {
        !Self::HERMETIC_TEMP_VARS.contains(&var) || Path::new(value).is_dir()
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

    /// Resolve all environment variables, returning resolved values and secret values.
    ///
    /// No policy filtering is applied - all variables are resolved.
    /// Secrets are resolved in parallel using a shared `SecretRegistry` and
    /// `tokio::task::JoinSet` for concurrent I/O.
    pub async fn resolve_all_with_secrets(
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        let all: Vec<_> = env_vars.iter().collect();
        Self::resolve_filtered_with_secrets(&all).await
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
    ///
    /// Secrets are resolved in parallel using a shared `SecretRegistry` and
    /// `tokio::task::JoinSet` for concurrent I/O.
    pub async fn resolve_for_task_with_secrets(
        task_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        tracing::debug!(
            task = task_name,
            env_count = env_vars.len(),
            "resolve_for_task_with_secrets"
        );

        let accessible: Vec<_> = env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_task(task_name))
            .collect();

        Self::resolve_filtered_with_secrets(&accessible).await
    }

    /// Build and resolve environment for a service, also returning secret values.
    ///
    /// Services use the same 3-phase secret resolution pipeline as tasks
    /// (collect, resolve in parallel via `SecretRegistry` + `JoinSet`,
    /// reassemble). Access policies are checked against the service name
    /// just as they are checked against task names for tasks.
    ///
    /// Returns `(resolved_env_vars, secret_values)` where `secret_values`
    /// contains the resolved values of any secrets for log redaction.
    pub async fn resolve_for_service_with_secrets(
        service_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        tracing::debug!(
            service = service_name,
            env_count = env_vars.len(),
            "resolve_for_service_with_secrets"
        );

        let accessible: Vec<_> = env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_task(service_name))
            .collect();

        Self::resolve_filtered_with_secrets(&accessible).await
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
    ///
    /// Secrets are resolved in parallel using a shared `SecretRegistry` and
    /// `tokio::task::JoinSet` for concurrent I/O.
    pub async fn resolve_for_exec_with_secrets(
        command: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        let accessible: Vec<_> = env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_exec(command))
            .collect();

        Self::resolve_filtered_with_secrets(&accessible).await
    }

    /// Resolve a pre-filtered set of environment variables, resolving all secrets
    /// in parallel via a shared `SecretRegistry` and `tokio::task::JoinSet`.
    ///
    /// Phase 1 (Collect): Non-secret vars go straight to output. Secrets are
    /// collected with their env key and part index for later reassembly.
    ///
    /// Phase 2 (Resolve): One `SecretRegistry` is created and shared via `Arc`.
    /// All secrets are spawned into a `JoinSet` for concurrent resolution.
    ///
    /// Phase 3 (Reassemble): Resolved values are grouped by env key and passed
    /// to `reassemble_with_resolved` to rebuild the final string values.
    async fn resolve_filtered_with_secrets(
        accessible: &[(&String, &EnvValue)],
    ) -> crate::Result<(HashMap<String, String>, Vec<String>)> {
        let mut resolved = HashMap::new();
        let mut all_secrets = Vec::new();

        // Phase 1: Separate non-secret vars (instant) from secret vars (need resolution)
        type SecretVarEntry<'a> = (
            &'a String,
            &'a EnvValue,
            Vec<(usize, crate::secrets::Secret)>,
        );
        let mut secret_vars: Vec<SecretVarEntry<'_>> = Vec::new();

        for (key, value) in accessible {
            let collected = value.collect_secrets();
            if collected.is_empty() {
                // No secrets - resolve immediately (just string conversion)
                resolved.insert((*key).clone(), value.to_string_value());
            } else {
                let owned_secrets: Vec<(usize, crate::secrets::Secret)> = collected
                    .into_iter()
                    .map(|(idx, s)| (idx, s.clone()))
                    .collect();
                secret_vars.push((key, value, owned_secrets));
            }
        }

        // If no secrets, return early
        if secret_vars.is_empty() {
            return Ok((resolved, all_secrets));
        }

        // Phase 2: Resolve all secrets in parallel with a shared registry
        let registry = Arc::new(crate::secrets::create_default_registry()?);
        let mut join_set = tokio::task::JoinSet::new();

        for (key, _, secrets) in &secret_vars {
            for (part_idx, secret) in secrets {
                let key = (*key).clone();
                let part_idx = *part_idx;
                let secret = secret.clone();
                let registry = Arc::clone(&registry);
                join_set.spawn(async move {
                    let value = secret.resolve_with_registry(&registry).await?;
                    Ok::<_, crate::Error>((key, part_idx, value))
                });
            }
        }

        // Collect all resolved values, grouped by env key
        let mut resolved_by_key: HashMap<String, HashMap<usize, String>> = HashMap::new();
        while let Some(result) = join_set.join_next().await {
            let (key, part_idx, value) = result.map_err(|e| {
                crate::Error::configuration(format!("Secret resolution task panicked: {e}"))
            })??;
            resolved_by_key
                .entry(key)
                .or_default()
                .insert(part_idx, value);
        }

        // Phase 3: Reassemble final values
        for (key, value, _) in &secret_vars {
            let key_resolved = resolved_by_key.get(*key).cloned().unwrap_or_default();
            let (final_value, mut value_secrets) = value.reassemble_with_resolved(&key_resolved);
            if !value_secrets.is_empty() {
                tracing::debug!(
                    key = *key,
                    secret_count = value_secrets.len(),
                    "resolved secrets"
                );
            }
            all_secrets.append(&mut value_secrets);
            resolved.insert((*key).clone(), final_value);
        }

        Ok((resolved, all_secrets))
    }
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
