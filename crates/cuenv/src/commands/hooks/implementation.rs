//! Hook lifecycle and environment commands.

use super::{recovery, shell, status};
use crate::cli::StatusFormat;
use crate::commands::env_file::{self, EnvFileStatus};
use crate::commands::{CommandExecutor, convert_engine_error};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::module::{ModuleEvaluationOptions, TASK_FIELD};
use cuenv_core::{ModuleEvaluation, Result};
use cuenv_hooks::{
    ApprovalManager, ApprovalStatus, ConfigSummary, ExecutionStatus, Hook, HookExecutionState,
    HookExecutor, HookSessionContext,
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::info;

pub(in crate::commands) fn optional_hook_session() -> Result<Option<uuid::Uuid>> {
    let value = match std::env::var("CUENV_HOOK_SESSION") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(cuenv_core::Error::configuration("Invalid hook session ID"));
        }
    };
    let session = uuid::Uuid::parse_str(&value)
        .map_err(|_| cuenv_core::Error::configuration("Invalid hook session ID"))?;
    if session.get_version_num() != 4 || session.to_string() != value {
        return Err(cuenv_core::Error::configuration("Invalid hook session ID"));
    }
    Ok(Some(session))
}

/// Read the explicitly selected hook session.
///
/// # Errors
///
/// Returns an error if the session is missing or is not a canonical version-4 UUID.
pub fn required_hook_session() -> Result<uuid::Uuid> {
    optional_hook_session()?.ok_or_else(|| {
        cuenv_core::Error::configuration(
            "No hook session is active; initialize shell integration or set CUENV_HOOK_SESSION from 'cuenv shell session'",
        )
    })
}

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
    let target_path = directory.canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path("canonicalize path", directory.to_path_buf(), e)
    })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.module(&target_path)?;
        let rel_path = crate::commands::relative_path_from_root(&module.root, &target_path);

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

    let module_root = env_file::find_cue_module_root(&target_path).ok_or_else(|| {
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
        task_field: Some(TASK_FIELD.to_string()),
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        ModuleEvaluationOptions {
            project_paths: raw_result.projects,
            references: None,
        },
    );

    let rel_path = crate::commands::relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    instance.deserialize()
}

fn get_execution_hash(
    directory: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    let config = evaluate_config(directory, package, executor)?;
    Ok(cuenv_hooks::compute_execution_hash(
        &extract_hooks_from_config(&config),
        directory,
    )?)
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
        cuenv_hooks::check_approval_status(&approval_manager, &directory, config.hooks.as_ref());

    match approval_status {
        ApprovalStatus::Approved => {
            // Extract hooks from configuration and execute
            let hooks = extract_hooks_from_config(&config);

            if hooks.is_empty() {
                return Ok("No hooks to execute".to_string());
            }

            // Start background execution
            let executor = HookExecutor::with_default_config()?;
            let config_hash = cuenv_hooks::compute_execution_hash(&hooks, &directory)?;
            let session_id = required_hook_session()?;

            let result = executor
                .execute_hooks_background(
                    HookSessionContext {
                        directory: &directory,
                        config_hash: &config_hash,
                        session_id,
                    },
                    hooks,
                    cuenv_hooks::HookStartMode::Force,
                )
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

/// Whether status should wait for hook completion.
#[derive(Debug, Clone, Copy)]
pub enum StatusWait {
    /// Return the current status immediately.
    Immediate,
    /// Wait up to the given number of seconds for completion.
    UntilComplete {
        /// Maximum time to wait for completion.
        timeout_seconds: u64,
    },
}

/// Output and evaluation options for the hook status command.
#[derive(Clone, Copy)]
pub struct EnvStatusOptions<'options> {
    /// Wait policy.
    pub wait: StatusWait,
    /// Status output format.
    pub format: StatusFormat,
    /// Cached evaluator, when available.
    pub executor: Option<&'options CommandExecutor>,
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
    options: EnvStatusOptions<'_>,
) -> Result<String> {
    let EnvStatusOptions {
        wait,
        format,
        executor: cmd_executor,
    } = options;
    // Check env.cue and canonicalize path
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(dir) => dir,
        status => return Ok(env_file_issue_message(path, package, status)),
    };

    let hook_executor = HookExecutor::with_default_config()?;
    let session_id = required_hook_session()?;

    if let StatusWait::UntilComplete { timeout_seconds } = wait {
        // Follow the observed run even when a hook changes one of its inputs.
        let observed =
            if let Some(state) = hook_executor.fast_status(&directory, session_id).await? {
                Some(state)
            } else {
                let config_hash = get_execution_hash(&directory, package, cmd_executor)?;
                hook_executor
                    .execution_status_for_instance(HookSessionContext {
                        directory: &directory,
                        config_hash: &config_hash,
                        session_id,
                    })
                    .await?
            };
        let Some(observed) = observed else {
            return Ok("No hook execution in progress".to_string());
        };
        match wait_for_observed_run(&hook_executor, observed.clone(), timeout_seconds).await {
            Ok(state) => Ok(status::format_status(&state, format)),
            Err(cuenv_hooks::Error::Timeout { .. }) => {
                if let Some(state) = hook_executor
                    .state_manager()
                    .state_for_instance(&observed.directory_path, &observed.config_hash, session_id)
                    .await?
                    && state.run_id == observed.run_id
                {
                    Ok(format!(
                        "Timeout after {} seconds. Current status: {}",
                        timeout_seconds,
                        status::format_status(&state, format)
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
        if let Some(state) = hook_executor.fast_status(&directory, session_id).await? {
            Ok(status::format_status(&state, format))
        } else {
            match format {
                StatusFormat::Text => Ok("No hook execution in progress".to_string()),
                StatusFormat::Short => Ok("-".to_string()),
                StatusFormat::Starship => Ok(String::new()), // Empty for starship if nothing happening
            }
        }
    }
}

async fn wait_for_observed_run(
    executor: &HookExecutor,
    mut state: HookExecutionState,
    timeout_seconds: u64,
) -> cuenv_hooks::Result<HookExecutionState> {
    let identity = state.run_id;
    let instance = state.instance_hash.clone();
    let directory = state.directory_path.clone();
    let config_hash = state.config_hash.clone();
    let session_id = state.session_id;
    let wait = async {
        while !state.is_complete() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            state = executor
                .state_manager()
                .state_for_instance(&directory, &config_hash, session_id)
                .await?
                .ok_or_else(|| cuenv_hooks::Error::state_not_found(&instance))?;
            if state.run_id != identity {
                return Err(cuenv_hooks::Error::process(
                    "Hook run was superseded while waiting",
                ));
            }
        }
        Ok(state)
    };
    tokio::time::timeout(std::time::Duration::from_secs(timeout_seconds), wait)
        .await
        .map_err(|_| cuenv_hooks::Error::Timeout {
            seconds: timeout_seconds,
        })?
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
    let session_id = required_hook_session()?;

    // FAST PATH: Skip config hash computation, use directory-based marker lookup.
    // This runs synchronously without tokio.
    if let Some(state) = executor.fast_status_sync(&directory, session_id)? {
        Ok(status::format_status(&state, format))
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

    let config_hash = get_execution_hash(&directory, package, executor)?;
    let session_id = required_hook_session()?;

    // Locate state file
    let instance_hash = cuenv_hooks::compute_instance_hash(&directory, &config_hash, session_id);
    let state_manager = recovery::state_manager()?;
    let state_path = state_manager.try_state_file_path(&instance_hash)?;

    // Try to load exact state
    if let Some(state) = state_manager
        .state_for_instance(&directory, &config_hash, session_id)
        .await?
    {
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

        writeln!(
            &mut output,
            "Captured env: {} variables",
            state.environment_vars.len()
        )
        .ok();

        return Ok(output);
    }

    // No exact state found; gather any other states for this directory for debugging
    let mut matching_states = Vec::new();
    for state in state_manager.list_active_states().await? {
        if state.directory_path == directory && state.session_id == session_id {
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
                    .try_state_file_path(&state.instance_hash)?
                    .display()
            )
            .ok();
        }
    }

    Ok(output)
}

/// How to confirm a configuration approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalConfirmation {
    /// Ask before allowing hooks.
    Prompt,
    /// Approve without asking.
    AssumeYes,
}

/// Approval and evaluation options for `allow`.
pub struct AllowOptions<'options> {
    /// Optional approval note.
    pub note: Option<String>,
    /// Confirmation policy.
    pub confirmation: ApprovalConfirmation,
    /// Cached evaluator, when available.
    pub executor: Option<&'options CommandExecutor>,
}

/// Execute allow command - approve current directory's configuration
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if env file lookup, CUE evaluation, or approval management fails.
pub async fn execute_allow(path: &str, package: &str, options: AllowOptions<'_>) -> Result<String> {
    let AllowOptions {
        note,
        confirmation,
        executor,
    } = options;
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
    if approval_manager.is_approved(&directory, &config_hash) {
        return Ok(format!(
            "Configuration is already approved for directory: {}",
            directory.display()
        ));
    }

    // Show what we're approving
    let summary = ConfigSummary::from_hooks(config.hooks.as_ref());

    // If we need confirmation and yes flag is not set
    if confirmation == ApprovalConfirmation::Prompt {
        let hooks = extract_hooks_from_config(&config);
        if !hooks.is_empty() {
            cuenv_events::println_redacted("The following hooks will be allowed:");
            for hook in &hooks {
                cuenv_events::println_redacted(&format!("  - Command: {}", hook.command));
                if !hook.args.is_empty() {
                    cuenv_events::println_redacted(&format!("    Args: {:?}", hook.args));
                }
            }
            cuenv_events::println_redacted("");
            cuenv_events::print_redacted("Do you want to allow this configuration? [y/N] ");
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
pub async fn execute_deny(path: &str, package: &str) -> Result<String> {
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

/// Shell and evaluator selection for the environment check.
#[derive(Clone, Copy)]
pub struct EnvCheckOptions<'options> {
    /// Target shell.
    pub shell: crate::cli::ShellType,
    /// Cached evaluator, when available.
    pub executor: Option<&'options CommandExecutor>,
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
    options: EnvCheckOptions<'_>,
) -> Result<String> {
    let EnvCheckOptions {
        shell,
        executor: cmd_executor,
    } = options;
    let shell = match shell {
        crate::cli::ShellType::Fish => cuenv_core::shell::Shell::Fish,
        crate::cli::ShellType::Bash => cuenv_core::shell::Shell::Bash,
        crate::cli::ShellType::Zsh => cuenv_core::shell::Shell::Zsh,
    };
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(directory) => Some(directory),
        EnvFileStatus::Missing | EnvFileStatus::PackageMismatch { .. } => None,
    };
    if let Some(output) =
        crate::commands::export::preflight_shell_transition(directory.as_deref(), package, shell)?
    {
        return Ok(output);
    }
    let Some(directory) = directory else {
        return Ok(String::new());
    };
    let config = evaluate_config(&directory, package, cmd_executor)?;
    let hooks = extract_hooks_from_config(&config);
    let environment = crate::commands::export::EnvironmentContext {
        directory: &directory,
        package,
        shell,
    };
    let static_env = crate::commands::export::shell_env_vars(&config, &directory)?;
    if hooks.is_empty() {
        return crate::commands::export::format_environment(environment, &static_env);
    }
    let mut approval = ApprovalManager::with_default_file()?;
    approval.load_approvals().await?;
    if !matches!(
        cuenv_hooks::check_approval_status(&approval, &directory, config.hooks.as_ref()),
        ApprovalStatus::Approved
    ) {
        return Err(cuenv_core::Error::configuration(
            "Hook configuration requires approval before its environment can be used",
        ));
    }
    let config_hash = cuenv_hooks::compute_execution_hash(&hooks, &directory)?;
    let session_id = required_hook_session()?;

    let hook_executor = HookExecutor::with_default_config()?;

    // Check execution status using the specific instance
    if let Some(state) = hook_executor
        .execution_status_for_instance(HookSessionContext {
            directory: &directory,
            config_hash: &config_hash,
            session_id,
        })
        .await?
        && state.is_complete()
        && state.status == ExecutionStatus::Completed
    {
        let mut all_env_vars = static_env;
        for (key, value) in &state.environment_vars {
            all_env_vars.insert(key.clone(), value.clone());
        }
        return crate::commands::export::format_environment(environment, &all_env_vars);
    }

    // No environment to load
    Ok(String::new())
}

/// Generate shell integration script
#[must_use]
pub fn execute_shell_init(shell: crate::cli::ShellType) -> String {
    shell::generate_integration(shell)
}

/// Extract hooks from the configuration JSON
fn extract_hooks_from_config(config: &Project) -> Vec<Hook> {
    config.on_enter_hooks()
}

#[cfg(test)]
#[path = "../hooks_tests.rs"]
mod tests;
