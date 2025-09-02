use cuengine::CueEvaluator;
use cuenv_core::Result;
use serde_json::Value;
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
    let json_result = evaluator.evaluate(dir_path, package)?;

    // Parse the JSON response
    let parsed: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output as JSON: {e}"))
    })?;

    // Extract the env field
    let env_object = parsed.get("env").ok_or_else(|| {
        cuenv_core::Error::configuration("No 'env' field found in CUE package".to_string())
    })?;

    // Format and return the output
    let output = match format {
        "json" => serde_json::to_string_pretty(env_object)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to format JSON: {e}")))?,
        "env" => format_as_env_vars(env_object)?,
        other => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported format: '{other}'. Supported formats are 'json' and 'env'."
            )));
        }
    };

    tracing::info!("Env print command completed successfully");
    Ok(output)
}

fn format_as_env_vars(env_object: &Value) -> Result<String> {
    let mut lines = Vec::new();

    if let Value::Object(obj) = env_object {
        // Sort keys for consistent output
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();

        for key in keys {
            let value = &obj[key];
            let formatted_value = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => serde_json::to_string(value).map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to serialize value for key '{key}': {e}"
                    ))
                })?,
            };
            lines.push(format!("{key}={formatted_value}"));
        }
    } else {
        return Err(cuenv_core::Error::configuration(
            "env field is not an object".to_string(),
        ));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_as_env_vars_basic() {
        let env_json = json!({
            "DATABASE_URL": "postgres://localhost/mydb",
            "DEBUG": true,
            "PORT": 3000
        });

        let result = format_as_env_vars(&env_json).unwrap();
        let lines: Vec<&str> = result.split('\n').collect();

        // Should be sorted alphabetically
        assert_eq!(lines.len(), 3);
        assert!(lines.contains(&"DATABASE_URL=postgres://localhost/mydb"));
        assert!(lines.contains(&"DEBUG=true"));
        assert!(lines.contains(&"PORT=3000"));
    }

    #[test]
    fn test_format_as_env_vars_empty() {
        let env_json = json!({});
        let result = format_as_env_vars(&env_json).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_as_env_vars_complex_values() {
        let env_json = json!({
            "ARRAY_VAL": [1, 2, 3],
            "NULL_VAL": null,
            "OBJECT_VAL": {"nested": "value"}
        });

        let result = format_as_env_vars(&env_json).unwrap();
        let lines: Vec<&str> = result.split('\n').collect();

        assert_eq!(lines.len(), 3);
        assert!(lines
            .iter()
            .any(|line| line.starts_with("ARRAY_VAL=[1,2,3]")));
        assert!(lines.contains(&"NULL_VAL=null"));
        assert!(lines
            .iter()
            .any(|line| line.starts_with("OBJECT_VAL=") && line.contains("nested")));
    }

    #[test]
    fn test_format_as_env_vars_invalid_input() {
        let env_json = json!("not an object");
        let result = format_as_env_vars(&env_json);
        assert!(result.is_err());
    }
}
