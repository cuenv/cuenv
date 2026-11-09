//! Hook execution and background state handling.

use crate::state::{HookExecutionIdentity, HookExecutionState, StateManager};
use crate::types::{
    ExecutionStatus, Hook, HookExecutionConfig, HookFailure, HookResult, HookSuccess, HookTimeout,
};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::source_environment;

/// A hook configuration as observed by one shell or CI session.
#[derive(Clone, Copy)]
pub struct HookSessionContext<'context> {
    /// Project directory containing the hooks.
    pub directory: &'context Path,
    /// Input-aware hook configuration identity.
    pub config_hash: &'context str,
    /// Non-nil version-4 identity for this shell or CI invocation.
    pub session_id: uuid::Uuid,
}

/// Manages hook execution with background processing and state persistence
#[derive(Debug)]
pub struct HookExecutor {
    pub(super) config: HookExecutionConfig,
    state_manager: StateManager,
}

impl HookExecutor {
    /// Create a new hook executor with the specified configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the default state directory cannot be initialized.
    pub fn try_new(config: HookExecutionConfig) -> Result<Self> {
        let state_manager = if let Some(dir) = config.state_dir.clone() {
            StateManager::new(dir)
        } else {
            StateManager::with_default_dir()?
        };

        Ok(Self {
            config,
            state_manager,
        })
    }

    /// Create a hook executor with default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the configured state directory is unavailable.
    pub fn with_default_config() -> Result<Self> {
        let mut config = HookExecutionConfig::default();

        // Use CUENV_STATE_DIR if set
        if let Some(state_dir) = std::env::var_os("CUENV_STATE_DIR") {
            config.state_dir = Some(PathBuf::from(state_dir));
        }

        Self::try_new(config)
    }

    /// Start executing hooks in the background for a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory is reserved, the run cannot be
    /// recorded, or the supervisor cannot receive its startup payload.
    pub async fn execute_hooks_background(
        &self,
        context: HookSessionContext<'_>,
        hooks: Vec<Hook>,
        mode: crate::HookStartMode,
    ) -> Result<String> {
        use tokio::io::AsyncWriteExt;

        if hooks.is_empty() {
            return Ok("No hooks to execute".to_string());
        }

        let mut config = self.config.clone();
        config.state_dir = None;
        let payload = crate::state::serialize_bounded(
            &crate::state::SupervisorPayloadRef {
                hooks: &hooks,
                config: &config,
            },
            crate::state::MAX_SUPERVISOR_PAYLOAD,
        )?;
        let payload_length = u32::try_from(payload.len())
            .map_err(|_| Error::configuration("Hook supervisor payload is too large"))?;
        self.state_manager.ensure_state_dir().await?;
        let state_dir =
            std::fs::canonicalize(self.state_manager.state_dir()).map_err(|source| Error::Io {
                source,
                path: Some(
                    self.state_manager
                        .state_dir()
                        .to_path_buf()
                        .into_boxed_path(),
                ),
                operation: "canonicalize hook state directory".to_string(),
            })?;
        let current_exe = if let Some(exe_path) = std::env::var_os("CUENV_EXECUTABLE") {
            PathBuf::from(exe_path)
        } else {
            std::env::current_exe()
                .map_err(|error| Error::process(format!("Failed to get current exe: {error}")))?
        };
        let directory_path = context.directory.to_path_buf();
        let config_hash = context.config_hash.to_string();
        let instance_hash =
            crate::state::compute_instance_hash(&directory_path, &config_hash, context.session_id);
        let total_hooks = hooks.len();

        let mut state = HookExecutionState::new(
            HookExecutionIdentity {
                directory_path: directory_path.clone(),
                instance_hash,
                session_id: context.session_id,
                config_hash,
            },
            hooks.clone(),
        );
        let identity = match self
            .state_manager
            .begin_run_with_mode(&mut state, mode)
            .await?
        {
            crate::HookRunStart::Started(identity) => identity,
            crate::HookRunStart::AlreadyPresent => {
                return Ok("Hook execution is already available".to_string());
            }
        };

        let mut cmd = Command::new(&current_exe);
        cmd.arg("__hook-supervisor")
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--directory-key")
            .arg(&identity.directory_key)
            .arg("--instance-hash")
            .arg(&identity.instance_hash)
            .arg("--run-id")
            .arg(identity.run_id.to_string())
            .env_remove("CUENV_STATE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.env_remove("CUENV_HOOK_SESSION");

        // Pass through CUENV_APPROVAL_FILE if set
        if let Ok(approval_file) = std::env::var("CUENV_APPROVAL_FILE") {
            cmd.env("CUENV_APPROVAL_FILE", approval_file);
        }

        // Pass through RUST_LOG for debugging
        if let Ok(rust_log) = std::env::var("RUST_LOG") {
            cmd.env("RUST_LOG", rust_log);
        }

        // Platform-specific detachment configuration
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Windows-specific flags for detached process
            const DETACHED_PROCESS: u32 = 0x00000008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            cmd.as_std_mut()
                .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.fail_unstarted_run(&identity, &mut state, &error.to_string())
                    .await?;
                return Err(Error::process(format!(
                    "Failed to spawn supervisor: {error}"
                )));
            }
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::process("Supervisor input was not captured"))?;
        let delivery = async move {
            stdin.write_u32(payload_length).await?;
            stdin.write_all(&payload).await?;
            stdin.shutdown().await
        };
        match tokio::time::timeout(Duration::from_secs(10), delivery).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(Error::process("Failed to deliver supervisor payload")),
            Err(_) => return Err(Error::process("Timed out delivering supervisor payload")),
        }

        // The child is now properly detached

        info!("Spawned supervisor process for hook execution");

        Ok(format!(
            "Started execution of {} hooks in background",
            total_hooks
        ))
    }

    async fn fail_unstarted_run(
        &self,
        identity: &crate::HookRunIdentity,
        state: &mut HookExecutionState,
        message: &str,
    ) -> Result<()> {
        state.status = ExecutionStatus::Failed;
        state.error_message = Some(message.to_string());
        state.finished_at = Some(chrono::Utc::now());
        match self.state_manager.fail_start(identity, state).await? {
            crate::HookRunUpdate::CancelRequested => {
                self.state_manager
                    .acknowledge_cancellation(identity)
                    .await?;
            }
            crate::HookRunUpdate::Applied | crate::HookRunUpdate::Superseded => {}
        }
        Ok(())
    }

    /// Get the current execution status for a directory and session.
    ///
    /// # Errors
    ///
    /// Returns an error if stored hook state cannot be read.
    pub async fn execution_status(
        &self,
        directory_path: &Path,
        session_id: uuid::Uuid,
    ) -> Result<Option<HookExecutionState>> {
        // List all active states and find one matching this directory
        let states = self.state_manager.list_active_states().await?;
        for state in states {
            if state.directory_path == directory_path && state.session_id == session_id {
                return Ok(Some(state));
            }
        }
        Ok(None)
    }

    /// Get execution status for a specific session and configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if state is inconsistent or its broker cannot be read.
    pub async fn execution_status_for_instance(
        &self,
        context: HookSessionContext<'_>,
    ) -> Result<Option<HookExecutionState>> {
        self.state_manager
            .state_for_instance(context.directory, context.config_hash, context.session_id)
            .await
    }

    /// Fast check if any hooks are active for a directory (no config hash needed).
    /// This is the hot path for Starship - skips config hash computation entirely.
    /// Returns None if no hooks running, Some(state) if hooks active.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory, session, or state cannot be verified.
    pub async fn fast_status(
        &self,
        directory_path: &Path,
        session_id: uuid::Uuid,
    ) -> Result<Option<HookExecutionState>> {
        self.state_manager
            .fast_status(directory_path, session_id)
            .await
    }

    /// Get a reference to the state manager (for marker operations from execute_hooks)
    #[must_use]
    pub fn state_manager(&self) -> &StateManager {
        &self.state_manager
    }

    /// Synchronous fast status check - no tokio runtime required.
    /// This is the hot path for Starship/shell prompts when no async runtime is available.
    /// Returns None if no hooks running, Some(state) if hooks active.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory, session, or state cannot be verified.
    pub fn fast_status_sync(
        &self,
        directory_path: &Path,
        session_id: uuid::Uuid,
    ) -> Result<Option<HookExecutionState>> {
        self.state_manager
            .fast_status_sync(directory_path, session_id)
    }

    /// Wait for hook execution to complete, with optional timeout in seconds.
    ///
    /// # Errors
    ///
    /// Returns an error if the run is unavailable, inconsistent, or times out.
    pub async fn wait_for_completion(
        &self,
        context: HookSessionContext<'_>,
        timeout_seconds: Option<u64>,
    ) -> Result<HookExecutionState> {
        let poll_interval = Duration::from_millis(500);
        let start_time = Instant::now();

        loop {
            if let Some(state) = self
                .state_manager
                .state_for_instance(context.directory, context.config_hash, context.session_id)
                .await?
            {
                if state.is_complete() {
                    return Ok(state);
                }
            } else {
                return Err(Error::state_not_found(context.config_hash));
            }

            // Check timeout
            if let Some(timeout) = timeout_seconds
                && start_time.elapsed().as_secs() >= timeout
            {
                return Err(Error::Timeout { seconds: timeout });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Request cancellation of this session's execution for a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if its reservation cannot be checked or updated.
    pub async fn cancel_execution(
        &self,
        context: HookSessionContext<'_>,
        reason: Option<String>,
    ) -> Result<bool> {
        if context.session_id.is_nil() || context.session_id.get_version_num() != 4 {
            return Err(Error::configuration(
                "Hook session must be a non-nil version-4 UUID",
            ));
        }
        let Some(reservation) = self
            .state_manager
            .current_reservation(context.directory)
            .await?
        else {
            return Ok(false);
        };
        if reservation.session_id != context.session_id {
            return Ok(false);
        }
        match self
            .state_manager
            .request_cancellation(&reservation.identity, reason)
            .await?
        {
            crate::HookRunUpdate::CancelRequested => Ok(true),
            crate::HookRunUpdate::Superseded => Ok(false),
            crate::HookRunUpdate::Applied => {
                Err(Error::configuration("Unexpected cancellation outcome"))
            }
        }
    }

    /// Clean up completed execution states older than the specified duration.
    ///
    /// # Errors
    ///
    /// Returns an error if state cannot be inspected or safely removed.
    pub async fn cleanup_old_states(&self, older_than: chrono::Duration) -> Result<usize> {
        let states = self.state_manager.list_active_states().await?;
        let cutoff = chrono::Utc::now() - older_than;
        let mut cleaned_count = 0;

        for state in states {
            if state.is_complete()
                && let Some(finished_at) = state.finished_at
                && finished_at < cutoff
                && self.state_manager.remove_terminal_state(&state).await?
            {
                cleaned_count += 1;
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} old execution states", cleaned_count);
        }

        Ok(cleaned_count)
    }

    /// Execute a single hook and return the result.
    ///
    /// # Errors
    ///
    /// Returns an error if output cannot be read or the child cannot be reaped.
    pub async fn execute_single_hook(&self, hook: Hook) -> Result<HookResult> {
        // Use the default timeout from config
        let timeout = self.config.default_timeout_seconds;

        // No validation - users approved this config with cuenv allow
        execute_hook_with_timeout(hook, &timeout).await
    }
}

/// State storage and mutable state for a hook run.
pub struct HookRunState<'run> {
    /// Persistent state manager.
    pub manager: &'run StateManager,
    /// State updated while executing hooks.
    pub state: &'run mut HookExecutionState,
    /// Exact reservation owned by this worker.
    pub identity: &'run crate::HookRunIdentity,
}

/// Execute a joined hook run sequentially.
///
/// A successful run remains reserved so its caller can publish the in-memory
/// result or explicitly finish an invocation that does not need a cache.
///
/// # Errors
///
/// Returns an error if the reservation, child, or inputs cannot be handled safely.
pub async fn execute_hooks(
    hooks: Vec<Hook>,
    config: &HookExecutionConfig,
    run: HookRunState<'_>,
) -> Result<()> {
    let HookRunState {
        manager,
        state,
        identity,
    } = run;
    let hook_count = hooks.len();
    let mut failed = false;
    for (index, hook) in hooks.into_iter().enumerate() {
        match manager.run_status(identity).await? {
            crate::HookRunUpdate::Applied => {}
            crate::HookRunUpdate::CancelRequested => {
                return finish_cancellation(manager, identity, state).await;
            }
            crate::HookRunUpdate::Superseded => {
                return Err(Error::process("Hook run was superseded"));
            }
        }

        let base_context = ReservedHookContext {
            manager,
            identity,
            timeout_seconds: config.default_timeout_seconds,
            environment: None,
            shell: None,
            input: None,
        };
        let shell = if hook.source.unwrap_or(false) {
            match select_reserved_shell(&base_context).await? {
                ReservedShell::Selected(shell) => Some(shell),
                ReservedShell::Cancelled => {
                    return finish_cancellation(manager, identity, state).await;
                }
                ReservedShell::Superseded => return Err(Error::process("Hook run was superseded")),
            }
        } else {
            None
        };
        state.mark_hook_running(index);
        let result = execute_reserved_hook(
            hook.clone(),
            &ReservedHookContext {
                shell: shell.as_deref(),
                ..base_context
            },
        )
        .await?;
        let mut hook_result = match result {
            ReservedHookOutcome::Completed(result) => result,
            ReservedHookOutcome::Cancelled => {
                return finish_cancellation(manager, identity, state).await;
            }
            ReservedHookOutcome::Superseded => {
                return Err(Error::process("Hook run was superseded"));
            }
        };
        if hook.source.unwrap_or(false) && hook_result.success && !hook_result.stdout.is_empty() {
            let source = SourceEvaluation {
                script: &hook_result.stdout,
                prior_env: &state.environment_vars,
                shell: shell.as_deref().unwrap_or("sh"),
            };
            match evaluate_reserved_environment(source, &base_context).await {
                Ok(ReservedEnvironment::Captured(env_vars, removed_keys)) => {
                    state.environment_vars.extend(env_vars);
                    for key in removed_keys {
                        state.environment_vars.remove(&key);
                    }
                }
                Ok(ReservedEnvironment::Cancelled) => {
                    return finish_cancellation(manager, identity, state).await;
                }
                Ok(ReservedEnvironment::Superseded) => {
                    return Err(Error::process("Hook run was superseded"));
                }
                Err(error) => {
                    let message = format!("Failed to evaluate source hook output: {error}");
                    hook_result.success = false;
                    hook_result.error = Some(match hook_result.error.take() {
                        Some(existing) => format!("{existing}; {message}"),
                        None => message,
                    });
                }
            }
        }
        failed |= !hook_result.success;
        state.record_hook_result(index, &hook_result);
        if crate::state::validate_snapshot(identity, state).is_err() {
            state.status = ExecutionStatus::Failed;
            state.finished_at = Some(chrono::Utc::now());
            state.error_message = Some("Hook result exceeds the 16 MiB limit".to_string());
            state.environment_vars.clear();
            state.hook_results.clear();
            match manager.update_run(identity, state).await? {
                crate::HookRunUpdate::Applied => return Ok(()),
                crate::HookRunUpdate::CancelRequested => {
                    return finish_cancellation(manager, identity, state).await;
                }
                crate::HookRunUpdate::Superseded => {
                    return Err(Error::process("Hook run was superseded"));
                }
            }
        }
        let last = index + 1 == hook_count;
        if failed && (last || config.fail_fast) {
            state.status = ExecutionStatus::Failed;
            state.finished_at = Some(chrono::Utc::now());
        } else if !last {
            state.status = ExecutionStatus::Running;
            state.finished_at = None;
            state.completed_display_until = None;
        }
        validate_completed_inputs(state);
        if state.status == ExecutionStatus::Completed {
            return Ok(());
        }
        match manager.update_run(identity, state).await? {
            crate::HookRunUpdate::Applied => {}
            crate::HookRunUpdate::CancelRequested => {
                return finish_cancellation(manager, identity, state).await;
            }
            crate::HookRunUpdate::Superseded => {
                return Err(Error::process("Hook run was superseded"));
            }
        }
        if state.is_complete() {
            return Ok(());
        }
    }

    // An empty run has no child and can finish immediately.
    state.status = if failed {
        ExecutionStatus::Failed
    } else {
        ExecutionStatus::Completed
    };
    state.finished_at = Some(chrono::Utc::now());
    validate_completed_inputs(state);
    if state.status == ExecutionStatus::Completed {
        return Ok(());
    }
    match manager.update_run(identity, state).await? {
        crate::HookRunUpdate::Applied => Ok(()),
        crate::HookRunUpdate::CancelRequested => {
            finish_cancellation(manager, identity, state).await
        }
        crate::HookRunUpdate::Superseded => Err(Error::process("Hook run was superseded")),
    }
}

fn validate_completed_inputs(state: &mut HookExecutionState) {
    if state.status != ExecutionStatus::Completed {
        return;
    }
    let message = match crate::compute_execution_hash(&state.hooks, &state.directory_path) {
        Ok(hash) if hash == state.config_hash => return,
        Ok(_) => "Hook inputs changed during execution",
        Err(_) => "Hook inputs could not be validated after execution",
    };
    state.status = ExecutionStatus::Failed;
    state.finished_at = Some(chrono::Utc::now());
    state.error_message = Some(message.to_string());
    state.environment_vars.clear();
}

async fn finish_cancellation(
    manager: &StateManager,
    identity: &crate::HookRunIdentity,
    state: &mut HookExecutionState,
) -> Result<()> {
    match manager.acknowledge_cancellation(identity).await? {
        crate::HookRunUpdate::Applied => {
            let cancelled = manager
                .load_state(&identity.instance_hash)
                .await?
                .ok_or_else(|| Error::state_not_found(&identity.instance_hash))?;
            if cancelled.run_id != identity.run_id {
                return Err(Error::process(
                    "Hook run was superseded during cancellation",
                ));
            }
            *state = cancelled;
            Ok(())
        }
        crate::HookRunUpdate::Superseded => Err(Error::process("Hook run was superseded")),
        crate::HookRunUpdate::CancelRequested => Err(Error::process("Hook child has not stopped")),
    }
}

#[derive(Clone, Copy)]
struct ReservedHookContext<'run> {
    manager: &'run StateManager,
    identity: &'run crate::HookRunIdentity,
    timeout_seconds: u64,
    environment: Option<&'run std::collections::HashMap<String, String>>,
    shell: Option<&'run str>,
    input: Option<&'run [u8]>,
}

enum ReservedHookOutcome {
    Completed(HookResult),
    Cancelled,
    Superseded,
}

enum ReservedShell {
    Selected(String),
    Cancelled,
    Superseded,
}

enum ReservedEnvironment {
    Captured(std::collections::HashMap<String, String>, Vec<String>),
    Cancelled,
    Superseded,
}

struct SourceEvaluation<'source> {
    script: &'source str,
    prior_env: &'source std::collections::HashMap<String, String>,
    shell: &'source str,
}

fn shell_command(shell: &str, script: String) -> Hook {
    Hook {
        order: 100,
        propagate: false,
        command: shell.to_string(),
        args: vec!["-c".to_string(), script],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    }
}

async fn select_reserved_shell(context: &ReservedHookContext<'_>) -> Result<ReservedShell> {
    for shell in ["bash", "zsh"] {
        let probe = shell_command(shell, "case x in x) true ;& y) true ;; esac".to_string());
        match execute_reserved_hook(probe, context).await? {
            ReservedHookOutcome::Completed(result) if result.success => {
                return Ok(ReservedShell::Selected(shell.to_string()));
            }
            ReservedHookOutcome::Completed(_) => {}
            ReservedHookOutcome::Cancelled => return Ok(ReservedShell::Cancelled),
            ReservedHookOutcome::Superseded => return Ok(ReservedShell::Superseded),
        }
    }
    Ok(ReservedShell::Selected("sh".to_string()))
}

async fn evaluate_reserved_environment(
    source: SourceEvaluation<'_>,
    context: &ReservedHookContext<'_>,
) -> Result<ReservedEnvironment> {
    if source
        .prior_env
        .keys()
        .any(|key| !source_environment::is_valid_environment_name(key))
    {
        return Err(Error::configuration(
            "Prior environment contains an invalid variable name",
        ));
    }
    let delimiter = format!("__CUENV_ENV_START_{}__", uuid::Uuid::new_v4().simple());
    let shell = source_environment::nix_bash_from_script(source.script).map_or_else(
        || source.shell.to_string(),
        |path| path.to_string_lossy().into_owned(),
    );
    let env_command = source_environment::shell_quote(&source_environment::find_env_command());
    let source_context = ReservedHookContext {
        environment: Some(source.prior_env),
        shell: None,
        ..*context
    };
    let baseline = shell_command(&shell, format!("{env_command} -0"));
    let baseline = match execute_reserved_hook(baseline, &source_context).await? {
        ReservedHookOutcome::Completed(result) if result.success => result,
        ReservedHookOutcome::Completed(_) => {
            return Err(Error::configuration(
                "Failed to capture baseline environment",
            ));
        }
        ReservedHookOutcome::Cancelled => return Ok(ReservedEnvironment::Cancelled),
        ReservedHookOutcome::Superseded => return Ok(ReservedEnvironment::Superseded),
    };
    let before = source_environment::parse_env_output(baseline.stdout.as_bytes());
    let filtered_script = source_environment::filter_shell_script(source.script);
    if filtered_script.contains('\0') {
        return Err(Error::configuration("Source script contains a null byte"));
    }
    let wrapper = source_environment::source_evaluation_command(
        &delimiter,
        &source_environment::find_env_command(),
    );
    let command = shell_command(&shell, wrapper);
    let evaluated = match execute_reserved_hook(
        command,
        &ReservedHookContext {
            input: Some(filtered_script.as_bytes()),
            ..source_context
        },
    )
    .await?
    {
        ReservedHookOutcome::Completed(result) => result,
        ReservedHookOutcome::Cancelled => return Ok(ReservedEnvironment::Cancelled),
        ReservedHookOutcome::Superseded => return Ok(ReservedEnvironment::Superseded),
    };
    if !evaluated.success {
        return Err(Error::configuration("Source shell evaluation failed"));
    }
    let bytes =
        source_environment::environment_output_bytes(evaluated.stdout.as_bytes(), &delimiter)
            .ok_or_else(|| {
                Error::configuration("Source shell did not finish environment capture")
            })?;
    let (delta, removed) =
        source_environment::parse_environment_delta(bytes, &before, source.prior_env)?;
    Ok(ReservedEnvironment::Captured(delta, removed))
}

async fn execute_reserved_hook(
    hook: Hook,
    context: &ReservedHookContext<'_>,
) -> Result<ReservedHookOutcome> {
    use tokio::io::AsyncWriteExt;

    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(context.timeout_seconds);
    let mut command = Command::new(&hook.command);
    command
        .args(&hook.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if context.input.is_some() {
        command.stdin(Stdio::piped());
    }
    if let Some(directory) = &hook.dir {
        command.current_dir(directory);
    }
    if let Some(environment) = context.environment {
        command.envs(environment);
    }
    if let Some(shell) = context.shell {
        command.env("SHELL", shell);
    }
    command.env_remove("CUENV_HOOK_SESSION");
    let mut child = match context
        .manager
        .spawn_run_child(context.identity, command)
        .await?
    {
        crate::state::HookChildStart::Started(child) => child,
        crate::state::HookChildStart::Failed(error) => {
            return Ok(ReservedHookOutcome::Completed(HookResult::failure(
                HookFailure {
                    hook,
                    exit_status: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: duration_millis(started.elapsed()),
                    error: format!("Failed to execute command: {error}"),
                },
            )));
        }
        crate::state::HookChildStart::CancelRequested => return Ok(ReservedHookOutcome::Cancelled),
        crate::state::HookChildStart::Superseded => return Ok(ReservedHookOutcome::Superseded),
    };
    let Some(pid) = child.id() else {
        super::output::stop_child(&mut child).await?;
        return Err(Error::process(
            "Started hook child has no PID; explicit recovery is required",
        ));
    };

    let input_task = if let Some(input) = context.input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::process("Hook input was not captured"))?;
        let input = input.to_vec();
        Some(tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        }))
    } else {
        None
    };
    let input_abort = input_task
        .as_ref()
        .map(tokio::task::JoinHandle::abort_handle);
    let abort_input = || {
        if let Some(abort) = &input_abort {
            abort.abort();
        }
    };
    let mut readers = super::output::OutputReaders::new(&mut child)?;
    let (stdout_abort, stderr_abort) = readers.abort_handles();
    let mut timed_out = false;
    let status = loop {
        tokio::select! {
            result = child.wait() => break Some(result.map_err(|error| Error::process(format!("Failed to wait for hook child: {error}")))?),
            Some(()) = readers.failed() => {
                super::output::stop_child(&mut child).await?;
                abort_input();
                let reaped = context.manager.record_child(context.identity, crate::HookChildUpdate::Reaped(pid)).await?;
                return match reaped {
                    crate::HookRunUpdate::Applied => Err(Error::process("Hook output could not be captured within the limit")),
                    crate::HookRunUpdate::CancelRequested => Ok(ReservedHookOutcome::Cancelled),
                    crate::HookRunUpdate::Superseded => Ok(ReservedHookOutcome::Superseded),
                };
            }
            () = tokio::time::sleep_until(deadline) => {
                super::output::stop_child(&mut child).await?;
                timed_out = true;
                break None;
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {
                match context.manager.run_status(context.identity).await {
                    Ok(crate::HookRunUpdate::Applied) => {}
                    outcome => {
                        super::output::stop_child(&mut child).await?;
                        abort_input();
                        stdout_abort.abort();
                        stderr_abort.abort();
                        let reaped = context.manager.record_child(context.identity, crate::HookChildUpdate::Reaped(pid)).await?;
                        return match outcome {
                            Ok(crate::HookRunUpdate::CancelRequested) if reaped != crate::HookRunUpdate::Superseded => Ok(ReservedHookOutcome::Cancelled),
                            Ok(_) => Ok(ReservedHookOutcome::Superseded),
                            Err(error) => Err(error),
                        };
                    }
                }
            }
        }
    };
    let reaped = context
        .manager
        .record_child(context.identity, crate::HookChildUpdate::Reaped(pid))
        .await?;
    if reaped != crate::HookRunUpdate::Applied {
        abort_input();
        stdout_abort.abort();
        stderr_abort.abort();
        return Ok(if reaped == crate::HookRunUpdate::CancelRequested {
            ReservedHookOutcome::Cancelled
        } else {
            ReservedHookOutcome::Superseded
        });
    }
    if timed_out {
        abort_input();
        stdout_abort.abort();
        stderr_abort.abort();
        return Ok(ReservedHookOutcome::Completed(HookResult::timeout(
            HookTimeout {
                hook,
                stdout: String::new(),
                stderr: String::new(),
                timeout_seconds: context.timeout_seconds,
            },
        )));
    }
    let output = async {
        if let Some(input_task) = input_task {
            input_task
                .await
                .map_err(|_| Error::process("Hook input task failed"))?
                .map_err(|_| Error::process("Failed to deliver hook input"))?;
        }
        readers.collect().await
    };
    tokio::pin!(output);
    let (stdout, stderr) = loop {
        tokio::select! {
            result = &mut output => break result?,
            () = tokio::time::sleep_until(deadline) => {
                abort_input();
                stdout_abort.abort();
                stderr_abort.abort();
                return Ok(ReservedHookOutcome::Completed(HookResult::timeout(HookTimeout {
                    hook, stdout: String::new(), stderr: String::new(), timeout_seconds: context.timeout_seconds,
                })));
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {
                match context.manager.run_status(context.identity).await? {
                    crate::HookRunUpdate::Applied => {}
                    crate::HookRunUpdate::CancelRequested => {
                        abort_input();
                        stdout_abort.abort();
                        stderr_abort.abort();
                        return Ok(ReservedHookOutcome::Cancelled);
                    }
                    crate::HookRunUpdate::Superseded => {
                        abort_input();
                        stdout_abort.abort();
                        stderr_abort.abort();
                        return Ok(ReservedHookOutcome::Superseded);
                    }
                }
            }
        }
    };
    let status = status.ok_or_else(|| Error::process("Hook exited without a status"))?;
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let duration_ms = duration_millis(started.elapsed());
    let result = if status.success() {
        HookResult::success(HookSuccess {
            hook,
            exit_status: status,
            stdout,
            stderr,
            duration_ms,
        })
    } else {
        HookResult::failure(HookFailure {
            hook,
            exit_status: Some(status),
            stdout,
            stderr,
            duration_ms,
            error: format!("Command exited with status: {status}"),
        })
    };
    Ok(ReservedHookOutcome::Completed(result))
}

/// Execute a single hook with timeout
pub(super) async fn execute_hook_with_timeout(
    hook: Hook,
    timeout_seconds: &u64,
) -> Result<HookResult> {
    let start_time = Instant::now();
    debug!("Executing hook (source: {})", hook.source.unwrap_or(false));

    let mut command = Command::new(&hook.command);
    command.args(&hook.args);
    command.env_remove("CUENV_HOOK_SESSION");
    if let Some(directory) = &hook.dir {
        command.current_dir(directory);
    }
    if hook.source.unwrap_or(false) {
        command.env("SHELL", source_environment::detect_shell().await);
    }

    let output = match super::output::run_command(
        command,
        None,
        Duration::from_secs(*timeout_seconds),
    )
    .await
    {
        Ok(Some(output)) => output,
        Ok(None) => {
            warn!("Hook timed out after {} seconds", timeout_seconds);
            return Ok(HookResult::timeout(HookTimeout {
                hook,
                stdout: String::new(),
                stderr: String::new(),
                timeout_seconds: *timeout_seconds,
            }));
        }
        Err(Error::ProcessStart) => {
            return Ok(HookResult::failure(HookFailure {
                hook,
                exit_status: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: duration_millis(start_time.elapsed()),
                error: "Failed to execute command".to_string(),
            }));
        }
        Err(error) => return Err(error),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let duration_ms = duration_millis(start_time.elapsed());
    if output.status.success() {
        Ok(HookResult::success(HookSuccess {
            hook,
            exit_status: output.status,
            stdout,
            stderr,
            duration_ms,
        }))
    } else {
        Ok(HookResult::failure(HookFailure {
            hook,
            exit_status: Some(output.status),
            stdout,
            stderr,
            duration_ms,
            error: format!("Command exited with status: {}", output.status),
        }))
    }
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
