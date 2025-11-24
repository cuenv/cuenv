//! Export command - the heart of cuenv's shell integration
//!
//! This command is called by the shell hook on every prompt to:
//! 1. Check if environment is ready (instant)
//! 2. Start supervisor if needed (async)
//! 3. Return environment diff for shell evaluation

use super::env_file::{self, EnvFileStatus};
use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::{
    Result,
    hooks::{
        approval::{ApprovalManager, ApprovalStatus, ConfigSummary, check_approval_status},
        executor::HookExecutor,
        types::ExecutionStatus,
    },
    shell::Shell,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info};

const PENDING_APPROVAL_ENV: &str = "CUENV_PENDING_APPROVAL_DIR";
const LOADED_DIR_ENV: &str = "CUENV_LOADED_DIR";

/// Execute the export command - the main entry point for shell integration
#[allow(clippy::too_many_lines, clippy::uninlined_format_args)]
pub async fn execute_export(shell_type: Option<&str>, package: &str) -> Result<String> {
    let shell = Shell::detect(shell_type);
    let current_dir = std::env::current_dir().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
    })?;

    // Check if env.cue exists with matching package
    let directory = match env_file::find_env_file(&current_dir, package)? {
        EnvFileStatus::Match(dir) => dir,
        EnvFileStatus::Missing => {
            debug!("No env.cue found in {}", current_dir.display());
            return Ok(format_no_op(shell));
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            debug!(
                "env.cue package mismatch in {}: found {:?}, expected {}",
                current_dir.display(),
                found_package,
                package
            );
            return Ok(format_no_op(shell));
        }
    };

    // Always evaluate CUE to get current config (not a bottleneck)
    debug!("Evaluating CUE for {}", directory.display());
    let evaluator = CueEvaluator::builder().build()?;
    let config: Cuenv = evaluator.evaluate_typed(&directory, package)?;

    // Load approval manager and check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    debug!("Checking approval for directory: {}", directory.display());
    // Convert Cuenv to Value for approval check (temporary until approval system is updated)
    let config_value = serde_json::to_value(&config).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize config: {e}"))
    })?;
    let approval_status = check_approval_status(&approval_manager, &directory, &config_value)?;

    match approval_status {
        ApprovalStatus::NotApproved { .. } | ApprovalStatus::RequiresApproval { .. } => {
            let summary = ConfigSummary::from_json(&config_value);
            // Only require approval if there are hooks
            if summary.has_hooks {
                // Return a comment that tells user to approve
                return Ok(format_not_allowed(&directory, shell, summary.hook_count));
            }
            debug!("Auto-approving configuration with no hooks");
        }
        ApprovalStatus::Approved => {
            // Continue with loading
        }
    }

    // Compute config hash for this directory + config
    let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config_value);

    // Check if state is ready
    let executor = HookExecutor::with_default_config()?;
    if let Some(state) = executor
        .get_execution_status_for_instance(&directory, &config_hash)
        .await?
    {
        match state.status {
            ExecutionStatus::Completed => {
                // Environment is ready - format and return with diff support
                let env_vars = collect_all_env_vars(&config, &state.environment_vars);
                return Ok(format_env_diff_with_unset(
                    &directory,
                    env_vars,
                    state.previous_env.as_ref(),
                    shell,
                ));
            }
            ExecutionStatus::Failed => {
                // Hooks failed - return safe no-op
                debug!(
                    "Hooks failed for {}: {:?}",
                    directory.display(),
                    state.error_message
                );
                return Ok(format_no_op(shell));
            }
            ExecutionStatus::Running => {
                // Still running - wait briefly
                debug!("Hooks still running for {}", directory.display());
            }
            ExecutionStatus::Cancelled => {
                // Cancelled - return safe no-op
                return Ok(format_no_op(shell));
            }
        }
    } else {
        // No state - need to start supervisor
        info!("Starting hook execution for {}", directory.display());

        // Extract hooks from config
        let hooks = extract_hooks_from_config(&config);
        if !hooks.is_empty() {
            executor
                .execute_hooks_background(directory.clone(), config_hash.clone(), hooks)
                .await?;
        }
    }

    // Wait briefly for fast hooks (10ms)
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Check again
    if let Some(state) = executor
        .get_execution_status_for_instance(&directory, &config_hash)
        .await?
        && state.status == ExecutionStatus::Completed
    {
        let env_vars = collect_all_env_vars(&config, &state.environment_vars);
        return Ok(format_env_diff_with_unset(
            &directory,
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
        return Ok(format_env_diff(&directory, static_env, shell));
    }

    // No environment available yet - return safe no-op
    Ok(format_no_op(shell))
}

/// Extract hooks from CUE config
fn extract_hooks_from_config(config: &Cuenv) -> Vec<cuenv_core::hooks::types::Hook> {
    config.on_enter_hooks()
}

/// Extract static environment variables from CUE config
fn extract_static_env_vars(config: &Cuenv) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    if let Some(env) = &config.env {
        for (key, value) in &env.base {
            // Use to_string_value for consistent handling
            let value_str = value.to_string_value();
            if value_str == "[SECRET]" {
                // Skip secrets in export
                continue;
            }
            env_vars.insert(key.clone(), value_str);
        }
    }

    env_vars
}

/// Collect all environment variables (static + hook-generated)
fn collect_all_env_vars(
    config: &Cuenv,
    hook_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut all_vars = extract_static_env_vars(config);

    // Hook environment variables override static ones
    for (key, value) in hook_env {
        all_vars.insert(key.clone(), value.clone());
    }

    all_vars
}

/// Get environment variables with hook-generated vars merged in
///
/// This function checks if hooks have completed and merges their environment
/// with the static environment from the CUE manifest. This is used by
/// `cuenv task` and `cuenv exec` to ensure they have access to hook-generated
/// environment variables.
///
/// This function ensures hooks are running and waits for their completion.
pub async fn get_environment_with_hooks(
    directory: &Path,
    config: &Cuenv,
) -> Result<HashMap<String, String>> {
    // Start with static environment from CUE manifest
    let static_env = extract_static_env_vars(config);

    // Compute config hash for this directory + config
    let config_value = serde_json::to_value(config).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize config: {e}"))
    })?;
    let config_hash = cuenv_core::hooks::approval::compute_config_hash(&config_value);

    let executor = HookExecutor::with_default_config()?;

    // Check if state exists
    let status = executor
        .get_execution_status_for_instance(directory, &config_hash)
        .await?;

    // If no state exists, start execution
    if status.is_none() {
        let hooks = extract_hooks_from_config(config);
        if hooks.is_empty() {
            // No hooks to run, just return static env
            return Ok(static_env);
        }

        info!("Starting hook execution for {}", directory.display());
        executor
            .execute_hooks_background(directory.to_path_buf(), config_hash.clone(), hooks)
            .await?;
    }

    // Wait for completion (timeout 60s for now, could be configurable)
    debug!("Waiting for hooks to complete for {}", directory.display());
    match executor
        .wait_for_completion(directory, &config_hash, Some(60))
        .await
    {
        Ok(state) => {
            match state.status {
                ExecutionStatus::Completed => {
                    // Hooks completed - merge their environment with static env
                    Ok(collect_all_env_vars(config, &state.environment_vars))
                }
                ExecutionStatus::Failed => {
                    // Hooks failed - log but still try to use any environment variables captured
                    // This is critical for tools that print exports before crashing
                    debug!(
                        "Hooks failed for {}: {:?}. Using captured environment.",
                        directory.display(),
                        state.error_message
                    );
                    Ok(collect_all_env_vars(config, &state.environment_vars))
                }
                ExecutionStatus::Cancelled => {
                    // Hooks cancelled - use static environment only
                    debug!("Hooks cancelled for {}", directory.display());
                    Ok(static_env)
                }
                ExecutionStatus::Running => Ok(static_env),
            }
        }
        Err(e) => {
            tracing::warn!(
                "Error or timeout waiting for hooks: {}, using static environment",
                e
            );
            Ok(static_env)
        }
    }
}

/// Generate script to print "Loaded" message if entering a new directory
#[allow(clippy::uninlined_format_args)]
fn format_loaded_check(dir: &Path, shell: Shell) -> String {
    let dir_display = dir.to_string_lossy();
    let escaped_dir = escape_shell_value(&dir_display);

    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r#"if [ "${{{loaded}:-}}" != "{escaped_dir}" ]; then
    echo "Cuenv environment loaded" >&2
    export {loaded}="{escaped_dir}"
    unset {pending} 2>/dev/null
fi"#,
            loaded = LOADED_DIR_ENV,
            pending = PENDING_APPROVAL_ENV,
            escaped_dir = escaped_dir,
        ),
        Shell::Fish => format!(
            r#"if not set -q {loaded}
    echo "Cuenv environment loaded" >&2
    set -x {loaded} "{escaped_dir}"
    set -e {pending} 2>/dev/null
else if test "${loaded}" != "{escaped_dir}"
    echo "Cuenv environment loaded" >&2
    set -x {loaded} "{escaped_dir}"
    set -e {pending} 2>/dev/null
end"#,
            loaded = LOADED_DIR_ENV,
            pending = PENDING_APPROVAL_ENV,
            escaped_dir = escaped_dir,
        ),
        Shell::PowerShell => {
            let ps_dir = dir_display.replace('\'', "''");
            format!(
                r"if ($env:{loaded} -ne '{ps_dir}') {{
    Write-Host 'Cuenv environment loaded'
    $env:{loaded} = '{ps_dir}'
    Remove-Item Env:{pending} -ErrorAction SilentlyContinue
}}",
                loaded = LOADED_DIR_ENV,
                pending = PENDING_APPROVAL_ENV,
                ps_dir = ps_dir,
            )
        }
    }
}

/// Format environment variables as shell export commands
fn format_env_diff(dir: &Path, env: HashMap<String, String>, shell: Shell) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    // Add loaded check/message
    output.push_str(&format_loaded_check(dir, shell));
    output.push('\n');

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
    dir: &Path,
    current_env: HashMap<String, String>,
    previous_env: Option<&HashMap<String, String>>,
    shell: Shell,
) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    // Add loaded check/message
    output.push_str(&format_loaded_check(dir, shell));
    output.push('\n');

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

/// Format "not allowed" message as an executable script snippet per shell.
/// The script prints a helpful notice once per directory until approval is granted.
#[allow(clippy::uninlined_format_args)]
fn format_not_allowed(dir: &Path, shell: Shell, hook_count: usize) -> String {
    let dir_display = dir.to_string_lossy();
    let escaped = escape_shell_value(&dir_display);
    let hooks_str = if hook_count == 1 { "hook" } else { "hooks" };

    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r#"if [ "${{{pending}:-}}" != "{dir}" ]; then
    printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    printf '%s\n' "Run 'cuenv allow' to approve."
    export {pending}="{dir}"
    unset {loaded} 2>/dev/null
fi
:"#,
            pending = PENDING_APPROVAL_ENV,
            loaded = LOADED_DIR_ENV,
            dir = escaped,
            count = hook_count,
            hooks = hooks_str,
        ),
        Shell::Fish => {
            let pending_ref = format!("${PENDING_APPROVAL_ENV}");
            format!(
                r#"if not set -q {pending}
    printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    printf '%s\n' "Run 'cuenv allow' to approve."
    set -x {pending} "{dir}"
    set -e {loaded} 2>/dev/null
else if test "{pending_ref}" != "{dir}"
    printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    printf '%s\n' "Run 'cuenv allow' to approve."
    set -x {pending} "{dir}"
    set -e {loaded} 2>/dev/null
end
true"#,
                pending = PENDING_APPROVAL_ENV,
                pending_ref = pending_ref,
                loaded = LOADED_DIR_ENV,
                dir = escaped,
                count = hook_count,
                hooks = hooks_str,
            )
        }
        Shell::PowerShell => {
            let ps_dir = dir_display.replace('\'', "''");
            format!(
                r"if ($env:{pending} -ne '{dir}') {{
    Write-Host 'cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}.'
    Write-Host 'Run ''cuenv allow'' to approve.'
    $env:{pending} = '{dir}'
    Remove-Item Env:{loaded} -ErrorAction SilentlyContinue
}}",
                pending = PENDING_APPROVAL_ENV,
                loaded = LOADED_DIR_ENV,
                dir = ps_dir,
                count = hook_count,
                hooks = hooks_str,
            )
        }
    }
}

/// Escape special characters in shell values
fn escape_shell_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

/// Format a safe no-op command for the shell while clearing pending approval notices
#[allow(clippy::uninlined_format_args)]
fn format_no_op(shell: Shell) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r"unset {pending} {loaded} 2>/dev/null
:",
            pending = PENDING_APPROVAL_ENV,
            loaded = LOADED_DIR_ENV,
        ),
        Shell::Fish => format!(
            r"if set -q {pending}
    set -e {pending}
end
if set -q {loaded}
    set -e {loaded}
end
true",
            pending = PENDING_APPROVAL_ENV,
            loaded = LOADED_DIR_ENV,
        ),
        Shell::PowerShell => format!(
            r"if (Test-Path Env:{pending}) {{ Remove-Item Env:{pending} }}
if (Test-Path Env:{loaded}) {{ Remove-Item Env:{loaded} }}
# no changes",
            pending = PENDING_APPROVAL_ENV,
            loaded = LOADED_DIR_ENV,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::environment::{Env, EnvValue, EnvValueSimple, EnvVarWithPolicies};
    use cuenv_core::manifest::Cuenv;
    use cuenv_core::secrets::Secret;
    use std::collections::HashMap;
    use std::path::Path;

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
    fn test_format_no_op_clears_state() {
        let bash = format_no_op(Shell::Bash);
        assert!(bash.contains("unset"));
        assert!(bash.contains("CUENV_PENDING_APPROVAL_DIR"));
        assert!(bash.contains("CUENV_LOADED_DIR"));
        assert!(bash.trim().ends_with(':'));

        let fish = format_no_op(Shell::Fish);
        assert!(fish.contains("set -e CUENV_PENDING_APPROVAL_DIR"));
        assert!(fish.contains("set -e CUENV_LOADED_DIR"));
        assert!(fish.trim().ends_with("true"));

        let zsh = format_no_op(Shell::Zsh);
        assert!(zsh.contains("unset"));
        assert!(zsh.contains("CUENV_PENDING_APPROVAL_DIR"));
        assert!(zsh.contains("CUENV_LOADED_DIR"));

        let pwsh = format_no_op(Shell::PowerShell);
        assert!(pwsh.contains("Remove-Item Env:CUENV_PENDING_APPROVAL_DIR"));
        assert!(pwsh.contains("Remove-Item Env:CUENV_LOADED_DIR"));
    }

    #[test]
    fn test_format_not_allowed_emits_notice_and_clears_loaded() {
        let dir = Path::new("/tmp/project");
        let bash_notice = format_not_allowed(dir, Shell::Bash, 1);
        assert!(bash_notice.contains("cuenv detected env.cue"));
        assert!(bash_notice.contains("cuenv allow'")); // Simplified command
        assert!(bash_notice.contains("contains 1 hook"));
        assert!(bash_notice.contains("export CUENV_PENDING_APPROVAL_DIR="));
        assert!(bash_notice.contains("unset CUENV_LOADED_DIR"));

        let fish_notice = format_not_allowed(dir, Shell::Fish, 2);
        assert!(fish_notice.contains("set -x CUENV_PENDING_APPROVAL_DIR"));
        assert!(fish_notice.contains("cuenv detected env.cue"));
        assert!(fish_notice.contains("contains 2 hooks"));
        assert!(fish_notice.contains("set -e CUENV_LOADED_DIR"));
    }

    #[test]
    fn test_format_env_diff_exports_and_loaded_message() {
        let dir = Path::new("/tmp/project");
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar baz".to_string());
        env.insert("NUM".to_string(), "42".to_string());

        let bash = format_env_diff(dir, env.clone(), Shell::Bash);
        assert!(bash.contains("echo \"Cuenv environment loaded\" >&2"));
        assert!(bash.contains("export CUENV_LOADED_DIR=\"/tmp/project\""));
        assert!(bash.contains("export FOO=\"bar baz\""));
        assert!(bash.contains("export NUM=\"42\""));

        let zsh = format_env_diff(dir, env.clone(), Shell::Zsh);
        assert!(zsh.contains("echo \"Cuenv environment loaded\" >&2"));
        assert!(zsh.contains("export FOO=\"bar baz\""));

        let fish = format_env_diff(dir, env.clone(), Shell::Fish);
        assert!(fish.contains("echo \"Cuenv environment loaded\" >&2"));
        assert!(fish.contains("set -x CUENV_LOADED_DIR \"/tmp/project\""));
        assert!(fish.contains("set -x FOO \"bar baz\""));

        let pwsh = format_env_diff(dir, env, Shell::PowerShell);
        assert!(pwsh.contains("Write-Host 'Cuenv environment loaded'"));
        assert!(pwsh.contains("$env:CUENV_LOADED_DIR = '/tmp/project'"));
        assert!(pwsh.contains("$env:FOO = \"bar baz\""));
    }

    #[test]
    fn test_format_env_diff_with_unset() {
        let dir = Path::new("/tmp/project");
        let current = HashMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]);
        let previous = HashMap::from([
            ("A".to_string(), "old".to_string()),
            ("REMOVED".to_string(), "x".to_string()),
        ]);

        let out_bash =
            format_env_diff_with_unset(dir, current.clone(), Some(&previous), Shell::Bash);
        assert!(out_bash.lines().any(|l| l == "unset REMOVED"));
        assert!(out_bash.contains("export A=\"1\""));
        assert!(out_bash.contains("echo \"Cuenv environment loaded\""));

        let out_fish =
            format_env_diff_with_unset(dir, current.clone(), Some(&previous), Shell::Fish);
        assert!(out_fish.lines().any(|l| l == "set -e REMOVED"));

        let out_pwsh = format_env_diff_with_unset(dir, current, Some(&previous), Shell::PowerShell);
        assert!(out_pwsh.lines().any(|l| l == "Remove-Item Env:REMOVED"));
    }

    #[test]
    fn test_extract_static_env_vars_skips_secrets() {
        // Build Cuenv with one normal var and one secret
        let mut base = HashMap::new();
        base.insert("PLAIN".to_string(), EnvValue::String("value".to_string()));
        let secret = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
        base.insert("SECRET".to_string(), EnvValue::Secret(secret));

        let env_cfg = Env {
            base,
            environment: None,
        };
        let cfg = Cuenv {
            config: None,
            env: Some(env_cfg),
            hooks: None,
            workspaces: None,
            tasks: HashMap::new(),
        };

        let vars = extract_static_env_vars(&cfg);
        assert!(vars.get("PLAIN") == Some(&"value".to_string()));
        assert!(!vars.contains_key("SECRET"));
    }

    #[test]
    fn test_collect_all_env_vars_override() {
        let mut base = HashMap::new();
        base.insert("OVERRIDE".to_string(), EnvValue::String("base".to_string()));
        base.insert(
            "BASE_ONLY".to_string(),
            EnvValue::WithPolicies(EnvVarWithPolicies {
                value: EnvValueSimple::String("plain".to_string()),
                policies: None,
            }),
        );

        let cfg = Cuenv {
            config: None,
            env: Some(Env {
                base,
                environment: None,
            }),
            hooks: None,
            workspaces: None,
            tasks: HashMap::new(),
        };

        let hook_env = HashMap::from([
            ("OVERRIDE".to_string(), "hook".to_string()),
            ("HOOK_ONLY".to_string(), "x".to_string()),
        ]);

        let merged = collect_all_env_vars(&cfg, &hook_env);
        assert_eq!(merged.get("OVERRIDE"), Some(&"hook".to_string()));
        assert_eq!(merged.get("BASE_ONLY"), Some(&"plain".to_string()));
        assert_eq!(merged.get("HOOK_ONLY"), Some(&"x".to_string()));
    }
}
