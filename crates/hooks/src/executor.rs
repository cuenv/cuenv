//! Hook execution engine with background processing and state management

use crate::state::{HookExecutionState, StateManager, compute_instance_hash};
use crate::types::{ExecutionStatus, Hook, HookExecutionConfig, HookFailure, HookResult};
use crate::{Error, Result};
#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

mod source_environment;

pub use source_environment::capture_source_environment;
use source_environment::{detect_shell, evaluate_shell_environment};

/// Manages hook execution with background processing and state persistence
#[derive(Debug)]
pub struct HookExecutor {
    config: HookExecutionConfig,
    state_manager: StateManager,
}

impl HookExecutor {
    /// Create a new hook executor with the specified configuration
    pub fn new(config: HookExecutionConfig) -> Result<Self> {
        let state_dir = if let Some(dir) = config.state_dir.clone() {
            dir
        } else {
            StateManager::default_state_dir()?
        };

        let state_manager = StateManager::new(state_dir);

        Ok(Self {
            config,
            state_manager,
        })
    }

    /// Create a hook executor with default configuration
    pub fn with_default_config() -> Result<Self> {
        let mut config = HookExecutionConfig::default();

        // Use CUENV_STATE_DIR if set
        if let Ok(state_dir) = std::env::var("CUENV_STATE_DIR") {
            config.state_dir = Some(PathBuf::from(state_dir));
        }

        Self::new(config)
    }

    /// Start executing hooks in the background for a directory
    pub async fn execute_hooks_background(
        &self,
        directory_path: PathBuf,
        config_hash: String,
        hooks: Vec<Hook>,
    ) -> Result<String> {
        use std::process::{Command, Stdio};

        if hooks.is_empty() {
            return Ok("No hooks to execute".to_string());
        }

        let instance_hash = compute_instance_hash(&directory_path, &config_hash);
        let total_hooks = hooks.len();

        // Check for existing state to preserve previous environment
        let previous_env =
            if let Ok(Some(existing_state)) = self.state_manager.load_state(&instance_hash).await {
                // If we have a completed state, save its environment as previous
                if existing_state.status == ExecutionStatus::Completed {
                    Some(existing_state.environment_vars.clone())
                } else {
                    existing_state.previous_env
                }
            } else {
                None
            };

        // Create initial execution state with previous environment
        let mut state = HookExecutionState::new(
            directory_path.clone(),
            instance_hash.clone(),
            config_hash.clone(),
            hooks.clone(),
        );
        state.previous_env = previous_env;

        // Save initial state
        self.state_manager.save_state(&state).await?;

        // Create directory marker for fast status lookups
        self.state_manager
            .create_directory_marker(&directory_path, &instance_hash)
            .await?;

        info!(
            "Starting background execution of {} hooks for directory: {}",
            total_hooks,
            directory_path.display()
        );

        // Check if a supervisor is already running for this instance
        let pid_file = self
            .state_manager
            .get_state_file_path(&instance_hash)
            .with_extension("pid");

        if pid_file.exists() {
            // Read the PID and check if process is still running
            if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = pid_str.trim().parse::<usize>()
            {
                // Check if process is still alive using sysinfo
                use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
                let mut system = System::new();
                let process_pid = Pid::from(pid);
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[process_pid]),
                    false,
                    ProcessRefreshKind::nothing(),
                );

                if system.process(process_pid).is_some() {
                    info!("Supervisor already running for directory with PID {}", pid);
                    return Ok(format!(
                        "Supervisor already running for {} hooks (PID: {})",
                        total_hooks, pid
                    ));
                }
            }
            // If we get here, the PID file exists but process is dead
            std::fs::remove_file(&pid_file).ok();
        }

        // Write hooks and config to temp files to avoid argument size limits
        let state_dir = self.state_manager.get_state_dir();
        let hooks_file = state_dir.join(format!("{}_hooks.json", instance_hash));
        let config_file = state_dir.join(format!("{}_config.json", instance_hash));

        // Serialize and write hooks
        let hooks_json = serde_json::to_string(&hooks)
            .map_err(|e| Error::serialization(format!("Failed to serialize hooks: {}", e)))?;
        std::fs::write(&hooks_file, &hooks_json).map_err(|e| Error::Io {
            source: e,
            path: Some(hooks_file.clone().into_boxed_path()),
            operation: "write".to_string(),
        })?;

        // Serialize and write config
        let config_json = serde_json::to_string(&self.config)
            .map_err(|e| Error::serialization(format!("Failed to serialize config: {}", e)))?;
        std::fs::write(&config_file, &config_json).map_err(|e| Error::Io {
            source: e,
            path: Some(config_file.clone().into_boxed_path()),
            operation: "write".to_string(),
        })?;

        // Get the executable path to spawn as supervisor
        // Allow override via CUENV_EXECUTABLE for testing
        let current_exe = if let Ok(exe_path) = std::env::var("CUENV_EXECUTABLE") {
            PathBuf::from(exe_path)
        } else {
            std::env::current_exe()
                .map_err(|e| Error::process(format!("Failed to get current exe: {}", e)))?
        };

        // Spawn a detached supervisor process
        let mut cmd = Command::new(&current_exe);
        cmd.arg("__hook-supervisor") // Special hidden command
            .arg("--directory")
            .arg(directory_path.to_string_lossy().to_string())
            .arg("--instance-hash")
            .arg(&instance_hash)
            .arg("--config-hash")
            .arg(&config_hash)
            .arg("--hooks-file")
            .arg(hooks_file.to_string_lossy().to_string())
            .arg("--config-file")
            .arg(config_file.to_string_lossy().to_string())
            .stdin(Stdio::null());

        // Redirect output to log files for debugging
        let temp_dir = std::env::temp_dir();
        let log_file = std::fs::File::create(temp_dir.join("cuenv_supervisor.log")).ok();
        let err_file = std::fs::File::create(temp_dir.join("cuenv_supervisor_err.log")).ok();

        if let Some(log) = log_file {
            cmd.stdout(Stdio::from(log));
        } else {
            cmd.stdout(Stdio::null());
        }

        if let Some(err) = err_file {
            cmd.stderr(Stdio::from(err));
        } else {
            cmd.stderr(Stdio::null());
        }

        // Pass through CUENV_STATE_DIR if set
        if let Ok(state_dir) = std::env::var("CUENV_STATE_DIR") {
            cmd.env("CUENV_STATE_DIR", state_dir);
        }

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
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }

        let _child = cmd
            .spawn()
            .map_err(|e| Error::process(format!("Failed to spawn supervisor: {}", e)))?;

        // The child is now properly detached

        info!("Spawned supervisor process for hook execution");

        Ok(format!(
            "Started execution of {} hooks in background",
            total_hooks
        ))
    }

    /// Get the current execution status for a directory
    pub async fn get_execution_status(
        &self,
        directory_path: &Path,
    ) -> Result<Option<HookExecutionState>> {
        // List all active states and find one matching this directory
        let states = self.state_manager.list_active_states().await?;
        for state in states {
            if state.directory_path == directory_path {
                return Ok(Some(state));
            }
        }
        Ok(None)
    }

    /// Get execution status for a specific instance (directory + config)
    pub async fn get_execution_status_for_instance(
        &self,
        directory_path: &Path,
        config_hash: &str,
    ) -> Result<Option<HookExecutionState>> {
        let instance_hash = compute_instance_hash(directory_path, config_hash);
        self.state_manager.load_state(&instance_hash).await
    }

    /// Fast check if any hooks are active for a directory (no config hash needed).
    /// This is the hot path for Starship - skips config hash computation entirely.
    /// Returns None if no hooks running, Some(state) if hooks active.
    pub async fn get_fast_status(
        &self,
        directory_path: &Path,
    ) -> Result<Option<HookExecutionState>> {
        // First check: does marker exist? O(1) filesystem stat
        if !self.state_manager.has_active_marker(directory_path) {
            return Ok(None);
        }

        // Marker exists - get instance hash and load state
        if let Some(instance_hash) = self
            .state_manager
            .get_marker_instance_hash(directory_path)
            .await
        {
            let state = self.state_manager.load_state(&instance_hash).await?;

            match &state {
                Some(s) if s.is_complete() && !s.should_display_completed() => {
                    // State is complete and expired, clean up marker
                    self.state_manager
                        .remove_directory_marker(directory_path)
                        .await
                        .ok();
                    return Ok(None);
                }
                None => {
                    // State file was deleted but marker exists - clean up orphaned marker
                    self.state_manager
                        .remove_directory_marker(directory_path)
                        .await
                        .ok();
                    return Ok(None);
                }
                Some(_) => return Ok(state),
            }
        }

        Ok(None)
    }

    /// Get a reference to the state manager (for marker operations from execute_hooks)
    #[must_use]
    pub fn state_manager(&self) -> &StateManager {
        &self.state_manager
    }

    /// Synchronous fast status check - no tokio runtime required.
    /// This is the hot path for Starship/shell prompts when no async runtime is available.
    /// Returns None if no hooks running, Some(state) if hooks active.
    pub fn get_fast_status_sync(
        &self,
        directory_path: &Path,
    ) -> Result<Option<HookExecutionState>> {
        // First check: does marker exist? O(1) filesystem stat
        if !self.state_manager.has_active_marker(directory_path) {
            return Ok(None);
        }

        // Marker exists - get instance hash and load state synchronously
        if let Some(instance_hash) = self
            .state_manager
            .get_marker_instance_hash_sync(directory_path)
        {
            let state = self.state_manager.load_state_sync(&instance_hash)?;

            match &state {
                Some(s) if s.is_complete() && !s.should_display_completed() => {
                    // State is complete and expired - for sync path, just return None
                    // (async cleanup will happen on next async call)
                    return Ok(None);
                }
                None => {
                    // State file was deleted but marker exists - return None
                    // (async cleanup will happen on next async call)
                    return Ok(None);
                }
                Some(_) => return Ok(state),
            }
        }

        Ok(None)
    }

    /// Wait for hook execution to complete, with optional timeout in seconds
    pub async fn wait_for_completion(
        &self,
        directory_path: &Path,
        config_hash: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<HookExecutionState> {
        let instance_hash = compute_instance_hash(directory_path, config_hash);
        let poll_interval = Duration::from_millis(500);
        let start_time = Instant::now();

        loop {
            if let Some(state) = self.state_manager.load_state(&instance_hash).await? {
                if state.is_complete() {
                    return Ok(state);
                }
            } else {
                return Err(Error::state_not_found(&instance_hash));
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

    /// Cancel execution for a directory
    pub async fn cancel_execution(
        &self,
        directory_path: &Path,
        config_hash: &str,
        reason: Option<String>,
    ) -> Result<bool> {
        let instance_hash = compute_instance_hash(directory_path, config_hash);

        // Try to kill the supervisor process if it exists
        let pid_file = self
            .state_manager
            .get_state_file_path(&instance_hash)
            .with_extension("pid");

        if pid_file.exists()
            && let Ok(pid_str) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid_str.trim().parse::<usize>()
        {
            use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

            let mut system = System::new();
            let process_pid = Pid::from(pid);

            // Refresh the specific process
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[process_pid]),
                false,
                ProcessRefreshKind::nothing(),
            );

            // Check if process exists and kill it
            if let Some(process) = system.process(process_pid) {
                if process.kill_with(Signal::Term).is_some() {
                    info!("Sent SIGTERM to supervisor process PID {}", pid);
                } else {
                    warn!("Failed to send SIGTERM to supervisor process PID {}", pid);
                }
            } else {
                info!(
                    "Supervisor process PID {} not found (may have already exited)",
                    pid
                );
            }

            // Clean up PID file regardless
            std::fs::remove_file(&pid_file).ok();
        }

        // Then update the state
        if let Some(mut state) = self.state_manager.load_state(&instance_hash).await?
            && !state.is_complete()
        {
            state.mark_cancelled(reason);
            self.state_manager.save_state(&state).await?;
            info!(
                "Cancelled execution for directory: {}",
                directory_path.display()
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Clean up completed execution states older than the specified duration
    pub async fn cleanup_old_states(&self, older_than: chrono::Duration) -> Result<usize> {
        let states = self.state_manager.list_active_states().await?;
        let cutoff = chrono::Utc::now() - older_than;
        let mut cleaned_count = 0;

        for state in states {
            if state.is_complete()
                && let Some(finished_at) = state.finished_at
                && finished_at < cutoff
            {
                self.state_manager
                    .remove_state(&state.instance_hash)
                    .await?;
                cleaned_count += 1;
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} old execution states", cleaned_count);
        }

        Ok(cleaned_count)
    }

    /// Execute a single hook and return the result
    pub async fn execute_single_hook(&self, hook: Hook) -> Result<HookResult> {
        // Use the default timeout from config
        let timeout = self.config.default_timeout_seconds;

        // No validation - users approved this config with cuenv allow
        execute_hook_with_timeout(hook, &timeout).await
    }
}

/// Execute hooks sequentially
pub async fn execute_hooks(
    hooks: Vec<Hook>,
    _directory_path: &Path,
    config: &HookExecutionConfig,
    state_manager: &StateManager,
    state: &mut HookExecutionState,
) -> Result<()> {
    let hook_count = hooks.len();
    debug!("execute_hooks called with {} hooks", hook_count);
    if hook_count == 0 {
        debug!("No hooks to execute");
        return Ok(());
    }
    debug!("Starting to iterate over {} hooks", hook_count);
    for (index, hook) in hooks.into_iter().enumerate() {
        debug!(
            "Processing hook {}/{}: command={}",
            index + 1,
            state.total_hooks,
            hook.command
        );
        // Check if execution was cancelled
        debug!("Checking if execution was cancelled");
        if let Ok(Some(current_state)) = state_manager.load_state(&state.instance_hash).await {
            debug!("Loaded state: status = {:?}", current_state.status);
            if current_state.status == ExecutionStatus::Cancelled {
                debug!("Execution was cancelled, stopping");
                break;
            }
        }

        // No validation - users approved this config with cuenv allow

        let timeout_seconds = config.default_timeout_seconds;

        // Mark hook as running
        state.mark_hook_running(index);

        // Execute the hook and wait for it to complete
        let result = execute_hook_with_timeout(hook.clone(), &timeout_seconds).await;

        // Record the result
        match result {
            Ok(hook_result) => {
                // If this is a source hook, evaluate its output to capture environment variables.
                // We do this even if the hook failed (exit code != 0), because tools like devenv
                // might output valid environment exports before crashing or exiting with error.
                // We rely on our robust delimiter-based parsing to extract what we can.
                if hook.source.unwrap_or(false) {
                    if hook_result.stdout.is_empty() {
                        warn!(
                            "Source hook produced empty stdout. Stderr content:\n{}",
                            hook_result.stderr
                        );
                    } else {
                        debug!(
                            "Evaluating source hook output for environment variables (success={})",
                            hook_result.success
                        );
                        match evaluate_shell_environment(
                            &hook_result.stdout,
                            &state.environment_vars,
                        )
                        .await
                        {
                            Ok((env_vars, removed_keys)) => {
                                let count = env_vars.len();
                                debug!(
                                    "Captured {} environment variables from source hook ({} removed)",
                                    count,
                                    removed_keys.len()
                                );
                                // Merge captured environment variables into state
                                for (key, value) in env_vars {
                                    state.environment_vars.insert(key, value);
                                }
                                // Remove variables that were unset by the hook
                                for key in &removed_keys {
                                    state.environment_vars.remove(key);
                                }
                            }
                            Err(e) => {
                                warn!("Failed to evaluate source hook output: {}", e);
                                // Don't fail the hook execution further, just log the error
                            }
                        }
                    }
                }

                state.record_hook_result(index, &hook_result);
                if !hook_result.success && config.fail_fast {
                    warn!(
                        "Hook {} failed and fail_fast is enabled, stopping",
                        index + 1
                    );
                    break;
                }
            }
            Err(e) => {
                let error_msg = format!("Hook execution error: {}", e);
                state.record_hook_result(
                    index,
                    &HookResult::failure(HookFailure {
                        hook: hook.clone(),
                        exit_status: None,
                        stdout: String::new(),
                        stderr: error_msg.clone(),
                        duration_ms: 0,
                        error: error_msg,
                    }),
                );
                if config.fail_fast {
                    warn!("Hook {} failed with error, stopping", index + 1);
                    break;
                }
            }
        }

        // Save state after each hook completes
        state_manager.save_state(state).await?;
    }

    // Mark execution as completed if we got here without errors
    if state.status == ExecutionStatus::Running {
        state.status = ExecutionStatus::Completed;
        state.finished_at = Some(chrono::Utc::now());
        info!(
            "All hooks completed successfully for directory: {}",
            state.directory_path.display()
        );
    }

    // Save final state
    state_manager.save_state(state).await?;

    Ok(())
}

/// Execute a single hook with timeout
async fn execute_hook_with_timeout(hook: Hook, timeout_seconds: &u64) -> Result<HookResult> {
    let start_time = Instant::now();

    debug!(
        "Executing hook: {} {} (source: {})",
        hook.command,
        hook.args.join(" "),
        hook.source.unwrap_or(false)
    );

    // Prepare the command
    let mut cmd = Command::new(&hook.command);
    cmd.args(&hook.args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Set working directory
    if let Some(dir) = &hook.dir {
        cmd.current_dir(dir);
    }

    // Force SHELL to match the evaluator shell for source hooks
    // This ensures tools like devenv output compatible syntax (e.g. avoid fish syntax)
    if hook.source.unwrap_or(false) {
        cmd.env("SHELL", detect_shell().await);
    }

    // Execute with timeout
    let execution_result = timeout(Duration::from_secs(*timeout_seconds), cmd.output()).await;

    let duration_ms = duration_millis(start_time.elapsed());

    match execution_result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                debug!("Hook completed successfully in {}ms", duration_ms);
                Ok(HookResult::success(
                    hook,
                    output.status,
                    stdout,
                    stderr,
                    duration_ms,
                ))
            } else {
                warn!("Hook failed with exit code: {:?}", output.status.code());
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
        Ok(Err(io_error)) => {
            error!("Failed to execute hook: {}", io_error);
            Ok(HookResult::failure(HookFailure {
                hook,
                exit_status: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms,
                error: format!("Failed to execute command: {}", io_error),
            }))
        }
        Err(_timeout_error) => {
            warn!("Hook timed out after {} seconds", timeout_seconds);
            Ok(HookResult::timeout(
                hook,
                String::new(),
                String::new(),
                *timeout_seconds,
            ))
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
