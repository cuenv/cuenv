use crate::commands::env_file::find_cue_module_root;
use crate::commands::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Base;
use cuenv_core::{ModuleEvaluation, Result};
use std::path::Path;
use tracing::instrument;

/// Load the module and get the Base configuration at the specified path.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
fn load_base_config(path: &str, package: &str, executor: Option<&CommandExecutor>) -> Result<Base> {
    let target_path = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    // Use executor's cached module if available, otherwise fresh evaluation
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let relative_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&relative_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                relative_path.display()
            ))
        })?;

        return instance.deserialize();
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh single-instance evaluation (no executor)");

    // Find the CUE module root
    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    // Evaluate only the target directory (non-recursive) since env commands
    // only need the current project's configuration, not cross-project references
    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(options))
        .map_err(convert_engine_error)?;

    // Build ModuleEvaluation
    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    // Get the instance at the target path
    let relative_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    // Deserialize to Base schema
    instance.deserialize()
}

#[instrument(name = "env_list", skip(executor))]
pub async fn execute_env_list(
    path: &str,
    package: &str,
    format: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    tracing::info!("Starting env list command");

    // Load Base configuration using module-wide evaluation
    tracing::debug!(
        "Loading CUE config for package '{}' at path '{}'",
        package,
        path
    );
    let manifest: Base = load_base_config(path, package, executor)?;

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

#[instrument(name = "env_print", skip(executor))]
pub async fn execute_env_print(
    path: &str,
    package: &str,
    format: &str,
    environment: Option<&str>,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    tracing::info!("Starting env print command");

    // Load Base configuration using module-wide evaluation
    tracing::debug!(
        "Loading CUE config for package '{}' at path '{}'",
        package,
        path
    );
    let manifest: Base = load_base_config(path, package, executor)?;

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

    // Resolve all environment variables including secrets
    let (resolved_vars, secrets) = resolve_env_vars_with_secrets(&env_vars).await?;

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

/// Resolve all environment variables, returning resolved values and secret values separately
async fn resolve_env_vars_with_secrets(
    env_map: &std::collections::HashMap<String, cuenv_core::environment::EnvValue>,
) -> Result<(std::collections::HashMap<String, String>, Vec<String>)> {
    let mut resolved = std::collections::HashMap::new();
    let mut secrets = Vec::new();

    for (key, value) in env_map {
        let resolved_value = value.resolve().await?;
        if value.is_secret() {
            secrets.push(resolved_value.clone());
        }
        resolved.insert(key.clone(), resolved_value);
    }

    Ok((resolved, secrets))
}

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
