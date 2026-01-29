//! Hook-related command implementations

use super::env_file::{self, EnvFileStatus, find_cue_module_root};
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use crate::cli::StatusFormat;
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result};
use cuenv_hooks::{
    ApprovalManager, ApprovalStatus, ConfigSummary, ExecutionStatus, Hook, HookExecutionState,
    HookExecutor, StateManager, check_approval_status, compute_instance_hash,
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::info;

fn env_file_issue_message(path: &str, package: &str, status: EnvFileStatus) -> String {
    match status {
        EnvFileStatus::Missing => format!("No env.cue file found in '{path}'"),
        EnvFileStatus::PackageMismatch { found_package } => match found_package {
            Some(found) => {
                format!("env.cue in '{path}' uses package '{found}', expected '{package}'")
            }
            None => format!(
                "env.cue in '{path}' is missing a package declaration (expected '{package}')"
            ),
        },
        EnvFileStatus::Match(_) => {
            unreachable!("env_file_issue_message should not be called with a match")
        }
    }
}

fn require_env_file(path: &Path, package: &str) -> Result<PathBuf> {
    match env_file::find_env_file(path, package)? {
        EnvFileStatus::Match(dir) => Ok(dir),
        EnvFileStatus::Missing => Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in '{}'",
            path.display()
        ))),
        EnvFileStatus::PackageMismatch { found_package } => {
            let message = match found_package {
                Some(found) => format!(
                    "env.cue in '{}' uses package '{found}', expected '{package}'",
                    path.display()
                ),
                None => format!(
                    "env.cue in '{}' is missing a package declaration (expected '{package}')",
                    path.display()
                ),
            };
            Err(cuenv_core::Error::configuration(message))
        }
    }
}

/// Helper to evaluate CUE configuration using module-wide evaluation.
///
/// This function loads the entire CUE module once and extracts the Project
/// configuration at the specified directory path.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
fn evaluate_config(
    directory: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Project> {
    let target_path = directory
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(directory.to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let rel_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&rel_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                rel_path.display()
            ))
        })?;

        return instance.deserialize();
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    // Use non-recursive evaluation since hooks only need the current project's config,
    // not cross-project references.
    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
        None,
    );

    let rel_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    instance.deserialize()
}

/// Helper to get config hash from approval manager or compute it
fn get_config_hash(
    directory: &Path,
    package: &str,
    approval_manager: &ApprovalManager,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    if let Some(approval) = approval_manager.get_approval(directory.to_str().unwrap_or("")) {
        Ok(approval.config_hash.clone())
    } else {
        // If not approved, compute it from current config
        let config = evaluate_config(directory, package, executor)?;
        Ok(cuenv_hooks::compute_approval_hash(config.hooks.as_ref()))
    }
}

/// Format status based on requested format
fn format_status(state: &HookExecutionState, format: StatusFormat) -> String {
    match format {
        StatusFormat::Text => state.progress_display(),
        StatusFormat::Short => match state.status {
            ExecutionStatus::Running => {
                format!("[{}/{}]", state.completed_hooks, state.total_hooks)
            }
            ExecutionStatus::Completed => "[OK]".to_string(),
            ExecutionStatus::Failed => "[ERR]".to_string(),
            ExecutionStatus::Cancelled => "[X]".to_string(),
        },
        StatusFormat::Starship => format_starship_status(state),
    }
}

/// Format status for starship integration with rich information
fn format_starship_status(state: &HookExecutionState) -> String {
    use cuenv_hooks::HookExecutionState;

    match state.status {
        ExecutionStatus::Running => {
            // Show current hook name + duration
            if let Some(hook_display) = state.current_hook_display() {
                if let Some(duration) = state.current_hook_duration() {
                    let duration_str = HookExecutionState::format_duration(duration);
                    format!("cuenv hook {hook_display} ({duration_str})")
                } else {
                    // Just started, no duration yet - use overall execution time
                    let duration = state.duration();
                    let duration_str = HookExecutionState::format_duration(duration);
                    format!("cuenv hook {hook_display} ({duration_str})")
                }
            } else {
                // Fallback if no current hook (shouldn't happen in Running state)
                format!("🔄 {}/{}", state.completed_hooks, state.total_hooks)
            }
        }
        ExecutionStatus::Completed => {
            // Only show if within display timeout (ensures at least one display)
            if state.should_display_completed() {
                let duration = state.duration();
                let duration_str = HookExecutionState::format_duration(duration);
                format!("✅ {duration_str}")
            } else {
                // State has expired, return empty string to hide from prompt
                String::new()
            }
        }
        ExecutionStatus::Failed => {
            // Show failed state with error if within display timeout
            if state.should_display_completed() {
                if let Some(error_msg) = &state.error_message {
                    // Extract just the command name from error if possible
                    format!("❌ {}", error_msg.lines().next().unwrap_or("failed"))
                } else {
                    "❌ failed".to_string()
                }
            } else {
                String::new()
            }
        }
        ExecutionStatus::Cancelled => {
            if state.should_display_completed() {
                "🚫 cancelled".to_string()
            } else {
                String::new()
            }
        }
    }
}

/// Execute env load command - evaluates config, checks approval, starts hook execution
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, CUE evaluation, or hook execution fails.
pub async fn execute_env_load(
    path: &str,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    // Check env.cue and canonicalize path
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(dir) => dir,
        status => return Ok(env_file_issue_message(path, package, status)),
    };

    // Evaluate the CUE configuration
    let config = evaluate_config(&directory, package, executor)?;

    // Check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    let approval_status =
        check_approval_status(&approval_manager, &directory, config.hooks.as_ref())?;

    match approval_status {
        ApprovalStatus::Approved => {
            // Extract hooks from configuration and execute
            let hooks = extract_hooks_from_config(&config);

            if hooks.is_empty() {
                return Ok("No hooks to execute".to_string());
            }

            // Start background execution
            let executor = HookExecutor::with_default_config()?;
            let config_hash = cuenv_hooks::compute_approval_hash(config.hooks.as_ref());

            let result = executor
                .execute_hooks_background(directory.clone(), config_hash, hooks)
                .await?;

            Ok(result)
        }
        ApprovalStatus::RequiresApproval { current_hash } => {
            let summary = ConfigSummary::from_hooks(config.hooks.as_ref());
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
            let summary = ConfigSummary::from_hooks(config.hooks.as_ref());
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
///
/// Uses a fast path for non-wait mode that skips config hash computation entirely.
/// This reduces latency from ~300ms to <20ms for Starship integration.
///
/// When a `cmd_executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, hook executor initialization, or status retrieval fails.
pub async fn execute_env_status(
    path: &str,
    package: &str,
    wait: bool,
    timeout_seconds: u64,
    format: StatusFormat,
    cmd_executor: Option<&CommandExecutor>,
) -> Result<String> {
    // Check env.cue and canonicalize path
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(dir) => dir,
        status => return Ok(env_file_issue_message(path, package, status)),
    };

    let hook_executor = HookExecutor::with_default_config()?;

    if wait {
        // Wait mode needs config hash to verify we're waiting for the correct config
        let mut approval_manager = ApprovalManager::with_default_file()?;
        approval_manager.load_approvals().await?;
        let config_hash = get_config_hash(&directory, package, &approval_manager, cmd_executor)?;

        match hook_executor
            .wait_for_completion(&directory, &config_hash, Some(timeout_seconds))
            .await
        {
            Ok(state) => Ok(format_status(&state, format)),
            Err(cuenv_hooks::Error::Timeout { .. }) => {
                // Timeout occurred, get current status
                if let Some(state) = hook_executor
                    .get_execution_status_for_instance(&directory, &config_hash)
                    .await?
                {
                    Ok(format!(
                        "Timeout after {} seconds. Current status: {}",
                        timeout_seconds,
                        format_status(&state, format)
                    ))
                } else {
                    Ok("No hook execution in progress".to_string())
                }
            }
            Err(e) => Err(e.into()),
        }
    } else {
        // FAST PATH: Skip config hash computation, use directory-based marker lookup.
        // This reduces latency from ~300ms to <20ms for Starship integration.
        if let Some(state) = hook_executor.get_fast_status(&directory).await? {
            Ok(format_status(&state, format))
        } else {
            match format {
                StatusFormat::Text => Ok("No hook execution in progress".to_string()),
                StatusFormat::Short => Ok("-".to_string()),
                StatusFormat::Starship => Ok(String::new()), // Empty for starship if nothing happening
            }
        }
    }
}

/// Synchronous version of `execute_env_status` for the fast path.
/// This skips the tokio runtime entirely for shell prompt integration.
/// Only supports non-wait mode.
///
/// # Errors
///
/// Returns an error if env file lookup or hook executor operations fail.
pub fn execute_env_status_sync(path: &str, package: &str, format: StatusFormat) -> Result<String> {
    // Check env.cue and canonicalize path
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(dir) => dir,
        status => return Ok(env_file_issue_message(path, package, status)),
    };

    let executor = HookExecutor::with_default_config()?;

    // FAST PATH: Skip config hash computation, use directory-based marker lookup.
    // This runs synchronously without tokio.
    if let Some(state) = executor.get_fast_status_sync(&directory)? {
        Ok(format_status(&state, format))
    } else {
        match format {
            StatusFormat::Text => Ok("No hook execution in progress".to_string()),
            StatusFormat::Short => Ok("-".to_string()),
            StatusFormat::Starship => Ok(String::new()), // Empty for starship if nothing happening
        }
    }
}

/// Inspect cached hook state and captured environment
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, config hash computation, or state loading fails.
pub async fn execute_env_inspect(
    path: &str,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    use std::fmt::Write;
    // Validate env.cue presence and canonicalize
    let directory = require_env_file(Path::new(path), package)?;

    // Compute config hash using the same path approval uses
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;
    let config_hash = get_config_hash(&directory, package, &approval_manager, executor)?;

    // Locate state file
    let instance_hash = compute_instance_hash(&directory, &config_hash);
    let state_manager = StateManager::with_default_dir()?;
    let state_path = state_manager.get_state_file_path(&instance_hash);

    // Try to load exact state
    if let Some(state) = state_manager.load_state(&instance_hash).await? {
        let mut output = String::new();

        writeln!(&mut output, "Directory: {}", directory.display()).ok();
        writeln!(&mut output, "Config hash: {config_hash}").ok();
        writeln!(&mut output, "Instance hash: {instance_hash}").ok();
        writeln!(&mut output, "State file: {}", state_path.display()).ok();
        writeln!(&mut output, "Status: {:?}", state.status).ok();
        writeln!(
            &mut output,
            "Hooks: {}/{}",
            state.completed_hooks, state.total_hooks
        )
        .ok();
        writeln!(&mut output, "Started: {}", state.started_at).ok();
        if let Some(finished) = state.finished_at {
            writeln!(&mut output, "Finished: {finished}").ok();
        }

        // Captured hook environment
        let mut env_keys: Vec<_> = state.environment_vars.keys().collect();
        env_keys.sort();
        writeln!(&mut output, "Captured env ({} vars):", env_keys.len()).ok();
        for key in env_keys {
            if let Some(value) = state.environment_vars.get(key) {
                writeln!(&mut output, "  {key}={value}").ok();
            }
        }

        // Previous environment for diff
        if let Some(prev) = state.previous_env.as_ref() {
            let mut prev_keys: Vec<_> = prev.keys().collect();
            prev_keys.sort();
            writeln!(
                &mut output,
                "Previous env snapshot ({} vars):",
                prev_keys.len()
            )
            .ok();
            for key in prev_keys {
                if let Some(value) = prev.get(key) {
                    writeln!(&mut output, "  {key}={value}").ok();
                }
            }
        }

        return Ok(output);
    }

    // No exact state found; gather any other states for this directory for debugging
    let mut matching_states = Vec::new();
    for state in state_manager.list_active_states().await? {
        if state.directory_path == directory {
            matching_states.push(state);
        }
    }

    let mut output = String::new();
    writeln!(
        &mut output,
        "No cached state found for {} (config hash {}, instance hash {}).",
        directory.display(),
        config_hash,
        instance_hash
    )
    .ok();
    writeln!(&mut output, "Expected state file: {}", state_path.display()).ok();

    if matching_states.is_empty() {
        writeln!(
            &mut output,
            "No other states for this directory were found."
        )
        .ok();
    } else {
        writeln!(
            &mut output,
            "Found {} state(s) for this directory with different config hashes:",
            matching_states.len()
        )
        .ok();
        for state in matching_states {
            writeln!(
                &mut output,
                "  status={:?} config_hash={} state_file={}",
                state.status,
                state.config_hash,
                state_manager
                    .get_state_file_path(&state.instance_hash)
                    .display()
            )
            .ok();
        }
    }

    Ok(output)
}

/// Execute allow command - approve current directory's configuration
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, CUE evaluation, or approval management fails.
pub async fn execute_allow(
    path: &str,
    package: &str,
    note: Option<String>,
    yes: bool,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    // Check env.cue and canonicalize path
    let directory = require_env_file(Path::new(path), package)?;

    // Evaluate the CUE configuration
    let config = evaluate_config(&directory, package, executor)?;

    // Compute configuration hash (only hooks are included for security purposes)
    let config_hash = cuenv_hooks::compute_approval_hash(config.hooks.as_ref());

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
    let summary = ConfigSummary::from_hooks(config.hooks.as_ref());

    // If we need confirmation and yes flag is not set
    if !yes {
        let hooks = extract_hooks_from_config(&config);
        if !hooks.is_empty() {
            #[allow(clippy::print_stdout)] // User-facing approval prompt, intentional display
            {
                println!("The following hooks will be allowed:");
                for hook in &hooks {
                    println!("  - Command: {}", hook.command);
                    if !hook.args.is_empty() {
                        println!("    Args: {:?}", hook.args);
                    }
                }
                println!();
                print!("Do you want to allow this configuration? [y/N] ");
            }
            io::stdout()
                .flush()
                .map_err(|e| cuenv_core::Error::configuration(format!("IO error: {e}")))?;

            let mut input = String::new();
            io::stdin()
                .read_line(&mut input)
                .map_err(|e| cuenv_core::Error::configuration(format!("IO error: {e}")))?;
            if !input.trim().eq_ignore_ascii_case("y") {
                return Ok("Aborted by user.".to_string());
            }
        }
    }

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
         Hash: {}\n\
         (Note: You may need to reload your environment, e.g., `cd .`, to apply changes)",
        directory.display(),
        summary.description(),
        &config_hash[..16]
    ))
}

/// Execute deny command - revoke approval for a directory
///
/// # Errors
///
/// Returns an error if path resolution or approval revocation fails.
pub async fn execute_deny(path: &str, package: &str, _all: bool) -> Result<String> {
    // Resolve directory path, but don't strictly require env.cue to exist
    // (user might want to deny a directory they deleted)
    let directory =
        if let Ok(EnvFileStatus::Match(dir)) = env_file::find_env_file(Path::new(path), package) {
            dir
        } else {
            // If no env file, just use canonical path
            Path::new(path).canonicalize().map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to resolve path '{path}': {e}"))
            })?
        };

    // Initialize approval manager
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    // Revoke approval
    if approval_manager.revoke_approval(&directory).await? {
        Ok(format!(
            "Revoked approval for directory: {}",
            directory.display()
        ))
    } else {
        Ok(format!(
            "No approval found for directory: {}",
            directory.display()
        ))
    }
}

/// Execute env check command - check hook status and output env for shell
///
/// When a `cmd_executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, config hash computation, or status retrieval fails.
pub async fn execute_env_check(
    path: &str,
    package: &str,
    shell: crate::cli::ShellType,
    cmd_executor: Option<&CommandExecutor>,
) -> Result<String> {
    // Check env.cue and canonicalize path - silent return if no env.cue
    let EnvFileStatus::Match(directory) = env_file::find_env_file(Path::new(path), package)? else {
        return Ok(String::new()); // Silent return for non-cuenv directories
    };

    // Get the config hash
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;
    let config_hash = get_config_hash(&directory, package, &approval_manager, cmd_executor)?;

    let hook_executor = HookExecutor::with_default_config()?;

    // Check execution status using the specific instance
    if let Some(state) = hook_executor
        .get_execution_status_for_instance(&directory, &config_hash)
        .await?
        && state.is_complete()
        && state.status == ExecutionStatus::Completed
    {
        let mut output = String::new();
        let mut all_env_vars = HashMap::new();

        // First, get environment variables from CUE configuration
        let config = evaluate_config(&directory, package, cmd_executor)?;

        // Add CUE env variables to our map
        if let Some(env) = &config.env {
            for (key, value) in &env.base {
                // Skip any value that contains secrets
                if value.is_secret() {
                    continue;
                }
                all_env_vars.insert(key.clone(), value.to_string_value());
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
                    writeln!(&mut output, "set -x {key} \"{value}\"").expect("write to string");
                }
                crate::cli::ShellType::Bash | crate::cli::ShellType::Zsh => {
                    use std::fmt::Write;
                    writeln!(&mut output, "export {key}=\"{value}\"").expect("write to string");
                }
            }
        }

        return Ok(output);
    }

    // No environment to load
    Ok(String::new())
}

/// Generate shell integration script
#[must_use]
pub fn execute_shell_init(shell: crate::cli::ShellType) -> String {
    match shell {
        crate::cli::ShellType::Fish => generate_fish_integration(),
        crate::cli::ShellType::Bash => generate_bash_integration(),
        crate::cli::ShellType::Zsh => generate_zsh_integration(),
    }
}

/// Extract hooks from the configuration JSON
fn extract_hooks_from_config(config: &Project) -> Vec<Hook> {
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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_extract_hooks_from_config() {
        use cuenv_core::manifest::Project;
        use cuenv_hooks::{Hook, Hooks};
        use std::collections::HashMap;

        let mut on_enter = HashMap::new();
        on_enter.insert(
            "npm".to_string(),
            Hook {
                order: 100,
                propagate: false,
                command: "npm".to_string(),
                args: vec!["install".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        );
        on_enter.insert(
            "docker".to_string(),
            Hook {
                order: 100,
                propagate: false,
                command: "docker-compose".to_string(),
                args: vec!["up".to_string(), "-d".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        );

        let config = Project {
            config: None,
            env: None,
            hooks: Some(Hooks {
                on_enter: Some(on_enter),
                on_exit: None,
                pre_push: None,
            }),
            ci: None,
            tasks: std::collections::HashMap::new(),
            name: "test".to_string(),
            codegen: None,
            formatters: None,
            runtime: None,
        };

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 2);
        // Sorted alphabetically by name when order is equal
        assert_eq!(hooks[0].command, "docker-compose");
        assert_eq!(hooks[1].command, "npm");
    }

    #[test]
    fn test_extract_hooks_single_hook() {
        use cuenv_core::manifest::Project;
        use cuenv_hooks::{Hook, Hooks};
        use std::collections::HashMap;

        let mut on_enter = HashMap::new();
        on_enter.insert(
            "echo".to_string(),
            Hook {
                order: 100,
                propagate: false,
                command: "echo".to_string(),
                args: vec!["hello".to_string()],
                dir: None,
                inputs: vec![],
                source: None,
            },
        );

        let config = Project {
            config: None,
            env: None,
            hooks: Some(Hooks {
                on_enter: Some(on_enter),
                on_exit: None,
                pre_push: None,
            }),
            ci: None,
            tasks: std::collections::HashMap::new(),
            name: "test".to_string(),
            codegen: None,
            formatters: None,
            runtime: None,
        };

        let hooks = extract_hooks_from_config(&config);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "echo");
        assert_eq!(hooks[0].args, vec!["hello"]);
    }

    #[test]
    fn test_extract_hooks_empty_config() {
        use cuenv_core::manifest::Project;

        let config = Project {
            config: None,
            env: None,
            hooks: None,
            ci: None,
            tasks: std::collections::HashMap::new(),
            name: "test".to_string(),
            codegen: None,
            formatters: None,
            runtime: None,
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
        let result = execute_allow("/nonexistent/directory", "cuenv", None, false, None).await;
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
        let result = execute_allow(
            temp_dir.path().to_str().unwrap(),
            "cuenv",
            None,
            false,
            None,
        )
        .await;
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
        let result = execute_env_load(temp_dir.path().to_str().unwrap(), "cuenv", None).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_execute_env_status_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = execute_env_status(
            temp_dir.path().to_str().unwrap(),
            "cuenv",
            false,
            30,
            StatusFormat::Text,
            None,
        )
        .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_execute_env_load_package_mismatch_message() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("env.cue"), "package other\n\nenv: {}").unwrap();

        let output = execute_env_load(temp_dir.path().to_str().unwrap(), "cuenv", None)
            .await
            .unwrap();
        assert!(output.contains("uses package 'other'"));
    }
}
