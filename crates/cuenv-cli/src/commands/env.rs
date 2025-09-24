use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::Result;
use std::path::Path;
use tracing::instrument;

#[instrument(name = "env_print")]
pub async fn execute_env_print(path: &str, package: &str, format: &str) -> Result<String> {
    tracing::info!("Starting env print command");

    // Create CUE evaluator
    let evaluator = CueEvaluator::builder().build()?;

    // Convert path string to Path
    let dir_path = Path::new(path);

    // Evaluate the CUE package
    tracing::debug!("Evaluating CUE package '{}' at path '{}'", package, path);
    let manifest: Cuenv = evaluator.evaluate_typed(dir_path, package)?;

    // Extract the env field
    let env = manifest.env.ok_or_else(|| {
        cuenv_core::Error::configuration("No 'env' field found in CUE package".to_string())
    })?;

    // Format and return the output
    let output = match format {
        "json" => serde_json::to_string_pretty(&env)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to format JSON: {e}")))?,
        "env" | "simple" => format_as_env_vars(&env), // Simple format is same as env format for env variables
        other => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported format: '{other}'. Supported formats are 'json', 'env', and 'simple'."
            )));
        }
    };

    tracing::info!("Env print command completed successfully");
    Ok(output)
}

fn format_as_env_vars(env: &cuenv_core::environment::Env) -> String {
    let mut lines = Vec::new();

    // Sort keys for consistent output
    let mut keys: Vec<&String> = env.base.keys().collect();
    keys.sort();

    for key in keys {
        let value = &env.base[key];
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
    use cuenv_core::environment::{Env, EnvValue};
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

        let env = Env {
            base: env_map,
            environment: None,
        };

        let result = format_as_env_vars(&env);
        let lines: Vec<&str> = result.split('\n').collect();

        // Should be sorted alphabetically
        assert_eq!(lines.len(), 3);
        assert!(lines.contains(&"DATABASE_URL=postgres://localhost/mydb"));
        assert!(lines.contains(&"DEBUG=true"));
        assert!(lines.contains(&"PORT=3000"));
    }

    #[test]
    fn test_format_as_env_vars_empty() {
        let env = Env {
            base: HashMap::new(),
            environment: None,
        };
        let result = format_as_env_vars(&env);
        assert_eq!(result, "");
    }
}
