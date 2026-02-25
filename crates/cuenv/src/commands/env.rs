//! Environment variable management commands.
//!
//! Provides commands for listing and printing environment variables
//! from CUE configurations.

use crate::commands::{CommandExecutor, relative_path_from_root};
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;
use tracing::instrument;

/// Load the module and get the Base configuration at the specified path.
///
/// Uses the executor's cached module evaluation (single CUE eval per process).
fn load_base_config(path: &str, executor: &CommandExecutor) -> Result<Base> {
    let target_path = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    tracing::debug!("Using cached module evaluation from executor");
    let module = executor.get_module(&target_path)?;
    let relative_path = relative_path_from_root(&module.root, &target_path);

    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    instance.deserialize()
}

/// List available environments in the CUE configuration.
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or the format is unsupported.
#[instrument(name = "env_list", skip(executor))]
pub async fn execute_env_list(
    path: &str,
    format: &str,
    executor: &CommandExecutor,
) -> Result<String> {
    tracing::info!("Starting env list command");

    // Load Base configuration using module-wide evaluation
    tracing::debug!("Loading CUE config at path '{}'", path);
    let manifest: Base = load_base_config(path, executor)?;

    let environments: Vec<String> = manifest
        .env
        .and_then(|env| env.environment)
        .map(|envs| {
            let mut keys: Vec<String> = envs.keys().cloned().collect();
            keys.sort();
            keys
        })
        .unwrap_or_default();

    // Format and return the output
    let output = match format {
        "json" => serde_json::to_string_pretty(&environments)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to format JSON: {e}")))?,
        "simple" => environments.join("\n"),
        other => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported format: '{other}'. Supported formats are 'json' and 'simple'."
            )));
        }
    };

    tracing::info!("Env list command completed successfully");
    Ok(output)
}

/// Print environment variables from the CUE configuration.
///
/// Resolves secrets and applies environment-specific overrides if specified.
///
/// # Errors
///
/// Returns an error if CUE evaluation fails, secrets cannot be resolved,
/// or the format is unsupported.
#[instrument(name = "env_print", skip(executor))]
pub async fn execute_env_print(
    path: &str,
    format: &str,
    environment: Option<&str>,
    executor: &CommandExecutor,
) -> Result<String> {
    tracing::info!("Starting env print command");

    // Load Base configuration using module-wide evaluation
    tracing::debug!("Loading CUE config at path '{}'", path);
    let manifest: Base = load_base_config(path, executor)?;

    // Extract the env field
    let env = manifest.env.ok_or_else(|| {
        cuenv_core::Error::configuration("No 'env' field found in CUE package".to_string())
    })?;

    // Get environment variables, applying environment-specific overrides if specified
    let env_vars = if let Some(env_name) = environment {
        tracing::debug!("Applying environment-specific overrides for '{}'", env_name);
        env.for_environment(env_name)
    } else {
        env.base.clone()
    };

    // Resolve all environment variables including secrets (parallel resolution)
    let (resolved_vars, secrets) =
        cuenv_core::environment::Environment::resolve_all_with_secrets(&env_vars).await?;

    // Register resolved secrets for global redaction
    cuenv_events::register_secrets(secrets.into_iter());

    // Format and return the output
    let output = match format {
        "json" => serde_json::to_string_pretty(&resolved_vars)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to format JSON: {e}")))?,
        "env" | "simple" => format_as_env_vars(&resolved_vars),
        other => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported format: '{other}'. Supported formats are 'json', 'env', and 'simple'."
            )));
        }
    };

    tracing::info!("Env print command completed successfully");
    Ok(output)
}

/// Format environment variables as shell-style KEY=VALUE pairs.
#[must_use]
fn format_as_env_vars(env_map: &std::collections::HashMap<String, String>) -> String {
    let mut lines = Vec::new();

    // Sort keys for consistent output
    let mut keys: Vec<&String> = env_map.keys().collect();
    keys.sort();

    for key in keys {
        let value = &env_map[key];
        lines.push(format!("{key}={value}"));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_format_as_env_vars_basic() {
        let mut env_map = HashMap::new();
        env_map.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost/mydb".to_string(),
        );
        env_map.insert("DEBUG".to_string(), "true".to_string());
        env_map.insert("PORT".to_string(), "3000".to_string());

        let result = format_as_env_vars(&env_map);
        let lines: Vec<&str> = result.split('\n').collect();

        // Should be sorted alphabetically
        assert_eq!(lines.len(), 3);
        assert!(lines.contains(&"DATABASE_URL=postgres://localhost/mydb"));
        assert!(lines.contains(&"DEBUG=true"));
        assert!(lines.contains(&"PORT=3000"));
    }

    #[test]
    fn test_format_as_env_vars_empty() {
        let env_map: HashMap<String, String> = HashMap::new();
        let result = format_as_env_vars(&env_map);
        assert_eq!(result, "");
    }
}
