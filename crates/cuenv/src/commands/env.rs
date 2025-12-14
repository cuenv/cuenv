use cuengine::CueEvaluator;
use cuenv_core::Result;
use cuenv_core::manifest::Cuenv;
use std::path::Path;
use tracing::instrument;

#[instrument(name = "env_list")]
pub async fn execute_env_list(path: &str, package: &str, format: &str) -> Result<String> {
    tracing::info!("Starting env list command");

    // Create CUE evaluator
    let evaluator = CueEvaluator::builder()
        .build()
        .map_err(super::convert_engine_error)?;

    // Convert path string to Path
    let dir_path = Path::new(path);

    // Evaluate the CUE package
    tracing::debug!("Evaluating CUE package '{}' at path '{}'", package, path);
    let manifest: Cuenv = evaluator
        .evaluate_typed(dir_path, package)
        .map_err(super::convert_engine_error)?;

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

#[instrument(name = "env_print")]
pub async fn execute_env_print(
    path: &str,
    package: &str,
    format: &str,
    environment: Option<&str>,
) -> Result<String> {
    tracing::info!("Starting env print command");

    // Create CUE evaluator
    let evaluator = CueEvaluator::builder()
        .build()
        .map_err(super::convert_engine_error)?;

    // Convert path string to Path
    let dir_path = Path::new(path);

    // Evaluate the CUE package
    tracing::debug!("Evaluating CUE package '{}' at path '{}'", package, path);
    let manifest: Cuenv = evaluator
        .evaluate_typed(dir_path, package)
        .map_err(super::convert_engine_error)?;

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
