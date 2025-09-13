//! Export command - the heart of cuenv's shell integration
//!
//! This command is called by the shell hook on every prompt to:
//! 1. Check if environment is ready (instant)
//! 2. Start supervisor if needed (async)
//! 3. Return environment diff for shell evaluation

use cuengine::CueEvaluator;
use cuenv_core::{
    hooks::{
        approval::{check_approval_status, ApprovalManager, ApprovalStatus},
        executor::HookExecutor,
        types::ExecutionStatus,
    },
    shell::Shell,
    Result,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info};

/// Execute the export command - the main entry point for shell integration
pub async fn execute_export(shell_type: Option<&str>, package: &str) -> Result<String> {
    let shell = Shell::detect(shell_type);
    let current_dir = std::env::current_dir().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
    })?;

    // Check if env.cue exists
    let env_file = current_dir.join("env.cue");
    if !env_file.exists() {
        debug!("No env.cue found in {}", current_dir.display());
        return Ok(format_no_op(shell));
    }

    // Always evaluate CUE to get current config (not a bottleneck)
    debug!("Evaluating CUE for {}", current_dir.display());
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&current_dir, package)?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output: {e}"))
    })?;

    // Load approval manager and check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    debug!("Checking approval for directory: {}", current_dir.display());
    let approval_status = check_approval_status(&approval_manager, &current_dir, &config)?;

    match approval_status {
        ApprovalStatus::NotApproved { .. } | ApprovalStatus::RequiresApproval { .. } => {
            // Return a comment that tells user to approve
            return Ok(format_not_allowed(&current_dir, shell));
        }
        ApprovalStatus::Approved => {
            // Continue with loading
        }
    }

    // Compute config hash for this directory + config
    let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config);

    // Check if state is ready
    let executor = HookExecutor::with_default_config()?;
    if let Some(state) = executor
        .get_execution_status_for_instance(&current_dir, &config_hash)
        .await?
    {
        match state.status {
            ExecutionStatus::Completed => {
                // Environment is ready - format and return with diff support
                let env_vars = collect_all_env_vars(&config, &state.environment_vars);
                return Ok(format_env_diff_with_unset(
                    env_vars,
                    state.previous_env.as_ref(),
                    shell,
                ));
            }
            ExecutionStatus::Failed => {
                // Hooks failed - return safe no-op
                debug!(
                    "Hooks failed for {}: {:?}",
                    current_dir.display(),
                    state.error_message
                );
                return Ok(format_no_op(shell));
            }
            ExecutionStatus::Running => {
                // Still running - wait briefly
                debug!("Hooks still running for {}", current_dir.display());
            }
            ExecutionStatus::Cancelled => {
                // Cancelled - return safe no-op
                return Ok(format_no_op(shell));
            }
        }
    } else {
        // No state - need to start supervisor
        info!("Starting hook execution for {}", current_dir.display());

        // Extract hooks from config
        let hooks = extract_hooks_from_config(&config);
        if !hooks.is_empty() {
            executor
                .execute_hooks_background(current_dir.clone(), config_hash.clone(), hooks)
                .await?;
        }
    }

    // Wait briefly for fast hooks (10ms)
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Check again
    if let Some(state) = executor
        .get_execution_status_for_instance(&current_dir, &config_hash)
        .await?
        && state.status == ExecutionStatus::Completed
    {
        let env_vars = collect_all_env_vars(&config, &state.environment_vars);
        return Ok(format_env_diff_with_unset(
            env_vars,
            state.previous_env.as_ref(),
            shell,
        ));
    }

    // Still not ready - return partial environment (just static vars from CUE)
    let static_env = extract_static_env_vars(&config);
    if !static_env.is_empty() {
        debug!(
            "Returning partial environment ({} vars) while hooks run",
            static_env.len()
        );
        return Ok(format_env_diff(static_env, shell));
    }

    // No environment available yet - return safe no-op
    Ok(format_no_op(shell))
}

/// Extract hooks from CUE config
fn extract_hooks_from_config(config: &Value) -> Vec<cuenv_core::hooks::types::Hook> {
    let mut hooks = Vec::new();

    if let Some(hooks_obj) = config.get("hooks").and_then(|v| v.as_object())
        && let Some(on_enter) = hooks_obj.get("onEnter").and_then(|v| v.as_array())
    {
        for hook_value in on_enter {
            if let Some(hook_obj) = hook_value.as_object() {
                let command = hook_obj
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("echo")
                    .to_string();

                let args = hook_obj
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                let source = hook_obj
                    .get("source")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                hooks.push(cuenv_core::hooks::types::Hook {
                    command,
                    args,
                    dir: None,
                    inputs: Vec::new(),
                    source: Some(source),
                });
            }
        }
    }

    hooks
}

/// Extract static environment variables from CUE config
fn extract_static_env_vars(config: &Value) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    if let Some(env_obj) = config.get("env").and_then(|v| v.as_object()) {
        for (key, value) in env_obj {
            if key == "environment" {
                continue; // Skip the special environment key
            }

            let value_str = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue, // Skip complex values
            };
            env_vars.insert(key.clone(), value_str);
        }
    }

    env_vars
}

/// Collect all environment variables (static + hook-generated)
fn collect_all_env_vars(
    config: &Value,
    hook_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut all_vars = extract_static_env_vars(config);

    // Hook environment variables override static ones
    for (key, value) in hook_env {
        all_vars.insert(key.clone(), value.clone());
    }

    all_vars
}

/// Format environment variables as shell export commands
fn format_env_diff(env: HashMap<String, String>, shell: Shell) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    for (key, value) in env {
        let escaped_value = escape_shell_value(&value);
        match shell {
            Shell::Bash | Shell::Zsh => {
                writeln!(&mut output, "export {key}=\"{escaped_value}\"").unwrap();
            }
            Shell::Fish => {
                writeln!(&mut output, "set -x {key} \"{escaped_value}\"").unwrap();
            }
            Shell::PowerShell => {
                writeln!(&mut output, "$env:{key} = \"{escaped_value}\"").unwrap();
            }
        }
    }

    output
}

/// Format environment diff with unset commands for removed variables
fn format_env_diff_with_unset(
    current_env: HashMap<String, String>,
    previous_env: Option<&HashMap<String, String>>,
    shell: Shell,
) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    // If we have a previous environment, generate unset commands for removed variables
    if let Some(prev) = previous_env {
        for key in prev.keys() {
            if !current_env.contains_key(key) {
                // Variable was removed, generate unset command
                match shell {
                    Shell::Bash | Shell::Zsh => {
                        writeln!(&mut output, "unset {key}").unwrap();
                    }
                    Shell::Fish => {
                        writeln!(&mut output, "set -e {key}").unwrap();
                    }
                    Shell::PowerShell => {
                        writeln!(&mut output, "Remove-Item Env:{key}").unwrap();
                    }
                }
            }
        }
    }

    // Export current environment variables
    for (key, value) in current_env {
        let escaped_value = escape_shell_value(&value);
        match shell {
            Shell::Bash | Shell::Zsh => {
                writeln!(&mut output, "export {key}=\"{escaped_value}\"").unwrap();
            }
            Shell::Fish => {
                writeln!(&mut output, "set -x {key} \"{escaped_value}\"").unwrap();
            }
            Shell::PowerShell => {
                writeln!(&mut output, "$env:{key} = \"{escaped_value}\"").unwrap();
            }
        }
    }

    output
}

/// Format "not allowed" message as a shell comment
fn format_not_allowed(dir: &Path, _shell: Shell) -> String {
    let comment_char = "#";

    format!(
        "{} Configuration not approved for {}\n{} Run 'cuenv allow' to approve\n",
        comment_char,
        dir.display(),
        comment_char
    )
}

/// Escape special characters in shell values
fn escape_shell_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

/// Format a safe no-op command for the shell
fn format_no_op(shell: Shell) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => ":".to_string(),
        Shell::Fish => "true".to_string(),
        Shell::PowerShell => "# no changes".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_shell_value() {
        // Test basic string
        assert_eq!(escape_shell_value("simple"), "simple");

        // Test double quotes
        assert_eq!(escape_shell_value("hello \"world\""), "hello \\\"world\\\"");

        // Test backslashes
        assert_eq!(escape_shell_value("path\\to\\file"), "path\\\\to\\\\file");

        // Test dollar signs
        assert_eq!(escape_shell_value("$HOME"), "\\$HOME");
        assert_eq!(escape_shell_value("test $var"), "test \\$var");

        // Test backticks
        assert_eq!(escape_shell_value("`command`"), "\\`command\\`");

        // Test multiple special characters
        assert_eq!(
            escape_shell_value("$HOME/path\\with\"quotes`and`backticks"),
            "\\$HOME/path\\\\with\\\"quotes\\`and\\`backticks"
        );

        // Test empty string
        assert_eq!(escape_shell_value(""), "");

        // Test string with newlines (not escaped)
        assert_eq!(escape_shell_value("line1\nline2"), "line1\nline2");

        // Test string with tabs (not escaped)
        assert_eq!(escape_shell_value("col1\tcol2"), "col1\tcol2");
    }

    #[test]
    fn test_format_no_op() {
        assert_eq!(format_no_op(Shell::Bash), ":");
        assert_eq!(format_no_op(Shell::Zsh), ":");
        assert_eq!(format_no_op(Shell::Fish), "true");
        assert_eq!(format_no_op(Shell::PowerShell), "# no changes");
    }
}
