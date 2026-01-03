//! Export command - the heart of cuenv's shell integration.
//!
//! This command is called by the shell hook on every prompt to:
//! 1. Check if environment is ready (instant)
//! 2. Start supervisor if needed (async)
//! 3. Return environment diff for shell evaluation

use super::env_file::{self, EnvFileStatus, find_cue_module_root};
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result, shell::Shell};
use cuenv_hooks::{
    ApprovalManager, ApprovalStatus, ConfigSummary, ExecutionStatus, HookExecutionConfig,
    HookExecutionState, HookExecutor, StateManager, check_approval_status,
    compute_instance_hash, execute_hooks,
};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info};

const PENDING_APPROVAL_ENV: &str = "CUENV_PENDING_APPROVAL_DIR";
const LOADED_DIR_ENV: &str = "CUENV_LOADED_DIR";

/// Evaluate CUE configuration using module-wide evaluation.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
fn evaluate_project(
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

        // Check if this is a Project (has name field) or Base (no name)
        return match instance.kind {
            cuenv_core::InstanceKind::Project => instance.deserialize(),
            cuenv_core::InstanceKind::Base => Err(cuenv_core::Error::configuration(
                "This directory uses schema.#Base which doesn't support export.\n\
                 To use export, update your env.cue to use schema.#Project:\n\n\
                 schema.#Project\n\
                 name: \"your-project-name\"",
            )),
        };
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    // Use non-recursive evaluation since export only needs the current project's config,
    // not cross-project references. This is called on every shell prompt so speed matters.
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
    );

    let rel_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    // Check if this is a Project (has name field) or Base (no name)
    match instance.kind {
        cuenv_core::InstanceKind::Project => instance.deserialize(),
        cuenv_core::InstanceKind::Base => Err(cuenv_core::Error::configuration(
            "This directory uses schema.#Base which doesn't support export.\n\
             To use export, update your env.cue to use schema.#Project:\n\n\
             schema.#Project\n\
             name: \"your-project-name\"",
        )),
    }
}

/// Synchronous fast path for export command.
///
/// This function handles cases that don't require CUE evaluation or async operations:
/// - No env.cue present: returns no-op immediately
/// - Hooks running/failed/cancelled: returns no-op immediately
///
/// Returns `Ok(Some(output))` if the fast path handled the request,
/// or `Ok(None)` if the async path is needed (for CUE evaluation or hook startup).
///
/// # Errors
///
/// Returns an error if state management fails.
pub fn execute_export_sync(
    shell_type: Option<&str>,
    path: &str,
    package: &str,
) -> Result<Option<String>> {
    let shell = Shell::detect(shell_type);
    let target_dir = Path::new(path);

    // Fast check: does env.cue exist with matching package?
    let directory = match env_file::find_env_file(target_dir, package)? {
        EnvFileStatus::Match(dir) => dir,
        EnvFileStatus::Missing => {
            debug!(
                "No env.cue found in {} (sync fast path)",
                target_dir.display()
            );
            return Ok(Some(format_no_op(shell)));
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            debug!(
                "env.cue package mismatch in {}: found {:?}, expected {} (sync fast path)",
                target_dir.display(),
                found_package,
                package
            );
            return Ok(Some(format_no_op(shell)));
        }
    };

    // Check if we have an active marker for this directory (single stat() call)
    let state_manager = StateManager::with_default_dir()?;
    if !state_manager.has_active_marker(&directory) {
        // No marker means we need async to evaluate CUE and potentially start hooks
        debug!(
            "No active marker for {} - falling back to async",
            directory.display()
        );
        return Ok(None);
    }

    // Marker exists - try to load state synchronously
    if let Some(instance_hash) = state_manager.get_marker_instance_hash_sync(&directory)
        && let Ok(Some(state)) = state_manager.load_state_sync(&instance_hash)
    {
        match state.status {
            ExecutionStatus::Completed => {
                // Environment is ready but we need CUE eval for static vars
                // Fall back to async path with lightweight runtime
                debug!(
                    "State completed for {} - need async for CUE eval",
                    directory.display()
                );
                return Ok(None);
            }
            ExecutionStatus::Running => {
                // Hooks still running - don't block the shell prompt
                debug!(
                    "Hooks still running for {} (sync fast path)",
                    directory.display()
                );
                return Ok(Some(format_no_op(shell)));
            }
            ExecutionStatus::Failed | ExecutionStatus::Cancelled => {
                // Hooks failed or cancelled - return no-op
                debug!(
                    "Hooks {:?} for {} (sync fast path)",
                    state.status,
                    directory.display()
                );
                return Ok(Some(format_no_op(shell)));
            }
        }
    }

    // Fall back to async path
    Ok(None)
}

/// Execute the export command - the main entry point for shell integration.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or approval management fails.
#[allow(clippy::too_many_lines, clippy::uninlined_format_args)]
pub async fn execute_export(
    shell_type: Option<&str>,
    path: &str,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    let shell = Shell::detect(shell_type);
    let target_dir = Path::new(path);

    // Check if env.cue exists with matching package
    let directory = match env_file::find_env_file(target_dir, package)? {
        EnvFileStatus::Match(dir) => dir,
        EnvFileStatus::Missing => {
            debug!("No env.cue found in {}", target_dir.display());
            return Ok(format_no_op(shell));
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            debug!(
                "env.cue package mismatch in {}: found {:?}, expected {}",
                target_dir.display(),
                found_package,
                package
            );
            return Ok(format_no_op(shell));
        }
    };

    // Always evaluate CUE to get current config (uses executor cache if available)
    debug!("Evaluating CUE for {}", directory.display());
    let config: Project = evaluate_project(&directory, package, executor)?;

    // Load approval manager and check approval status
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    debug!("Checking approval for directory: {}", directory.display());
    let approval_status = check_approval_status(&approval_manager, &directory, config.hooks.as_ref())?;

    match approval_status {
        ApprovalStatus::NotApproved { .. } | ApprovalStatus::RequiresApproval { .. } => {
            let summary = ConfigSummary::from_hooks(config.hooks.as_ref());
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

    // Compute config hash for this directory + config (only hooks are included)
    let config_hash = cuenv_hooks::compute_approval_hash(config.hooks.as_ref());

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
fn extract_hooks_from_config(config: &Project) -> Vec<cuenv_hooks::Hook> {
    config.on_enter_hooks()
}

/// Resolve a hook's `dir` field relative to the env.cue directory where it's defined.
/// If `dir` is None or ".", it becomes the `env_cue_dir`.
/// If `dir` is a relative path, it's resolved relative to `env_cue_dir`.
/// The result is always an absolute path.
fn resolve_hook_dir(hook: &mut cuenv_hooks::Hook, env_cue_dir: &Path) {
    let relative_dir = hook.dir.as_deref().unwrap_or(".");
    let absolute_dir = env_cue_dir.join(relative_dir);

    // Canonicalize if possible, otherwise use the joined path
    let resolved = absolute_dir.canonicalize().unwrap_or(absolute_dir);

    hook.dir = Some(resolved.to_string_lossy().to_string());
}

/// Extract hooks from a config and resolve their `dir` fields relative to the given directory.
fn extract_hooks_with_resolved_dirs(
    config: &Project,
    env_cue_dir: &Path,
) -> Vec<cuenv_hooks::Hook> {
    let mut hooks = config.on_enter_hooks();
    for hook in &mut hooks {
        resolve_hook_dir(hook, env_cue_dir);
    }
    hooks
}

/// Extract static environment variables from CUE config.
///
/// Secrets are excluded from the returned map.
#[must_use]
pub fn extract_static_env_vars(config: &Project) -> HashMap<String, String> {
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
    config: &Project,
    hook_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut all_vars = extract_static_env_vars(config);

    // Hook environment variables override static ones
    for (key, value) in hook_env {
        all_vars.insert(key.clone(), value.clone());
    }

    all_vars
}

/// Run hooks in the foreground (same process) instead of spawning a detached supervisor.
/// This is useful for CI environments where detached processes may not work correctly.
async fn run_hooks_foreground(
    directory: &Path,
    config_hash: &str,
    hooks: Vec<cuenv_hooks::Hook>,
    config: &Project,
) -> Result<HashMap<String, String>> {
    let static_env = extract_static_env_vars(config);
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
            debug!(
                "Foreground hooks failed: {:?}. Using captured environment.",
                state.error_message
            );
            Ok(collect_all_env_vars(config, &state.environment_vars))
        }
        _ => {
            debug!("Foreground hooks did not complete normally");
            Ok(static_env)
        }
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
#[allow(clippy::too_many_lines)]
pub async fn get_environment_with_hooks(
    directory: &Path,
    config: &Project,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<HashMap<String, String>> {
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
    let foreground_hooks = std::env::var("CUENV_FOREGROUND_HOOKS")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    if foreground_hooks {
        info!(
            "Running {} hooks in foreground for {} (CUENV_FOREGROUND_HOOKS=1)",
            all_hooks.len(),
            directory.display()
        );
        return run_hooks_foreground(directory, &config_hash, all_hooks, config).await;
    }

    let executor = HookExecutor::with_default_config()?;

    // Check if state exists
    let status = executor
        .get_execution_status_for_instance(directory, &config_hash)
        .await?;

    // If no state exists, start execution
    if status.is_none() {
        info!(
            "Starting execution of {} hooks for {}",
            all_hooks.len(),
            directory.display()
        );
        executor
            .execute_hooks_background(directory.to_path_buf(), config_hash.clone(), all_hooks)
            .await?;
    }

    // Wait for completion with progress indicator (timeout 60s)
    debug!("Waiting for hooks to complete for {}", directory.display());

    let poll_interval = Duration::from_millis(50);
    let start_time = Instant::now();
    let timeout_seconds = 60u64;
    let is_tty = std::io::stderr().is_terminal();

    loop {
        if let Some(state) = executor
            .get_execution_status_for_instance(directory, &config_hash)
            .await?
        {
            // Show progress indicator on TTY
            if is_tty && state.status == ExecutionStatus::Running {
                let elapsed = start_time.elapsed().as_secs();
                let hook_name = state
                    .current_hook_display()
                    .unwrap_or_else(|| "hook".to_string());
                eprint!("\r\x1b[KWaiting for hook `{hook_name}` to complete... [{elapsed}s]");
                let _ = std::io::stderr().flush();
            }

            if state.is_complete() {
                // Clear the progress line
                if is_tty {
                    eprint!("\r\x1b[K");
                    let _ = std::io::stderr().flush();
                }

                return match state.status {
                    ExecutionStatus::Completed => {
                        Ok(collect_all_env_vars(config, &state.environment_vars))
                    }
                    ExecutionStatus::Failed => {
                        debug!(
                            "Hooks failed for {}: {:?}. Using captured environment.",
                            directory.display(),
                            state.error_message
                        );
                        Ok(collect_all_env_vars(config, &state.environment_vars))
                    }
                    ExecutionStatus::Cancelled => {
                        debug!("Hooks cancelled for {}", directory.display());
                        Ok(static_env)
                    }
                    ExecutionStatus::Running => Ok(static_env),
                };
            }
        } else {
            // No state found - this shouldn't happen since we started execution above
            tracing::warn!("No execution state found, using static environment");
            return Ok(static_env);
        }

        // Check timeout
        if start_time.elapsed().as_secs() >= timeout_seconds {
            if is_tty {
                eprint!("\r\x1b[K");
                let _ = std::io::stderr().flush();
            }
            tracing::warn!(
                "Timeout waiting for hooks after {}s, using static environment",
                timeout_seconds
            );
            return Ok(static_env);
        }

        tokio::time::sleep(poll_interval).await;
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
    echo "Project environment loaded" >&2
    export {loaded}="{escaped_dir}"
    unset {pending} 2>/dev/null
fi"#,
            loaded = LOADED_DIR_ENV,
            pending = PENDING_APPROVAL_ENV,
            escaped_dir = escaped_dir,
        ),
        Shell::Fish => format!(
            r#"if not set -q {loaded}
    echo "Project environment loaded" >&2
    set -x {loaded} "{escaped_dir}"
    set -e {pending} 2>/dev/null
else if test "${loaded}" != "{escaped_dir}"
    echo "Project environment loaded" >&2
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
    Write-Host 'Project environment loaded'
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
                let _ = writeln!(&mut output, "export {key}=\"{escaped_value}\"");
            }
            Shell::Fish => {
                let _ = writeln!(&mut output, "set -x {key} \"{escaped_value}\"");
            }
            Shell::PowerShell => {
                let _ = writeln!(&mut output, "$env:{key} = \"{escaped_value}\"");
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
                        let _ = writeln!(&mut output, "unset {key}");
                    }
                    Shell::Fish => {
                        let _ = writeln!(&mut output, "set -e {key}");
                    }
                    Shell::PowerShell => {
                        let _ = writeln!(&mut output, "Remove-Item Env:{key}");
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
                let _ = writeln!(&mut output, "export {key}=\"{escaped_value}\"");
            }
            Shell::Fish => {
                let _ = writeln!(&mut output, "set -x {key} \"{escaped_value}\"");
            }
            Shell::PowerShell => {
                let _ = writeln!(&mut output, "$env:{key} = \"{escaped_value}\"");
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
    use cuenv_core::manifest::Project;
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
        assert!(bash.contains("echo \"Project environment loaded\" >&2"));
        assert!(bash.contains("export CUENV_LOADED_DIR=\"/tmp/project\""));
        assert!(bash.contains("export FOO=\"bar baz\""));
        assert!(bash.contains("export NUM=\"42\""));

        let zsh = format_env_diff(dir, env.clone(), Shell::Zsh);
        assert!(zsh.contains("echo \"Project environment loaded\" >&2"));
        assert!(zsh.contains("export FOO=\"bar baz\""));

        let fish = format_env_diff(dir, env.clone(), Shell::Fish);
        assert!(fish.contains("echo \"Project environment loaded\" >&2"));
        assert!(fish.contains("set -x CUENV_LOADED_DIR \"/tmp/project\""));
        assert!(fish.contains("set -x FOO \"bar baz\""));

        let pwsh = format_env_diff(dir, env, Shell::PowerShell);
        assert!(pwsh.contains("Write-Host 'Project environment loaded'"));
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
        assert!(out_bash.contains("echo \"Project environment loaded\""));

        let out_fish =
            format_env_diff_with_unset(dir, current.clone(), Some(&previous), Shell::Fish);
        assert!(out_fish.lines().any(|l| l == "set -e REMOVED"));

        let out_pwsh = format_env_diff_with_unset(dir, current, Some(&previous), Shell::PowerShell);
        assert!(out_pwsh.lines().any(|l| l == "Remove-Item Env:REMOVED"));
    }

    #[test]
    fn test_extract_static_env_vars_skips_secrets() {
        // Build Project with one normal var and one secret
        let mut base = HashMap::new();
        base.insert("PLAIN".to_string(), EnvValue::String("value".to_string()));
        let secret = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
        base.insert("SECRET".to_string(), EnvValue::Secret(secret));

        let env_cfg = Env {
            base,
            environment: None,
        };
        let cfg = Project {
            config: None,
            env: Some(env_cfg),
            hooks: None,
            ci: None,
            tasks: HashMap::new(),
            name: "test".to_string(),
            cube: None,
            runtime: None,
            formatters: None,
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

        let cfg = Project {
            config: None,
            env: Some(Env {
                base,
                environment: None,
            }),
            hooks: None,
            ci: None,
            tasks: HashMap::new(),
            name: "test".to_string(),
            cube: None,
            runtime: None,
            formatters: None,
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
