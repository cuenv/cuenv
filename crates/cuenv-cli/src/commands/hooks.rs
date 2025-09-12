//! Hook-related command implementations

use cuengine::CueEvaluator;
use cuenv_core::{
    hooks::{
        approval::{check_approval_status, ApprovalManager, ApprovalStatus, ConfigSummary},
        executor::HookExecutor,
        types::Hook,
    },
    Result,
};
use serde_json::Value;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Execute env load command - evaluates config, checks approval, starts hook execution
pub async fn execute_env_load(path: &str) -> Result<String> {
    let directory = PathBuf::from(path);

    // Check if env.cue exists
    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Ok(format!("No env.cue file found in '{path}'"));
    }

    // Evaluate the CUE configuration
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&directory, "cuenv")?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to parse CUE output: {e}\nRaw output: {json_result}"
        ))
    })?;

    // Check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    let approval_status = check_approval_status(&approval_manager, &directory, &config)?;

    match approval_status {
        ApprovalStatus::Approved => {
            // Extract hooks from configuration and execute
            let hooks = extract_hooks_from_config(&config);

            if hooks.is_empty() {
                return Ok("No hooks to execute".to_string());
            }

            // Start background execution
            let executor = HookExecutor::with_default_config()?;
            let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config);

            executor
                .execute_hooks_background(directory, config_hash, hooks)
                .await
        }
        ApprovalStatus::RequiresApproval { current_hash } => {
            let summary = ConfigSummary::from_json(&config);
            Ok(format!(
                "Configuration has changed and requires approval.\n\
                 This configuration contains: {}\n\
                 Hash: {}\n\
                 Run 'cuenv allow --path {}' to approve the new configuration",
                summary.description(),
                &current_hash[..16],
                path
            ))
        }
        ApprovalStatus::NotApproved { current_hash } => {
            let summary = ConfigSummary::from_json(&config);
            Ok(format!(
                "Configuration not approved.\n\
                 This configuration contains: {}\n\
                 Hash: {}\n\
                 Run 'cuenv allow --path {}' to approve this configuration",
                summary.description(),
                &current_hash[..16],
                path
            ))
        }
    }
}

/// Execute env status command - show current hook execution status
pub async fn execute_env_status(path: &str, wait: bool, timeout_seconds: u64) -> Result<String> {
    let directory = PathBuf::from(path);

    // Check if env.cue exists
    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Ok(format!("No env.cue file found in '{path}'"));
    }

    let executor = HookExecutor::with_default_config()?;

    if wait {
        // Wait for completion with timeout
        match executor
            .wait_for_completion(&directory, Some(timeout_seconds))
            .await
        {
            Ok(state) => Ok(state.progress_display()),
            Err(cuenv_core::Error::Timeout { .. }) => {
                // Timeout occurred, get current status
                if let Some(state) = executor.get_execution_status(&directory).await? {
                    Ok(format!(
                        "Timeout after {} seconds. Current status: {}",
                        timeout_seconds,
                        state.progress_display()
                    ))
                } else {
                    Ok("No hook execution in progress".to_string())
                }
            }
            Err(e) => Err(e),
        }
    } else {
        // Return current status immediately
        if let Some(state) = executor.get_execution_status(&directory).await? {
            Ok(state.progress_display())
        } else {
            Ok("No hook execution in progress".to_string())
        }
    }
}

/// Execute allow command - approve current directory's configuration
pub async fn execute_allow(path: &str, note: Option<String>) -> Result<String> {
    let directory = PathBuf::from(path);

    // Check if directory exists
    if !directory.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "Directory does not exist: {path}"
        )));
    }

    // Check if env.cue exists
    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in directory: {path}"
        )));
    }

    // Evaluate the CUE configuration
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&directory, "cuenv")?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to parse CUE output: {e}\nRaw output: {json_result}"
        ))
    })?;

    // Compute configuration hash
    let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config);

    // Initialize approval manager
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    // Check if already approved
    if approval_manager.is_approved(&directory, &config_hash)? {
        return Ok(format!(
            "Configuration is already approved for directory: {}",
            directory.display()
        ));
    }

    // Show what we're approving
    let summary = ConfigSummary::from_json(&config);

    // Approve the configuration
    approval_manager
        .approve_config(&directory, config_hash.clone(), note)
        .await?;

    info!(
        "Approved configuration for directory: {}",
        directory.display()
    );
    Ok(format!(
        "Configuration approved for directory: {}\n\
         Contains: {}\n\
         Hash: {}",
        directory.display(),
        summary.description(),
        &config_hash[..16]
    ))
}

/// Generate shell integration script
pub fn execute_shell_init(shell: crate::cli::ShellType) -> String {
    match shell {
        crate::cli::ShellType::Fish => generate_fish_integration(),
        crate::cli::ShellType::Bash => generate_bash_integration(),
        crate::cli::ShellType::Zsh => generate_zsh_integration(),
    }
}

/// Extract hooks from the configuration JSON
fn extract_hooks_from_config(config: &Value) -> Vec<Hook> {
    let mut hooks = Vec::new();

    if let Some(hooks_obj) = config.get("hooks").and_then(|v| v.as_object()) {
        // Process onEnter hooks
        if let Some(on_enter) = hooks_obj.get("onEnter") {
            extract_hooks_from_value(on_enter, &mut hooks);
        }

        // Could also process onExit hooks here if needed
        if let Some(_on_exit) = hooks_obj.get("onExit") {
            debug!("Found onExit hooks but skipping for now");
            // TODO: Implement onExit hook handling
        }
    }

    hooks
}

/// Extract hooks from a JSON value (array or single object)
fn extract_hooks_from_value(value: &Value, hooks: &mut Vec<Hook>) {
    if let Some(arr) = value.as_array() {
        for hook_value in arr {
            if let Ok(hook) = serde_json::from_value::<Hook>(hook_value.clone()) {
                hooks.push(hook);
            } else {
                warn!("Failed to parse hook from configuration: {:?}", hook_value);
            }
        }
    } else if let Ok(hook) = serde_json::from_value::<Hook>(value.clone()) {
        hooks.push(hook);
    } else {
        warn!(
            "Failed to parse single hook from configuration: {:?}",
            value
        );
    }
}

/// Generate Fish shell integration script
fn generate_fish_integration() -> String {
    r#"# cuenv Fish shell integration
# Add this to your ~/.config/fish/config.fish

function __cuenv_auto_load --on-variable PWD
    if test -f "$PWD/env.cue"
        cuenv env load --path "$PWD" 2>/dev/null
    end
end

# Also check current directory on startup
if test -f "$PWD/env.cue"
    cuenv env load --path "$PWD" 2>/dev/null
end"#
        .to_string()
}

/// Generate Bash shell integration script  
fn generate_bash_integration() -> String {
    r#"# cuenv Bash shell integration
# Add this to your ~/.bashrc

__cuenv_auto_load() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load --path "$PWD" 2>/dev/null
    fi
}

# Set up directory change hook
if [[ -n "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__cuenv_auto_load; $PROMPT_COMMAND"
else
    PROMPT_COMMAND="__cuenv_auto_load"
fi

# Also check current directory on startup
__cuenv_auto_load"#
        .to_string()
}

/// Generate Zsh shell integration script
fn generate_zsh_integration() -> String {
    r#"# cuenv Zsh shell integration  
# Add this to your ~/.zshrc

__cuenv_auto_load() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load --path "$PWD" 2>/dev/null
    fi
}

# Set up directory change hook
autoload -U add-zsh-hook
add-zsh-hook chpwd __cuenv_auto_load

# Also check current directory on startup
__cuenv_auto_load"#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_extract_hooks_from_config() {
        let config = json!({
            "env": {"NODE_ENV": "development"},
            "hooks": {
                "onEnter": [
                    {"command": "npm", "args": ["install"]},
                    {"command": "docker-compose", "args": ["up", "-d"]}
                ]
            }
        });

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].command, "npm");
        assert_eq!(hooks[0].args, vec!["install"]);
        assert_eq!(hooks[1].command, "docker-compose");
        assert_eq!(hooks[1].args, vec!["up", "-d"]);
    }

    #[test]
    fn test_extract_hooks_single_hook() {
        let config = json!({
            "hooks": {
                "onEnter": {"command": "echo", "args": ["hello"]}
            }
        });

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "echo");
        assert_eq!(hooks[0].args, vec!["hello"]);
    }

    #[test]
    fn test_extract_hooks_empty_config() {
        let config = json!({
            "env": {"TEST": "value"}
        });

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 0);
    }

    #[test]
    fn test_shell_integration_generation() {
        let fish_script = generate_fish_integration();
        assert!(fish_script.contains("function __cuenv_auto_load"));
        assert!(fish_script.contains("on-variable PWD"));

        let bash_script = generate_bash_integration();
        assert!(bash_script.contains("__cuenv_auto_load()"));
        assert!(bash_script.contains("PROMPT_COMMAND"));

        let zsh_script = generate_zsh_integration();
        assert!(zsh_script.contains("add-zsh-hook"));
        assert!(zsh_script.contains("chpwd"));
    }

    #[tokio::test]
    async fn test_execute_allow_no_directory() {
        let result = execute_allow("/nonexistent/directory", None).await;
        assert!(result.is_err());
        // The error type is Configuration error, which doesn't include the detailed message in Display
        // Just verify it's an error for a non-existent directory
        assert!(matches!(
            result.unwrap_err(),
            cuenv_core::Error::Configuration { .. }
        ));
    }

    #[tokio::test]
    async fn test_execute_allow_no_env_cue() {
        let temp_dir = TempDir::new().unwrap();
        let result = execute_allow(temp_dir.path().to_str().unwrap(), None).await;
        assert!(result.is_err());
        // The error type is Configuration error for missing env.cue file
        assert!(matches!(
            result.unwrap_err(),
            cuenv_core::Error::Configuration { .. }
        ));
    }

    #[tokio::test]
    async fn test_execute_env_load_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = execute_env_load(temp_dir.path().to_str().unwrap()).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_execute_env_status_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = execute_env_status(temp_dir.path().to_str().unwrap(), false, 30).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }
}
