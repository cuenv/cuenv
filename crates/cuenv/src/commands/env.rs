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
    tracing::debug!("Using fresh module evaluation (no executor)");

    // Find the CUE module root
    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    // Evaluate the entire module (recursively to include all subdirectories)
    let options = ModuleEvalOptions {
        recursive: true,
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

    // Format and return the output
    let output = match format {
        "json" => serde_json::to_string_pretty(&env_vars)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to format JSON: {e}")))?,
        "env" | "simple" => format_as_env_vars_from_map(&env_vars),
        other => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported format: '{other}'. Supported formats are 'json', 'env', and 'simple'."
            )));
        }
    };

    tracing::info!("Env print command completed successfully");
    Ok(output)
}

fn format_as_env_vars_from_map(
    env_map: &std::collections::HashMap<String, cuenv_core::environment::EnvValue>,
) -> String {
    let mut lines = Vec::new();

    // Sort keys for consistent output
    let mut keys: Vec<&String> = env_map.keys().collect();
    keys.sort();

    for key in keys {
        let value = &env_map[key];
        // Use to_string_value for consistent handling
        let formatted_value = value.to_string_value();
        if formatted_value == "[SECRET]" {
            // Skip secrets in env output
            continue;
        }
        lines.push(format!("{key}={formatted_value}"));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::environment::EnvValue;
    use std::collections::HashMap;

    #[test]
    fn test_format_as_env_vars_basic() {
        let mut env_map = HashMap::new();
        env_map.insert(
            "DATABASE_URL".to_string(),
            EnvValue::String("postgres://localhost/mydb".to_string()),
        );
        env_map.insert("DEBUG".to_string(), EnvValue::Bool(true));
        env_map.insert("PORT".to_string(), EnvValue::Int(3000));

        let result = format_as_env_vars_from_map(&env_map);
        let lines: Vec<&str> = result.split('\n').collect();

        // Should be sorted alphabetically
        assert_eq!(lines.len(), 3);
        assert!(lines.contains(&"DATABASE_URL=postgres://localhost/mydb"));
        assert!(lines.contains(&"DEBUG=true"));
        assert!(lines.contains(&"PORT=3000"));
    }

    #[test]
    fn test_format_as_env_vars_empty() {
        let env_map: HashMap<String, EnvValue> = HashMap::new();
        let result = format_as_env_vars_from_map(&env_map);
        assert_eq!(result, "");
    }
}
