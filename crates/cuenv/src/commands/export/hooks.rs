//! Hook-backed environment resolution for export, exec, and task commands.

use super::{
    collect_all_env_vars, evaluate_project, extract_hooks_with_resolved_dirs,
    extract_static_env_vars,
};
use crate::commands::{CommandExecutor, env_file};
use cuenv_core::manifest::Project;
use cuenv_core::{Error, Result};
use cuenv_hooks::{
    ExecutionStatus, HookExecutionConfig, HookExecutionState, HookExecutor, StateManager,
    compute_instance_hash, execute_hooks,
};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info};

struct ForegroundHookRun<'a> {
    directory: &'a Path,
    config_hash: &'a str,
    hooks: Vec<cuenv_hooks::Hook>,
    config: &'a Project,
}

/// Run hooks in the foreground (same process) instead of spawning a detached supervisor.
/// This is useful for CI environments where detached processes may not work correctly.
async fn run_hooks_foreground(request: ForegroundHookRun<'_>) -> Result<HashMap<String, String>> {
    let ForegroundHookRun {
        directory,
        config_hash,
        hooks,
        config,
    } = request;
    let instance_hash = compute_instance_hash(directory, config_hash);

    // Get or create state manager
    let state_dir = if let Ok(dir) = std::env::var("CUENV_STATE_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        StateManager::default_state_dir()?
    };
    let state_manager = StateManager::new(state_dir);

    // Create execution config
    let hook_config = HookExecutionConfig {
        default_timeout_seconds: 600, // 10 minutes for nix print-dev-env
        fail_fast: true,
        state_dir: None,
    };

    // Create initial state
    let mut state = HookExecutionState::new(
        directory.to_path_buf(),
        instance_hash.clone(),
        config_hash.to_string(),
        hooks.clone(),
    );

    // Execute hooks synchronously in this process
    debug!(
        "Executing {} hooks in foreground for {}",
        hooks.len(),
        directory.display()
    );

    execute_hooks(hooks, directory, &hook_config, &state_manager, &mut state).await?;

    // Check result
    match state.status {
        ExecutionStatus::Completed => {
            info!(
                "Foreground hooks completed successfully, captured {} env vars",
                state.environment_vars.len()
            );
            Ok(collect_all_env_vars(config, &state.environment_vars))
        }
        ExecutionStatus::Failed => {
            let msg = state
                .error_message
                .unwrap_or_else(|| "unknown error".to_string());
            Err(Error::execution_with_help(
                format!("Hook execution failed for {}: {msg}", directory.display()),
                "Check the hook command output above for details",
            ))
        }
        _ => Err(Error::execution(format!(
            "Hook execution did not complete normally for {}",
            directory.display()
        ))),
    }
}

/// Collect hooks from all ancestor env.cue files, resolving their dirs.
/// Returns hooks in root-to-leaf order (ancestors first).
///
/// Hooks from ancestor directories are only included if `propagate: true`.
/// Hooks from the current directory are always included regardless of `propagate`.
fn collect_hooks_from_ancestors(
    directory: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Vec<cuenv_hooks::Hook>> {
    let ancestors = env_file::find_ancestor_env_files(directory, package)?;

    let mut all_hooks = Vec::new();
    let ancestors_len = ancestors.len();

    for (i, ancestor_dir) in ancestors.into_iter().enumerate() {
        let is_current_dir = i == ancestors_len - 1;

        // Evaluate the CUE config for this ancestor using module-wide evaluation
        let config: Project = match evaluate_project(&ancestor_dir, package, executor) {
            Ok(c) => c,
            Err(e) => {
                debug!(
                    "Failed to evaluate {} for hooks: {}",
                    ancestor_dir.display(),
                    e
                );
                continue;
            }
        };

        // Extract hooks and resolve their dirs relative to this ancestor
        let mut hooks = extract_hooks_with_resolved_dirs(&config, &ancestor_dir);

        // Filter: ancestor hooks only if propagate=true, current dir always included
        if !is_current_dir {
            hooks.retain(|h| h.propagate);
        }

        if !hooks.is_empty() {
            debug!(
                "Found {} hooks in {} (is_current={})",
                hooks.len(),
                ancestor_dir.display(),
                is_current_dir
            );
        }
        all_hooks.extend(hooks);
    }

    Ok(all_hooks)
}

/// Request for resolving static environment variables plus hook-generated variables.
pub struct HookEnvironmentRequest<'a> {
    /// Project directory whose environment should be resolved.
    directory: &'a Path,
    /// Evaluated project configuration for the current directory.
    config: &'a Project,
    /// CUE package name used when ancestor configs need evaluation.
    package: &'a str,
    /// Optional cached command executor for module-wide CUE evaluation.
    executor: Option<&'a CommandExecutor>,
}

impl<'a> HookEnvironmentRequest<'a> {
    /// Create a request without a cached command executor.
    #[must_use]
    pub fn new(directory: &'a Path, config: &'a Project, package: &'a str) -> Self {
        Self {
            directory,
            config,
            package,
            executor: None,
        }
    }

    /// Use an existing command executor for ancestor CUE evaluation.
    #[must_use]
    pub fn with_executor(mut self, executor: &'a CommandExecutor) -> Self {
        self.executor = Some(executor);
        self
    }
}

struct BackgroundHookStart<'a> {
    executor: &'a HookExecutor,
    directory: &'a Path,
    config_hash: &'a str,
    hooks: Vec<cuenv_hooks::Hook>,
}

struct BackgroundHookWait<'a> {
    executor: &'a HookExecutor,
    directory: &'a Path,
    config_hash: &'a str,
    config: &'a Project,
}

/// Get environment variables with hook-generated vars merged in.
///
/// This function checks if hooks have completed and merges their environment
/// with the static environment from the CUE manifest. This is used by
/// `cuenv task` and `cuenv exec` to ensure they have access to hook-generated
/// environment variables.
///
/// This function walks up from `directory` to find all ancestor env.cue files
/// with hooks, resolves each hook's `dir` field relative to its source env.cue,
/// and executes hooks in root-to-leaf order.
///
/// This function ensures hooks are running and waits for their completion.
///
/// # Errors
///
/// Returns an error if hook execution fails or state management fails.
pub async fn get_environment_with_hooks(
    request: HookEnvironmentRequest<'_>,
) -> Result<HashMap<String, String>> {
    let HookEnvironmentRequest {
        directory,
        config,
        package,
        executor,
    } = request;

    // Start with static environment from CUE manifest
    let static_env = extract_static_env_vars(config);

    // Collect hooks from all ancestors with resolved dirs
    let all_hooks = collect_hooks_from_ancestors(directory, package, executor)?;

    if all_hooks.is_empty() {
        return Ok(static_env);
    }

    debug!(
        "Collected {} hooks from ancestors for {}",
        all_hooks.len(),
        directory.display()
    );

    // Compute execution hash including hook definitions AND input file contents
    // This is separate from approval hash - approval only cares about hook definitions,
    // but execution cache needs to invalidate when input files (e.g., flake.nix) change
    let config_hash = cuenv_hooks::compute_execution_hash(&all_hooks, directory);

    // Check if foreground hook execution is requested (useful for CI environments
    // where detached supervisor processes may not work correctly).
    // When foreground hooks are requested, we always run them synchronously,
    // ignoring any cached state from previous background executions.
    if should_run_hooks_foreground() {
        info!(
            "Running {} hooks in foreground for {} (CUENV_FOREGROUND_HOOKS=1)",
            all_hooks.len(),
            directory.display()
        );
        return run_hooks_foreground(ForegroundHookRun {
            directory,
            config_hash: &config_hash,
            hooks: all_hooks,
            config,
        })
        .await;
    }

    let executor = HookExecutor::with_default_config()?;
    start_background_hooks_if_needed(BackgroundHookStart {
        executor: &executor,
        directory,
        config_hash: &config_hash,
        hooks: all_hooks,
    })
    .await?;

    wait_for_background_hooks(BackgroundHookWait {
        executor: &executor,
        directory,
        config_hash: &config_hash,
        config,
    })
    .await
}

fn should_run_hooks_foreground() -> bool {
    std::env::var("CUENV_FOREGROUND_HOOKS")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

async fn start_background_hooks_if_needed(request: BackgroundHookStart<'_>) -> Result<()> {
    let BackgroundHookStart {
        executor,
        directory,
        config_hash,
        hooks,
    } = request;

    // Check if state exists
    let status = executor
        .get_execution_status_for_instance(directory, config_hash)
        .await?;

    // If no state exists, start execution
    if status.is_none() {
        info!(
            "Starting execution of {} hooks for {}",
            hooks.len(),
            directory.display()
        );
        executor
            .execute_hooks_background(directory.to_path_buf(), config_hash.to_string(), hooks)
            .await?;
    }

    Ok(())
}

async fn wait_for_background_hooks(
    request: BackgroundHookWait<'_>,
) -> Result<HashMap<String, String>> {
    let BackgroundHookWait {
        executor,
        directory,
        config_hash,
        config,
    } = request;

    // Wait for completion with progress indicator
    debug!("Waiting for hooks to complete for {}", directory.display());

    let poll_interval = Duration::from_millis(50);
    let start_time = Instant::now();
    let timeout_seconds = hook_timeout_seconds();
    let is_tty = std::io::stderr().is_terminal();

    loop {
        if let Some(state) = executor
            .get_execution_status_for_instance(directory, config_hash)
            .await?
        {
            render_hook_wait_progress(&state, start_time, is_tty);

            if state.is_complete() {
                clear_hook_progress_line(is_tty);
                return hook_completion_result(directory, config, state);
            }
        } else {
            // No state found - this shouldn't happen since we started execution above
            return Err(Error::execution(format!(
                "Hook execution state lost for {}. This is a bug — hooks were started but no state was recorded.",
                directory.display()
            )));
        }

        // Check timeout
        if start_time.elapsed().as_secs() >= timeout_seconds {
            clear_hook_progress_line(is_tty);
            return Err(Error::Timeout {
                seconds: timeout_seconds,
            });
        }

        tokio::time::sleep(poll_interval).await;
    }
}

fn hook_timeout_seconds() -> u64 {
    std::env::var("CUENV_HOOK_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600)
}

fn render_hook_wait_progress(state: &HookExecutionState, start_time: Instant, is_tty: bool) {
    if !is_tty || state.status != ExecutionStatus::Running {
        return;
    }

    let elapsed = start_time.elapsed().as_secs();
    let hook_name = state
        .current_hook_display()
        .unwrap_or_else(|| "hook".to_string());
    #[allow(clippy::print_stderr)] // TTY progress indicator, no secrets
    {
        eprint!("\r\x1b[KWaiting for hook `{hook_name}` to complete... [{elapsed}s]");
    }
    let _ = std::io::stderr().flush();
}

fn clear_hook_progress_line(is_tty: bool) {
    if !is_tty {
        return;
    }

    #[allow(clippy::print_stderr)] // TTY clear line, no secrets
    {
        eprint!("\r\x1b[K");
    }
    let _ = std::io::stderr().flush();
}

fn hook_completion_result(
    directory: &Path,
    config: &Project,
    state: HookExecutionState,
) -> Result<HashMap<String, String>> {
    match state.status {
        ExecutionStatus::Completed => Ok(collect_all_env_vars(config, &state.environment_vars)),
        ExecutionStatus::Failed => {
            let msg = state
                .error_message
                .unwrap_or_else(|| "unknown error".to_string());
            Err(Error::execution_with_help(
                format!("Hook execution failed for {}: {msg}", directory.display()),
                "Run with CUENV_LOG=debug for more details, or CUENV_FOREGROUND_HOOKS=1 to see hook output directly",
            ))
        }
        ExecutionStatus::Cancelled => Err(Error::execution(format!(
            "Hook execution was cancelled for {}",
            directory.display()
        ))),
        ExecutionStatus::Running => {
            unreachable!("is_complete() returned true but status is Running")
        }
    }
}
