//! Hook-related command implementations

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::{
    hooks::{
        approval::{check_approval_status, ApprovalManager, ApprovalStatus, ConfigSummary},
        executor::HookExecutor,
        types::Hook,
    },
    Result,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::info;

/// Helper to check if env.cue exists and return early if not
fn check_env_file(path: &Path) -> Result<PathBuf> {
    let directory = if path.is_absolute() {
        path.to_path_buf()
    } else {
        PathBuf::from(path)
    };

    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in '{}'",
            path.display()
        )));
    }

    // Canonicalize the path to ensure consistency
    directory
        .canonicalize()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to canonicalize path: {e}")))
}

/// Helper to evaluate CUE configuration
fn evaluate_config(directory: &Path, package: &str) -> Result<Cuenv> {
    let evaluator = CueEvaluator::builder().build()?;
    evaluator.evaluate_typed(directory, package)
}

/// Helper to evaluate CUE configuration as Value (for approval system)
fn evaluate_config_as_value(directory: &Path, package: &str) -> Result<Value> {
    let manifest = evaluate_config(directory, package)?;
    serde_json::to_value(&manifest)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize config: {e}")))
}

/// Helper to get config hash from approval manager or compute it
fn get_config_hash(
    directory: &Path,
    package: &str,
    approval_manager: &ApprovalManager,
) -> Result<String> {
    if let Some(approval) = approval_manager.get_approval(directory.to_str().unwrap_or("")) {
        Ok(approval.config_hash.clone())
    } else {
        // If not approved, compute it from current config
        let config_value = evaluate_config_as_value(directory, package)?;
        Ok(cuenv_core::hooks::approval::compute_config_hash(
            &config_value,
        ))
    }
}

/// Execute env load command - evaluates config, checks approval, starts hook execution
pub async fn execute_env_load(path: &str, package: &str) -> Result<String> {
    // Check env.cue and canonicalize path
    let Ok(directory) = check_env_file(Path::new(path)) else {
        return Ok(format!("No env.cue file found in '{path}'"));
    };

    // Evaluate the CUE configuration
    let config = evaluate_config(&directory, package)?;
    let config_value = serde_json::to_value(&config).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize config: {e}"))
    })?;

    // Check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    let approval_status = check_approval_status(&approval_manager, &directory, &config_value)?;

    match approval_status {
        ApprovalStatus::Approved => {
            // Extract hooks from configuration and execute
            let hooks = extract_hooks_from_config(&config);

            if hooks.is_empty() {
                return Ok("No hooks to execute".to_string());
            }

            // Start background execution
            let executor = HookExecutor::with_default_config()?;
            let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config_value);

            let result = executor
                .execute_hooks_background(directory.clone(), config_hash, hooks)
                .await?;

            Ok(result)
        }
        ApprovalStatus::RequiresApproval { current_hash } => {
            let summary = ConfigSummary::from_json(&config_value);
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
            let summary = ConfigSummary::from_json(&config_value);
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
pub async fn execute_env_status(
    path: &str,
    package: &str,
    wait: bool,
    timeout_seconds: u64,
) -> Result<String> {
    // Check env.cue and canonicalize path
    let Ok(directory) = check_env_file(Path::new(path)) else {
        return Ok(format!("No env.cue file found in '{path}'"));
    };

    // Get the config hash
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;
    let config_hash = get_config_hash(&directory, package, &approval_manager)?;

    let executor = HookExecutor::with_default_config()?;

    if wait {
        // Wait for completion with timeout
        match executor
            .wait_for_completion(&directory, &config_hash, Some(timeout_seconds))
            .await
        {
            Ok(state) => Ok(state.progress_display()),
            Err(cuenv_core::Error::Timeout { .. }) => {
                // Timeout occurred, get current status
                if let Some(state) = executor
                    .get_execution_status_for_instance(&directory, &config_hash)
                    .await?
                {
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
        if let Some(state) = executor
            .get_execution_status_for_instance(&directory, &config_hash)
            .await?
        {
            Ok(state.progress_display())
        } else {
            Ok("No hook execution in progress".to_string())
        }
    }
}

/// Execute allow command - approve current directory's configuration
pub async fn execute_allow(path: &str, package: &str, note: Option<String>) -> Result<String> {
    // Check env.cue and canonicalize path
    let directory = check_env_file(Path::new(path))?;

    // Evaluate the CUE configuration
    let config = evaluate_config(&directory, package)?;
    let config_value = serde_json::to_value(&config).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize config: {e}"))
    })?;

    // Compute configuration hash
    let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config_value);

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
    let summary = ConfigSummary::from_json(&config_value);

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

/// Execute env check command - check hook status and output env for shell
pub async fn execute_env_check(
    path: &str,
    package: &str,
    shell: crate::cli::ShellType,
) -> Result<String> {
    // Check env.cue and canonicalize path - silent return if no env.cue
    let Ok(directory) = check_env_file(Path::new(path)) else {
        return Ok(String::new()); // Silent return for non-cuenv directories
    };

    // Get the config hash
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;
    let config_hash = get_config_hash(&directory, package, &approval_manager)?;

    let executor = HookExecutor::with_default_config()?;

    // Check execution status using the specific instance
    if let Some(state) = executor
        .get_execution_status_for_instance(&directory, &config_hash)
        .await?
        && state.is_complete()
        && state.status == cuenv_core::hooks::types::ExecutionStatus::Completed
    {
        let mut output = String::new();
        let mut all_env_vars = HashMap::new();

        // First, get environment variables from CUE configuration
        let config = evaluate_config(&directory, package)?;

        // Add CUE env variables to our map
        if let Some(env) = &config.env {
            for (key, value) in &env.base {
                let value_str = match value {
                    cuenv_core::environment::EnvValue::String(s) => s.clone(),
                    cuenv_core::environment::EnvValue::Int(i) => i.to_string(),
                    cuenv_core::environment::EnvValue::Bool(b) => b.to_string(),
                    cuenv_core::environment::EnvValue::Secret(_) => continue, // Skip secrets
                };
                all_env_vars.insert(key.clone(), value_str);
            }
        }

        // Then, add/override with environment variables from source hooks
        for (key, value) in &state.environment_vars {
            all_env_vars.insert(key.clone(), value.clone());
        }

        // Output all environment variables
        for (key, value) in &all_env_vars {
            match shell {
                crate::cli::ShellType::Fish => {
                    use std::fmt::Write;
                    writeln!(&mut output, "set -x {key} \"{value}\"").unwrap();
                }
                crate::cli::ShellType::Bash | crate::cli::ShellType::Zsh => {
                    use std::fmt::Write;
                    writeln!(&mut output, "export {key}=\"{value}\"").unwrap();
                }
            }
        }

        return Ok(output);
    }

    // No environment to load
    Ok(String::new())
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
fn extract_hooks_from_config(config: &Cuenv) -> Vec<Hook> {
    config.on_enter_hooks()
}

/// Generate Fish shell integration script
fn generate_fish_integration() -> String {
    r"# cuenv Fish shell integration
# Add this to your ~/.config/fish/config.fish

# Mark that shell integration is active
set -x CUENV_SHELL_INTEGRATION 1

# Hook function that loads environment on each prompt
function __cuenv_hook --on-variable PWD
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    source (cuenv export --shell fish 2>/dev/null | psub)
end

# Also run on shell startup
source (cuenv export --shell fish 2>/dev/null | psub)"
        .to_string()
}

/// Generate Bash shell integration script  
fn generate_bash_integration() -> String {
    r#"# cuenv Bash shell integration
# Add this to your ~/.bashrc

# Mark that shell integration is active
export CUENV_SHELL_INTEGRATION=1

# Hook function that loads environment on each prompt
__cuenv_hook() {
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    eval "$(cuenv export --shell bash 2>/dev/null)"
}

# Set up the hook via PROMPT_COMMAND
if [[ -n "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__cuenv_hook; $PROMPT_COMMAND"
else
    PROMPT_COMMAND="__cuenv_hook"
fi

# Also run on shell startup
__cuenv_hook"#
        .to_string()
}

/// Generate Zsh shell integration script
fn generate_zsh_integration() -> String {
    r#"# cuenv Zsh shell integration  
# Add this to your ~/.zshrc

# Mark that shell integration is active
export CUENV_SHELL_INTEGRATION=1

# Hook function that loads environment on each prompt
__cuenv_hook() {
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    eval "$(cuenv export --shell zsh 2>/dev/null)"
}

# Set up the hook via precmd
autoload -U add-zsh-hook
add-zsh-hook precmd __cuenv_hook

# Also run on shell startup
__cuenv_hook"#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_extract_hooks_from_config() {
        use cuenv_core::hooks::types::Hook;
        use cuenv_core::manifest::{Cuenv, HookList, Hooks};

        let config = Cuenv {
            config: None,
            env: None,
            hooks: Some(Hooks {
                on_enter: Some(HookList::Multiple(vec![
                    Hook {
                        command: "npm".to_string(),
                        args: vec!["install".to_string()],
                        dir: None,
                        inputs: vec![],
                        source: None,
                    },
                    Hook {
                        command: "docker-compose".to_string(),
                        args: vec!["up".to_string(), "-d".to_string()],
                        dir: None,
                        inputs: vec![],
                        source: None,
                    },
                ])),
                on_exit: None,
            }),
            tasks: std::collections::HashMap::new(),
        };

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].command, "npm");
        assert_eq!(hooks[0].args, vec!["install"]);
        assert_eq!(hooks[1].command, "docker-compose");
        assert_eq!(hooks[1].args, vec!["up", "-d"]);
    }

    #[test]
    fn test_extract_hooks_single_hook() {
        use cuenv_core::hooks::types::Hook;
        use cuenv_core::manifest::{Cuenv, HookList, Hooks};

        let config = Cuenv {
            config: None,
            env: None,
            hooks: Some(Hooks {
                on_enter: Some(HookList::Single(Hook {
                    command: "echo".to_string(),
                    args: vec!["hello".to_string()],
                    dir: None,
                    inputs: vec![],
                    source: None,
                })),
                on_exit: None,
            }),
            tasks: std::collections::HashMap::new(),
        };

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "echo");
        assert_eq!(hooks[0].args, vec!["hello"]);
    }

    #[test]
    fn test_extract_hooks_empty_config() {
        use cuenv_core::manifest::Cuenv;

        let config = Cuenv {
            config: None,
            env: None,
            hooks: None,
            tasks: std::collections::HashMap::new(),
        };

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 0);
    }

    #[test]
    fn test_shell_integration_generation() {
        let fish_script = generate_fish_integration();
        assert!(fish_script.contains("function __cuenv_hook"));
        assert!(fish_script.contains("on-variable PWD"));

        let bash_script = generate_bash_integration();
        assert!(bash_script.contains("__cuenv_hook()"));
        assert!(bash_script.contains("PROMPT_COMMAND"));

        let zsh_script = generate_zsh_integration();
        assert!(zsh_script.contains("add-zsh-hook"));
        assert!(zsh_script.contains("precmd"));
    }

    #[tokio::test]
    async fn test_execute_allow_no_directory() {
        let result = execute_allow("/nonexistent/directory", "cuenv", None).await;
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
        let result = execute_allow(temp_dir.path().to_str().unwrap(), "cuenv", None).await;
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
        let result = execute_env_load(temp_dir.path().to_str().unwrap(), "cuenv").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_execute_env_status_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result =
            execute_env_status(temp_dir.path().to_str().unwrap(), "cuenv", false, 30).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }
}
