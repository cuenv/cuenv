//! Export command - the heart of cuenv's shell integration.
//!
//! This command is called by the shell hook on every prompt to:
//! 1. Check if environment is ready (instant)
//! 2. Start supervisor if needed (async)
//! 3. Return environment diff for shell evaluation

mod hooks;

pub use hooks::{HookEnvironmentRequest, get_environment_with_hooks};

use super::env_file::{self, EnvFileStatus, find_cue_module_root};
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result, shell::Shell};
use cuenv_hooks::{
    ApprovalManager, ApprovalStatus, ConfigSummary, ExecutionStatus, HookExecutionState,
    HookExecutor, StateManager, check_approval_status,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
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
pub async fn execute_export(
    shell_type: Option<&str>,
    path: &str,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    let shell = Shell::detect(shell_type);
    let target_dir = Path::new(path);

    let Some(directory) = find_export_directory(target_dir, package)? else {
        return Ok(format_no_op(shell));
    };

    // Always evaluate CUE to get current config (uses executor cache if available)
    debug!("Evaluating CUE for {}", directory.display());
    let config: Project = evaluate_project(&directory, package, executor)?;

    if let Some(output) = render_approval_required(&directory, &config, shell).await? {
        return Ok(output);
    }

    // Compute config hash for this directory + config (only hooks are included)
    let config_hash = cuenv_hooks::compute_approval_hash(config.hooks.as_ref());

    // Check if state is ready
    let executor = HookExecutor::with_default_config()?;
    let context = ExportHookContext {
        executor: &executor,
        directory: &directory,
        config: &config,
        config_hash: &config_hash,
        shell,
    };

    match inspect_initial_hook_state(context).await? {
        InitialHookState::Rendered(output) => return Ok(output),
        InitialHookState::Running => {}
        InitialHookState::Missing => start_hook_execution(context).await?,
    }

    // Wait briefly for fast hooks (10ms)
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Check again
    if let Some(output) = render_completed_hook_state(context).await? {
        return Ok(output);
    }

    // Still not ready - return partial environment (just static vars from CUE)
    let static_env = extract_static_env_vars(context.config);
    if !static_env.is_empty() {
        debug!(
            "Returning partial environment ({} vars) while hooks run",
            static_env.len()
        );
        return Ok(format_env_diff(context.directory, static_env, shell));
    }

    // No environment available yet - return safe no-op
    Ok(format_no_op(shell))
}

fn find_export_directory(target_dir: &Path, package: &str) -> Result<Option<std::path::PathBuf>> {
    match env_file::find_env_file(target_dir, package)? {
        EnvFileStatus::Match(dir) => Ok(Some(dir)),
        EnvFileStatus::Missing => {
            debug!("No env.cue found in {}", target_dir.display());
            Ok(None)
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            debug!(
                "env.cue package mismatch in {}: found {:?}, expected {}",
                target_dir.display(),
                found_package,
                package
            );
            Ok(None)
        }
    }
}

async fn render_approval_required(
    directory: &Path,
    config: &Project,
    shell: Shell,
) -> Result<Option<String>> {
    let mut approval_manager = ApprovalManager::with_default_file()?;
    approval_manager.load_approvals().await?;

    debug!("Checking approval for directory: {}", directory.display());
    let approval_status =
        check_approval_status(&approval_manager, directory, config.hooks.as_ref())?;

    match approval_status {
        ApprovalStatus::NotApproved { .. } | ApprovalStatus::RequiresApproval { .. } => {
            let summary = ConfigSummary::from_hooks(config.hooks.as_ref());
            if summary.has_hooks {
                return Ok(Some(format_not_allowed(
                    directory,
                    shell,
                    summary.hook_count,
                )));
            }
            debug!("Auto-approving configuration with no hooks");
            Ok(None)
        }
        ApprovalStatus::Approved => Ok(None),
    }
}

#[derive(Clone, Copy)]
struct ExportHookContext<'a> {
    executor: &'a HookExecutor,
    directory: &'a Path,
    config: &'a Project,
    config_hash: &'a str,
    shell: Shell,
}

enum InitialHookState {
    Rendered(String),
    Running,
    Missing,
}

async fn inspect_initial_hook_state(context: ExportHookContext<'_>) -> Result<InitialHookState> {
    let Some(state) = context
        .executor
        .get_execution_status_for_instance(context.directory, context.config_hash)
        .await?
    else {
        return Ok(InitialHookState::Missing);
    };

    match state.status {
        ExecutionStatus::Completed => Ok(InitialHookState::Rendered(render_ready_environment(
            context, &state,
        ))),
        ExecutionStatus::Failed => {
            debug!(
                "Hooks failed for {}: {:?}",
                context.directory.display(),
                state.error_message
            );
            Ok(InitialHookState::Rendered(format_no_op(context.shell)))
        }
        ExecutionStatus::Running => {
            debug!("Hooks still running for {}", context.directory.display());
            Ok(InitialHookState::Running)
        }
        ExecutionStatus::Cancelled => Ok(InitialHookState::Rendered(format_no_op(context.shell))),
    }
}

async fn start_hook_execution(context: ExportHookContext<'_>) -> Result<()> {
    info!(
        "Starting hook execution for {}",
        context.directory.display()
    );

    let hooks = extract_hooks_from_config(context.config);
    if !hooks.is_empty() {
        context
            .executor
            .execute_hooks_background(
                context.directory.to_path_buf(),
                context.config_hash.to_string(),
                hooks,
            )
            .await?;
    }

    Ok(())
}

async fn render_completed_hook_state(context: ExportHookContext<'_>) -> Result<Option<String>> {
    let Some(state) = context
        .executor
        .get_execution_status_for_instance(context.directory, context.config_hash)
        .await?
    else {
        return Ok(None);
    };

    if state.status == ExecutionStatus::Completed {
        Ok(Some(render_ready_environment(context, &state)))
    } else {
        Ok(None)
    }
}

fn render_ready_environment(context: ExportHookContext<'_>, state: &HookExecutionState) -> String {
    let env_vars = collect_all_env_vars(context.config, &state.environment_vars);
    format_env_diff_with_unset(
        context.directory,
        env_vars,
        state.previous_env.as_ref(),
        context.shell,
    )
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
/// Secrets (including interpolated values containing secrets) are excluded from the returned map.
#[must_use]
pub fn extract_static_env_vars(config: &Project) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    if let Some(env) = &config.env {
        for (key, value) in &env.base {
            // Skip any value that contains secrets
            if value.is_secret() {
                continue;
            }
            env_vars.insert(key.clone(), value.to_string_value());
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
#[path = "export_tests.rs"]
mod tests;
