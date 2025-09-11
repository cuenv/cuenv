use cuengine::CueEvaluator;
use cuenv_core::{
    approval::{ApprovalManager, ConfigSummary},
    hooks::{executor::HookExecutor, state::{HookExecutionState, StateManager}, Hook, HookExecutionConfig},
    Result,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, instrument};

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

/// Execute env load command - evaluates config, checks approval, executes hooks in background
#[instrument(name = "env_load")]
pub async fn execute_env_load(directory: Option<PathBuf>) -> Result<String> {
    let dir = directory.unwrap_or_else(|| PathBuf::from("."));
    info!("Loading environment from: {}", dir.display());
    
    // Check if env.cue exists
    let env_file = dir.join("env.cue");
    if !env_file.exists() {
        debug!("No env.cue file found in {}", dir.display());
        return Ok("No env.cue file found in current directory".to_string());
    }
    
    // Evaluate the CUE configuration
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&dir, "cuenv")?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output: {}", e))
    })?;
    
    // Compute configuration hash
    let config_hash = ApprovalManager::compute_hash(&config);
    
    // Check approval
    let approval_file = ApprovalManager::default_approval_file()?;
    let mut approval_manager = ApprovalManager::new(approval_file);
    approval_manager.load_approvals().await?;
    
    if !approval_manager.is_approved(&config_hash) {
        let summary = ConfigSummary::from_json(&config);
        return Ok(format!(
            "Configuration not approved. This configuration contains: {}\n\
             Run 'cuenv allow' to approve this configuration",
            summary.description()
        ));
    }
    
    // Extract hooks from configuration
    let hooks = extract_hooks_from_config(&config)?;
    
    if hooks.is_empty() {
        info!("No hooks to execute");
        return Ok("Environment loaded (no hooks to execute)".to_string());
    }
    
    // Create state for tracking
    let state_dir = StateManager::default_state_dir()?;
    let state_manager = StateManager::new(state_dir);
    
    let mut state = HookExecutionState::new(
        dir.clone(),
        config_hash.clone(),
        hooks.len(),
    );
    
    // Save initial state
    state_manager.save_state(&state).await?;
    
    // Execute hooks in background
    tokio::spawn(async move {
        let executor = HookExecutor::new(HookExecutionConfig::default());
        
        for (index, hook) in hooks.into_iter().enumerate() {
            // Update state to mark hook as running
            state.mark_running(index);
            if let Err(e) = state_manager.save_state(&state).await {
                error!("Failed to save state: {}", e);
            }
            
            // Execute the hook
            match executor.execute_single_hook(hook).await {
                Ok(result) => {
                    state.update_result(index, result);
                    if let Err(e) = state_manager.save_state(&state).await {
                        error!("Failed to save state: {}", e);
                    }
                }
                Err(e) => {
                    error!("Hook execution failed: {}", e);
                    break;
                }
            }
            
            // Check if we should stop (fail-fast)
            if state.is_complete() {
                break;
            }
        }
        
        info!("Hook execution completed: {}", state.status_string());
    });
    
    Ok(format!("Environment loading started. Run 'cuenv env status' to check progress."))
}

/// Execute env status command - show current hook execution status
#[instrument(name = "env_status")]
pub async fn execute_env_status(wait: bool) -> Result<String> {
    let dir = PathBuf::from(".");
    
    // Get config hash for current directory
    let env_file = dir.join("env.cue");
    if !env_file.exists() {
        return Ok("No env.cue file in current directory".to_string());
    }
    
    // Evaluate to get hash
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&dir, "cuenv")?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output: {}", e))
    })?;
    let config_hash = ApprovalManager::compute_hash(&config);
    
    // Load state
    let state_dir = StateManager::default_state_dir()?;
    let state_manager = StateManager::new(state_dir);
    
    let mut state = match state_manager.load_state(&config_hash).await? {
        Some(s) => s,
        None => return Ok("No active environment loading".to_string()),
    };
    
    if wait {
        // Wait for completion
        while !state.is_complete() {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            state = match state_manager.load_state(&config_hash).await? {
                Some(s) => s,
                None => break,
            };
        }
    }
    
    Ok(state.status_string())
}

/// Extract hooks from the configuration
fn extract_hooks_from_config(config: &Value) -> Result<Vec<Hook>> {
    let mut hooks = Vec::new();
    
    if let Some(hooks_obj) = config.get("hooks").and_then(|v| v.as_object()) {
        // Process onEnter hooks
        if let Some(on_enter) = hooks_obj.get("onEnter") {
            if let Some(arr) = on_enter.as_array() {
                for hook_value in arr {
                    if let Ok(hook) = serde_json::from_value::<Hook>(hook_value.clone()) {
                        hooks.push(hook);
                    }
                }
            } else if let Ok(hook) = serde_json::from_value::<Hook>(on_enter.clone()) {
                hooks.push(hook);
            }
        }
    }
    
    Ok(hooks)
}
