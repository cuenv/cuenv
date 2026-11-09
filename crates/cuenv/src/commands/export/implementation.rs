//! Shell environment export and differences.

use crate::commands::env_file::{self, EnvFileStatus};
use crate::commands::{CommandExecutor, convert_engine_error};
use cuengine::ModuleEvalOptions;
use cuenv_core::environment::EnvValue;
use cuenv_core::manifest::Project;
use cuenv_core::module::{ModuleEvaluationOptions, TASK_FIELD};
use cuenv_core::shell::Shell;
use cuenv_core::{ModuleEvaluation, Result};
use cuenv_hooks::{
    ApprovalManager, ApprovalStatus, ConfigSummary, ExecutionStatus, HookExecutionState,
    HookExecutor, HookSessionContext, StateManager,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info};

const PENDING_APPROVAL_ENV: &str = "CUENV_PENDING_APPROVAL_DIR";
const LOADED_DIR_ENV: &str = "CUENV_LOADED_DIR";
const MANAGED_VARS_ENV: &str = "CUENV_MANAGED_VARS";

/// Evaluate CUE configuration using module-wide evaluation.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
pub(super) fn evaluate_project(
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

    let module_root = env_file::find_cue_module_root(&target_path).ok_or_else(|| {
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
        EnvFileStatus::Match(dir) => Some(dir),
        EnvFileStatus::Missing => {
            debug!(
                "No env.cue found in {} (sync fast path)",
                target_dir.display()
            );
            None
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            debug!(
                "env.cue package mismatch in {}: found {:?}, expected {} (sync fast path)",
                target_dir.display(),
                found_package,
                package
            );
            None
        }
    };
    if let Some(output) = preflight_shell_transition(directory.as_deref(), package, shell)? {
        return Ok(Some(output));
    }
    let Some(directory) = directory else {
        return Ok(Some(format_no_op(shell)));
    };

    let Some(session_id) = crate::commands::hooks::optional_hook_session()? else {
        return Ok(None);
    };
    let state_manager = match std::env::var_os("CUENV_STATE_DIR") {
        Some(path) => StateManager::new(path.into()),
        None => StateManager::with_default_dir()?,
    };
    let Some(state) = state_manager.fast_status_sync(&directory, session_id)? else {
        debug!(
            "No hook state for {} - falling back to async",
            directory.display()
        );
        return Ok(None);
    };

    match state.status {
        ExecutionStatus::Completed => {
            // Static variables still need CUE evaluation.
            debug!(
                "State completed for {} - need async for CUE eval",
                directory.display()
            );
            Ok(None)
        }
        ExecutionStatus::Running | ExecutionStatus::Cancelling => {
            debug!(
                "Hooks still active for {} (sync fast path)",
                directory.display()
            );
            Ok(Some(format_pending(shell)))
        }
        ExecutionStatus::Failed | ExecutionStatus::Cancelled => {
            // Inputs may have changed since the terminal run; check its identity.
            Ok(None)
        }
    }
}

/// Shell and evaluator selection for environment export.
#[derive(Clone, Copy)]
pub struct ExportOptions<'options> {
    /// Explicit shell name, or automatic detection when absent.
    pub shell_type: Option<&'options str>,
    /// Cached evaluator, when available.
    pub executor: Option<&'options CommandExecutor>,
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
    path: &str,
    package: &str,
    options: ExportOptions<'_>,
) -> Result<String> {
    let ExportOptions {
        shell_type,
        executor,
    } = options;
    let shell = Shell::detect(shell_type);
    let target_dir = Path::new(path);

    let directory = find_export_directory(target_dir, package)?;
    if let Some(output) = preflight_shell_transition(directory.as_deref(), package, shell)? {
        return Ok(output);
    }
    let Some(directory) = directory else {
        return Ok(format_no_op(shell));
    };

    // Always evaluate CUE to get current config (uses executor cache if available)
    debug!("Evaluating CUE for {}", directory.display());
    let config: Project = evaluate_project(&directory, package, executor)?;

    if let Some(output) = render_approval_required(&directory, &config, shell).await? {
        return Ok(output);
    }

    let hooks = config.on_enter_hooks();
    if hooks.is_empty() {
        let env = extract_static_env_vars(&config, StaticEnvConsumer::Shell);
        return format_environment(
            EnvironmentContext {
                directory: &directory,
                package,
                shell,
            },
            env,
        );
    }
    let config_hash = cuenv_hooks::compute_execution_hash(&hooks, &directory)?;
    let session_id = crate::commands::hooks::required_hook_session()?;

    // Check if state is ready
    let executor = HookExecutor::with_default_config()?;
    let context = ExportHookContext {
        executor: &executor,
        directory: &directory,
        package,
        config: &config,
        config_hash: &config_hash,
        session_id,
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

    // Keep the last successful environment until all hooks have completed.
    Ok(format_pending(shell))
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
        cuenv_hooks::check_approval_status(&approval_manager, directory, config.hooks.as_ref());

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
struct ExportHookContext<'context> {
    executor: &'context HookExecutor,
    directory: &'context Path,
    package: &'context str,
    config: &'context Project,
    config_hash: &'context str,
    session_id: uuid::Uuid,
    shell: Shell,
}

enum InitialHookState {
    Rendered(String),
    Running,
    Missing,
}

async fn inspect_initial_hook_state(context: ExportHookContext<'_>) -> Result<InitialHookState> {
    let state = match context
        .executor
        .execution_status_for_instance(HookSessionContext {
            directory: context.directory,
            config_hash: context.config_hash,
            session_id: context.session_id,
        })
        .await
    {
        Ok(state) => state,
        Err(cuenv_hooks::Error::Busy) => return Ok(InitialHookState::Running),
        Err(error) => return Err(error.into()),
    };
    let Some(state) = state else {
        return Ok(InitialHookState::Missing);
    };

    match state.status {
        ExecutionStatus::Completed => Ok(InitialHookState::Rendered(render_ready_environment(
            context, &state,
        )?)),
        ExecutionStatus::Failed => {
            debug!(
                "Hooks failed for {}: {:?}",
                context.directory.display(),
                state.error_message
            );
            Ok(InitialHookState::Rendered(format_pending(context.shell)))
        }
        ExecutionStatus::Running | ExecutionStatus::Cancelling => {
            debug!("Hooks still running for {}", context.directory.display());
            Ok(InitialHookState::Running)
        }
        ExecutionStatus::Cancelled => Ok(InitialHookState::Rendered(format_pending(context.shell))),
    }
}

async fn start_hook_execution(context: ExportHookContext<'_>) -> Result<()> {
    info!(
        "Starting hook execution for {}",
        context.directory.display()
    );

    let hooks = extract_hooks_from_config(context.config);
    if !hooks.is_empty() {
        match context
            .executor
            .execute_hooks_background(
                HookSessionContext {
                    directory: context.directory,
                    config_hash: context.config_hash,
                    session_id: context.session_id,
                },
                hooks,
                cuenv_hooks::HookStartMode::IfMissing,
            )
            .await
        {
            Ok(_) | Err(cuenv_hooks::Error::Busy) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

async fn render_completed_hook_state(context: ExportHookContext<'_>) -> Result<Option<String>> {
    let state = match context
        .executor
        .execution_status_for_instance(HookSessionContext {
            directory: context.directory,
            config_hash: context.config_hash,
            session_id: context.session_id,
        })
        .await
    {
        Ok(state) => state,
        Err(cuenv_hooks::Error::Busy) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(state) = state else {
        return Ok(None);
    };

    if state.status == ExecutionStatus::Completed {
        Ok(Some(render_ready_environment(context, &state)?))
    } else {
        Ok(None)
    }
}

fn render_ready_environment(
    context: ExportHookContext<'_>,
    state: &HookExecutionState,
) -> Result<String> {
    let env_vars = collect_all_env_vars(context.config, &state.environment_vars);
    format_environment(
        EnvironmentContext {
            directory: context.directory,
            package: context.package,
            shell: context.shell,
        },
        env_vars,
    )
}

/// Extract hooks from CUE config
fn extract_hooks_from_config(config: &Project) -> Vec<cuenv_hooks::Hook> {
    config.on_enter_hooks()
}

/// The consumer of a set of static environment variables.
///
/// A value declared with `policies` is restricted to the consumers its
/// policies list, the same rule that applies to secrets: `allowTasks` for
/// tasks and `allowExec` for `cuenv exec`. No policy names the interactive
/// shell, so the shell receives only values without a policy list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticEnvConsumer<'static_env_consumer> {
    /// The interactive shell integration (`cuenv export`).
    Shell,
    /// A task, by its fully qualified name.
    Task(&'static_env_consumer str),
    /// A `cuenv exec` command.
    Exec(&'static_env_consumer str),
}

impl StaticEnvConsumer<'_> {
    fn may_receive(self, value: &EnvValue) -> bool {
        match self {
            Self::Shell => value.is_accessible_by_shell(),
            Self::Task(name) => value.is_accessible_by_task(name),
            Self::Exec(command) => value.is_accessible_by_exec(command),
        }
    }
}

/// Extract static environment variables from CUE config for one consumer.
///
/// Secrets (including interpolated values containing secrets) are excluded
/// from the returned map, and so are values whose policies do not allow the
/// consumer.
#[must_use]
pub fn extract_static_env_vars(
    config: &Project,
    consumer: StaticEnvConsumer<'_>,
) -> HashMap<String, String> {
    config
        .env
        .iter()
        .flat_map(|env| &env.base)
        .filter(|(_, value)| !value.is_secret() && consumer.may_receive(value))
        .map(|(key, value)| (key.clone(), value.to_string_value()))
        .collect()
}

/// Collect all environment variables (static + hook-generated)
pub(super) fn collect_all_env_vars(
    config: &Project,
    hook_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut all_vars = extract_static_env_vars(config, StaticEnvConsumer::Shell);

    // Hook environment variables override static ones
    for (key, value) in hook_env {
        all_vars.insert(key.clone(), value.clone());
    }

    all_vars
}

/// Generate script to print "Loaded" message if entering a new directory
fn format_loaded_check(dir: &Path, shell: Shell) -> String {
    let dir_display = dir.to_string_lossy();
    let escaped_dir = escape_shell_value(&dir_display);
    let loaded = LOADED_DIR_ENV;
    let pending = PENDING_APPROVAL_ENV;

    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r#"if builtin [ "${{{loaded}:-}}" != "{escaped_dir}" ]; then
    echo "Project environment loaded" >&2
    builtin export {loaded}="{escaped_dir}"
    builtin unset {pending} 2>/dev/null
fi"#
        ),
        Shell::Fish => {
            let fish_dir = escape_fish_value(&dir_display);
            format!(
                r#"if not set -q {loaded}
    echo "Project environment loaded" >&2
    set -gx {loaded} '{fish_dir}'
    if set -q -g {pending}; set -e -g {pending}; end
else if test "${loaded}" != '{fish_dir}'
    echo "Project environment loaded" >&2
    set -gx {loaded} '{fish_dir}'
    if set -q -g {pending}; set -e -g {pending}; end
end"#
            )
        }
        Shell::PowerShell => {
            let ps_dir = escape_powershell_value(&dir_display);
            format!(
                r"if ($env:{loaded} -ne '{ps_dir}') {{
    Write-Host 'Project environment loaded'
    $env:{loaded} = '{ps_dir}'
    Remove-Item Env:{pending} -ErrorAction SilentlyContinue
}}"
            )
        }
    }
}

/// Format environment variables as shell export commands
fn format_env_diff(dir: &Path, env: HashMap<String, String>, shell: Shell) -> Result<String> {
    let mut output = render_shell_assignments(&env, shell)?;

    // Record the loaded project only after its assignments succeed.
    output.push_str(&format_loaded_check(dir, shell));
    output.push('\n');

    Ok(output)
}

#[cfg(test)]
struct EnvironmentDifference<'environment_difference> {
    current: HashMap<String, String>,
    previous: Option<&'environment_difference HashMap<String, String>>,
}

/// Format environment diff with unset commands for removed variables
#[cfg(test)]
fn format_env_diff_with_unset(
    dir: &Path,
    difference: EnvironmentDifference<'_>,
    shell: Shell,
) -> Result<String> {
    use std::fmt::Write;
    let EnvironmentDifference {
        current: current_env,
        previous: previous_env,
    } = difference;
    validate_shell_environment(&current_env)?;
    if let Some(previous) = previous_env {
        validate_shell_environment(previous)?;
    }
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
    output.push_str(&render_shell_assignments(&current_env, shell)?);
    Ok(output)
}

pub(super) fn is_allowed_shell_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let valid = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let upper = name.to_ascii_uppercase();
    let reserved = upper.starts_with("__CUENV_")
        || upper.starts_with("BASH_")
        || upper.starts_with("ZSH_")
        || upper.starts_with("FISH_")
        || matches!(
            upper.as_str(),
            "CUENV_LOADED_DIR"
                | "CUENV_PENDING_APPROVAL_DIR"
                | "CUENV_MANAGED_VARS"
                | "CUENV_SHELL_INTEGRATION"
                | "CUENV_HOOK_SESSION"
                | "FISH_TRACE"
                | "PS4"
                | "BASH_ENV"
                | "ENV"
                | "SHELLOPTS"
                | "BASHOPTS"
                | "BASH_XTRACEFD"
                | "BASH_SUBSHELL"
                | "ZSH_SUBSHELL"
                | "BASHPID"
                | "ZDOTDIR"
                | "PWD"
                | "OLDPWD"
                | "LINENO"
                | "SECONDS"
                | "RANDOM"
                | "SRANDOM"
                | "EPOCHSECONDS"
                | "EPOCHREALTIME"
                | "SHLVL"
                | "PPID"
                | "UID"
                | "EUID"
                | "GROUPS"
                | "FUNCNAME"
                | "ARGV"
                | "STATUS"
                | "PIPESTATUS"
                | "HISTORY"
                | "VERSION"
                | "PROMPT_COMMAND"
                | "PS1"
                | "PS2"
                | "PS3"
                | "OPTIND"
                | "OPTARG"
                | "IFS"
                | "_"
        );
    valid && !reserved
}

fn validate_shell_environment(env: &HashMap<String, String>) -> Result<()> {
    for (name, value) in env {
        if !is_allowed_shell_name(name) || value.contains('\0') {
            return Err(cuenv_core::Error::configuration(
                "Environment contains an invalid or reserved shell variable",
            ));
        }
    }
    Ok(())
}

fn format_pending(shell: Shell) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => ":".to_string(),
        Shell::Fish => "true".to_string(),
        Shell::PowerShell => "# pending".to_string(),
    }
}

fn shell_state() -> Result<Option<super::shell_state::ShellState>> {
    match std::env::var(MANAGED_VARS_ENV) {
        Ok(token) => super::shell_state::ShellState::parse(&token).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(cuenv_core::Error::configuration(
            "Shell restoration state is invalid; start a fresh shell or explicitly clear the inherited environment",
        )),
    }
}

pub(in crate::commands) fn preflight_shell_transition(
    directory: Option<&Path>,
    package: &str,
    shell: Shell,
) -> Result<Option<String>> {
    let Some(state) = shell_state()? else {
        return Ok(None);
    };
    if std::env::var("CUENV_SHELL_INTEGRATION").as_deref() != Ok("1") {
        return Err(cuenv_core::Error::configuration(
            "Shell restoration data belongs to another shell; start a fresh shell or explicitly clear the inherited environment",
        ));
    }
    let target = directory
        .map(|path| super::shell_state::ShellState::for_project(path, package))
        .transpose()?;
    if target.as_deref() == Some(state.project.as_str()) {
        return Ok(None);
    }
    if matches!(shell, Shell::PowerShell) {
        return Err(cuenv_core::Error::configuration(
            "This shell cannot restore an inherited cuenv environment",
        ));
    }
    let retry = usize::from(target.is_some());
    Ok(Some(format!(
        "__cuenv_unload '{}' {retry}\n",
        state.token()?
    )))
}

pub(in crate::commands) struct EnvironmentContext<'context> {
    pub(in crate::commands) directory: &'context Path,
    pub(in crate::commands) package: &'context str,
    pub(in crate::commands) shell: Shell,
}

pub(in crate::commands) fn format_environment(
    context: EnvironmentContext<'_>,
    env: HashMap<String, String>,
) -> Result<String> {
    let EnvironmentContext {
        directory,
        package,
        shell,
    } = context;
    if std::env::var("CUENV_SHELL_INTEGRATION").as_deref() != Ok("1") {
        return format_env_diff(directory, env, shell);
    }
    validate_shell_environment(&env)?;
    if matches!(shell, Shell::PowerShell) {
        return Err(cuenv_core::Error::configuration(
            "PowerShell shell integration is not available",
        ));
    }
    let project = super::shell_state::ShellState::for_project(directory, package)?;
    let old = shell_state()?;
    if old.as_ref().is_some_and(|state| state.project != project) {
        return Err(cuenv_core::Error::configuration(
            "Shell environment must be restored before loading another project",
        ));
    }
    let nonce = old
        .as_ref()
        .map_or_else(uuid::Uuid::new_v4, |state| state.nonce);
    let expected = old
        .as_ref()
        .map_or_else(|| Ok(String::new()), super::shell_state::ShellState::token)?;
    let mut names: Vec<_> = env.keys().cloned().collect();
    names.sort();
    let mut union = old
        .as_ref()
        .map_or_else(Vec::new, |state| state.names.clone());
    for name in &names {
        if !union.contains(name) {
            union.push(name.clone());
        }
    }
    super::shell_state::ShellState {
        nonce,
        project: project.clone(),
        names: union,
    }
    .token()?;
    use std::fmt::Write;
    let mut output = format!("__cuenv_apply '{expected}' '{project}' '{nonce}'");
    for name in names {
        let value = &env[&name];
        match shell {
            Shell::Bash | Shell::Zsh => {
                let _ = write!(output, " '{name}' \"{}\"", escape_shell_value(value));
            }
            Shell::Fish => {
                let _ = write!(output, " '{name}' '{}'", escape_fish_value(value));
            }
            Shell::PowerShell => unreachable!("PowerShell integration was rejected"),
        }
    }
    let loaded = format_loaded_check(directory, shell);
    match shell {
        Shell::Bash | Shell::Zsh => Ok(format!(
            "if {output}; then\n{loaded}\nelse\n    false\nfi\n"
        )),
        Shell::Fish => Ok(format!("if {output}\n{loaded}\nelse\n    false\nend\n")),
        Shell::PowerShell => Err(cuenv_core::Error::configuration(
            "PowerShell shell integration is not available",
        )),
    }
}

pub(in crate::commands) fn render_shell_assignments(
    env: &HashMap<String, String>,
    shell: Shell,
) -> Result<String> {
    use std::fmt::Write;

    validate_shell_environment(env)?;
    if cfg!(windows) && matches!(shell, Shell::PowerShell) {
        let mut names = std::collections::HashSet::new();
        if env
            .keys()
            .any(|name| !names.insert(name.to_ascii_uppercase()))
        {
            return Err(cuenv_core::Error::configuration(
                "Environment contains names that collide on Windows",
            ));
        }
    }
    let mut variables: Vec<_> = env.iter().collect();
    variables.sort_by_key(|(name, _)| *name);
    let mut output = String::new();
    if matches!(shell, Shell::PowerShell) && env.values().any(String::is_empty) {
        output.push_str(
            "if ($PSVersionTable.PSVersion -lt [version]'7.5') { throw 'PowerShell 7.5 or later is required to export empty environment values' }\n",
        );
    }
    for (key, value) in variables {
        match shell {
            Shell::Bash | Shell::Zsh => {
                let escaped = escape_shell_value(value);
                let _ = writeln!(&mut output, "builtin export {key}=\"{escaped}\"");
            }
            Shell::Fish => {
                let escaped = escape_fish_value(value);
                let _ = writeln!(&mut output, "set -gx {key} '{escaped}'");
            }
            Shell::PowerShell => {
                let escaped = escape_powershell_value(value);
                let _ = writeln!(&mut output, "$env:{key} = '{escaped}'");
            }
        }
    }
    Ok(output)
}

/// Format "not allowed" message as an executable script snippet per shell.
/// The script prints a helpful notice once per directory until approval is granted.
fn format_not_allowed(dir: &Path, shell: Shell, hook_count: usize) -> String {
    let dir_display = dir.to_string_lossy();
    let escaped = escape_shell_value(&dir_display);
    let hooks_str = if hook_count == 1 { "hook" } else { "hooks" };
    let loaded = LOADED_DIR_ENV;
    let pending = PENDING_APPROVAL_ENV;
    let count = hook_count;
    let hooks = hooks_str;

    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r#"if builtin [ "${{{pending}:-}}" != "{escaped}" ]; then
    builtin printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    builtin printf '%s\n' "Run 'cuenv allow' to approve."
    builtin export {pending}="{escaped}"
    builtin unset {loaded} 2>/dev/null
fi
:"#
        ),
        Shell::Fish => {
            let pending_ref = format!("${PENDING_APPROVAL_ENV}");
            let fish_dir = escape_fish_value(&dir_display);
            format!(
                r#"if not set -q {pending}
    printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    printf '%s\n' "Run 'cuenv allow' to approve."
    set -gx {pending} '{fish_dir}'
    set -e {loaded} 2>/dev/null
else if test "{pending_ref}" != '{fish_dir}'
    printf '%s\n' "cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}."
    printf '%s\n' "Run 'cuenv allow' to approve."
    set -gx {pending} '{fish_dir}'
    set -e {loaded} 2>/dev/null
end
true"#
            )
        }
        Shell::PowerShell => {
            let ps_dir = escape_powershell_value(&dir_display);
            format!(
                r"if ($env:{pending} -ne '{ps_dir}') {{
    Write-Host 'cuenv detected env.cue, but approval is required because this configuration contains {count} {hooks}.'
    Write-Host 'Run ''cuenv allow'' to approve.'
    $env:{pending} = '{ps_dir}'
    Remove-Item Env:{loaded} -ErrorAction SilentlyContinue
}}"
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

fn escape_fish_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn escape_powershell_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        escaped.push(character);
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
        ) {
            escaped.push(character);
        }
    }
    escaped
}

/// Format a safe no-op command for the shell while clearing pending approval notices
fn format_no_op(shell: Shell) -> String {
    let loaded = LOADED_DIR_ENV;
    let pending = PENDING_APPROVAL_ENV;

    match shell {
        Shell::Bash | Shell::Zsh => format!(
            r"unset {pending} {loaded} 2>/dev/null
:"
        ),
        Shell::Fish => format!(
            r"if set -q {pending}
    set -e {pending}
end
if set -q {loaded}
    set -e {loaded}
end
true",
        ),
        Shell::PowerShell => format!(
            r"if (Test-Path Env:{pending}) {{ Remove-Item Env:{pending} }}
if (Test-Path Env:{loaded}) {{ Remove-Item Env:{loaded} }}
# no changes"
        ),
    }
}

#[cfg(test)]
#[path = "../export_tests.rs"]
mod tests;
