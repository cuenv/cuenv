use cuengine::CueEvaluator;
use cuenv_core::{
    approval::{ApprovalManager, ApprovalStatus, check_approval_status},
    hooks::{executor::HookExecutor, ExecHook},
    Result
};
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

/// Execute env load command to start background hook execution
#[instrument(name = "env_load")]
pub async fn execute_env_load(path: &str) -> Result<String> {
    tracing::info!("Starting env load command");

    let directory = Path::new(path);
    
    // Validate directory exists
    if !directory.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "Directory does not exist: {}",
            path
        )));
    }

    // Evaluate CUE configuration
    let config_value = evaluate_cue_config(directory).await?;
    
    // Check approval status
    let approval_manager = ApprovalManager::new()?;
    let approval_status = check_approval_status(&approval_manager, directory, &config_value)?;
    
    match approval_status {
        ApprovalStatus::Approved => {
            // Configuration is approved, proceed with hook execution
            execute_hooks_if_present(directory, &config_value).await
        }
        ApprovalStatus::RequiresApproval { current_hash } => {
            Ok(format!(
                "Configuration has changed (hash: {})\nRun 'cuenv allow --path {}' to approve the new configuration",
                &current_hash[..16],
                directory.display()
            ))
        }
        ApprovalStatus::NotApproved { current_hash } => {
            Ok(format!(
                "Configuration not approved (hash: {})\nRun 'cuenv allow --path {}' to approve this configuration", 
                &current_hash[..16],
                directory.display()
            ))
        }
    }
}

/// Execute env status command to show hook execution status
#[instrument(name = "env_status")]
pub async fn execute_env_status(path: &str, wait: bool) -> Result<String> {
    tracing::info!("Starting env status command");

    let directory = Path::new(path);
    
    // Validate directory exists
    if !directory.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "Directory does not exist: {}",
            path
        )));
    }

    let executor = HookExecutor::with_default_config()?;
    
    if wait {
        // Wait for completion and return final status
        match executor.wait_for_completion(directory, Some(300)).await {
            Ok(state) => {
                Ok(state.progress_display())
            }
            Err(_) => {
                // Check current status if wait times out
                if let Some(state) = executor.get_execution_status(directory).await? {
                    Ok(state.progress_display())
                } else {
                    Ok("No hook execution in progress".to_string())
                }
            }
        }
    } else {
        // Return current status immediately  
        if let Some(state) = executor.get_execution_status(directory).await? {
            Ok(state.progress_display())
        } else {
            Ok("No hook execution in progress".to_string())
        }
    }
}

/// Evaluate CUE configuration in directory
async fn evaluate_cue_config(directory: &Path) -> Result<serde_json::Value> {
    // Look for env.cue file
    let env_cue_path = directory.join("env.cue");
    if !env_cue_path.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in directory: {}",
            directory.display()
        )));
    }
    
    // Create CUE evaluator
    let evaluator = CueEvaluator::builder().build()?;
    
    // Evaluate the CUE package
    tracing::debug!("Evaluating CUE package at path '{}'", directory.display());
    let json_result = evaluator.evaluate(directory, "cuenv")?;
    
    // Parse the JSON response
    let parsed: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output as JSON: {e}"))
    })?;
    
    Ok(parsed)
}

/// Execute hooks if present in configuration
async fn execute_hooks_if_present(directory: &Path, config_value: &serde_json::Value) -> Result<String> {
    // Extract hooks from configuration
    let hooks = extract_hooks_from_config(config_value)?;
    
    if hooks.is_empty() {
        return Ok("No hooks to execute".to_string());
    }
    
    // Compute config hash for tracking
    let config_hash = ApprovalManager::compute_config_hash(config_value);
    
    // Create hook executor and start background execution
    let executor = HookExecutor::with_default_config()?;
    executor.execute_hooks_background(
        directory.to_path_buf(),
        hooks.clone(),
        config_hash
    ).await?;
    
    Ok(format!("Started execution of {} hooks in background", hooks.len()))
}

/// Extract hooks from CUE configuration
fn extract_hooks_from_config(config_value: &serde_json::Value) -> Result<Vec<ExecHook>> {
    let mut hooks = Vec::new();
    
    // Look for hooks.onEnter
    if let Some(hooks_obj) = config_value.get("hooks") {
        if let Some(on_enter) = hooks_obj.get("onEnter") {
            match on_enter {
                // Single hook
                serde_json::Value::Object(_) => {
                    let hook: ExecHook = serde_json::from_value(on_enter.clone()).map_err(|e| {
                        cuenv_core::Error::configuration(format!("Failed to parse hook: {e}"))
                    })?;
                    hooks.push(hook);
                }
                // Array of hooks
                serde_json::Value::Array(hook_array) => {
                    for hook_value in hook_array {
                        let hook: ExecHook = serde_json::from_value(hook_value.clone()).map_err(|e| {
                            cuenv_core::Error::configuration(format!("Failed to parse hook: {e}"))
                        })?;
                        hooks.push(hook);
                    }
                }
                _ => {
                    return Err(cuenv_core::Error::configuration(
                        "hooks.onEnter must be an object or array of objects".to_string()
                    ));
                }
            }
        }
    }
    
    Ok(hooks)
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

    #[tokio::test]
    async fn test_execute_env_load_directory_not_found() {
        let result = execute_env_load("/nonexistent/directory").await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Directory does not exist"));
    }

    #[tokio::test]
    async fn test_execute_env_status_directory_not_found() {
        let result = execute_env_status("/nonexistent/directory", false).await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Directory does not exist"));
    }

    #[test]
    fn test_extract_hooks_from_config_empty() {
        let config = json!({});
        let hooks = extract_hooks_from_config(&config).unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_extract_hooks_from_config_single_hook() {
        let config = json!({
            "hooks": {
                "onEnter": {
                    "command": "echo",
                    "args": ["hello"]
                }
            }
        });
        
        let hooks = extract_hooks_from_config(&config).unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "echo");
        assert_eq!(hooks[0].args, Some(vec!["hello".to_string()]));
    }

    #[test]
    fn test_extract_hooks_from_config_multiple_hooks() {
        let config = json!({
            "hooks": {
                "onEnter": [
                    {
                        "command": "echo",
                        "args": ["first"]
                    },
                    {
                        "command": "echo", 
                        "args": ["second"]
                    }
                ]
            }
        });
        
        let hooks = extract_hooks_from_config(&config).unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].command, "echo");
        assert_eq!(hooks[1].command, "echo");
    }

    #[test]
    fn test_extract_hooks_from_config_invalid_format() {
        let config = json!({
            "hooks": {
                "onEnter": "invalid"
            }
        });
        
        let result = extract_hooks_from_config(&config);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("must be an object or array"));
    }
}
