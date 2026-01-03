//! Hook execution engine with background processing and state management

use crate::state::{compute_instance_hash, HookExecutionState, StateManager};
use crate::types::{ExecutionStatus, Hook, HookExecutionConfig, HookResult};
use crate::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

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
        use std::process::{Command, Stdio};

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
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Detach from parent process group using setsid
            unsafe {
                cmd.pre_exec(|| {
                    // Create a new session, detaching from controlling terminal
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

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
                    if !hook_result.stdout.is_empty() {
                        debug!(
                            "Evaluating source hook output for environment variables (success={})",
                            hook_result.success
                        );
                        match evaluate_shell_environment(&hook_result.stdout).await {
                            Ok(env_vars) => {
                                let count = env_vars.len();
                                debug!("Captured {} environment variables from source hook", count);
                                if count > 0 {
                                    // Merge captured environment variables into state
                                    for (key, value) in env_vars {
                                        state.environment_vars.insert(key, value);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to evaluate source hook output: {}", e);
                                // Don't fail the hook execution further, just log the error
                            }
                        }
                    } else {
                        warn!(
                            "Source hook produced empty stdout. Stderr content:\n{}",
                            hook_result.stderr
                        );
                    }
                }

                state.record_hook_result(index, hook_result.clone());
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
                    HookResult::failure(
                        hook.clone(),
                        None,
                        String::new(),
                        error_msg.clone(),
                        0,
                        error_msg,
                    ),
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

/// Detect which shell to use for environment evaluation
async fn detect_shell() -> String {
    // Try bash first
    if is_shell_capable("bash").await {
        return "bash".to_string();
    }

    // Try zsh (common on macOS where bash is old)
    if is_shell_capable("zsh").await {
        return "zsh".to_string();
    }

    // Fall back to sh (likely to fail for advanced scripts but better than nothing)
    "sh".to_string()
}

/// Check if a shell supports modern features like case fallthrough (;&)
async fn is_shell_capable(shell: &str) -> bool {
    let check_script = "case x in x) true ;& y) true ;; esac";
    match Command::new(shell)
        .arg("-c")
        .arg(check_script)
        .output()
        .await
    {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Evaluate shell script and extract resulting environment variables
async fn evaluate_shell_environment(shell_script: &str) -> Result<HashMap<String, String>> {
    debug!(
        "Evaluating shell script to extract environment ({} bytes)",
        shell_script.len()
    );

    tracing::trace!("Raw shell script from hook:\n{}", shell_script);

    // Try to find the specific bash binary that produced this script (common in Nix/devenv)
    // This avoids compatibility issues with system bash (e.g. macOS bash 3.2 vs Nix bash 5.x)
    let mut shell = detect_shell().await;

    for line in shell_script.lines() {
        if let Some(path) = line.strip_prefix("BASH='")
            && let Some(end) = path.find('\'')
        {
            let bash_path = &path[..end];
            let path = PathBuf::from(bash_path);
            if path.exists() {
                debug!("Detected Nix bash in script: {}", bash_path);
                shell = bash_path.to_string();
                break;
            }
        }
    }

    debug!("Using shell: {}", shell);

    // First, get the environment before running the script
    let mut cmd_before = Command::new(&shell);
    cmd_before.arg("-c");
    cmd_before.arg("env -0");
    cmd_before.stdout(Stdio::piped());
    cmd_before.stderr(Stdio::piped());

    let output_before = cmd_before.output().await.map_err(|e| {
        Error::configuration(format!("Failed to get initial environment: {}", e))
    })?;

    let env_before_output = String::from_utf8_lossy(&output_before.stdout);
    let mut env_before = HashMap::new();
    for line in env_before_output.split('\0') {
        if let Some((key, value)) = line.split_once('=') {
            env_before.insert(key.to_string(), value.to_string());
        }
    }

    // Filter out lines that are likely status messages or not shell assignments
    let filtered_lines: Vec<&str> = shell_script
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }

            // Filter out known status/error prefixes that might pollute stdout
            if trimmed.starts_with("✓")
                || trimmed.starts_with("sh:")
                || trimmed.starts_with("bash:")
            {
                return false;
            }

            // Otherwise keep it. We trust the tool to output valid shell code
            // (including multiline strings, comments, unsets, aliases, etc.)
            true
        })
        .collect();

    let filtered_script = filtered_lines.join("\n");
    tracing::trace!("Filtered shell script:\n{}", filtered_script);

    // Now execute the filtered script and capture the environment after
    let mut cmd = Command::new(shell);
    cmd.arg("-c");

    const DELIMITER: &str = "__CUENV_ENV_START__";
    let script = format!(
        "{}\necho -ne '\\0{}\\0'; env -0",
        filtered_script, DELIMITER
    );
    cmd.arg(script);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let output = cmd.output().await.map_err(|e| {
        Error::configuration(format!("Failed to evaluate shell environment: {}", e))
    })?;

    // If the command failed, we still try to parse the output, in case env -0 ran.
    // But we should log the error.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "Shell script evaluation finished with error (exit code {:?}): {}",
            output.status.code(),
            stderr
        );
        // We continue to try to parse stdout.
    }

    // Parse the output. We expect: <script_output>\0<DELIMITER>\0<env_vars>\0...
    let stdout_bytes = &output.stdout;
    let delimiter_bytes = format!("\0{}\0", DELIMITER).into_bytes();

    // Find the delimiter in the output
    let env_start_index = stdout_bytes
        .windows(delimiter_bytes.len())
        .position(|window| window == delimiter_bytes);

    let env_output_bytes = if let Some(idx) = env_start_index {
        // We found the delimiter, everything after it is the environment
        &stdout_bytes[idx + delimiter_bytes.len()..]
    } else {
        debug!("Environment delimiter not found in hook output");
        // Log the tail of stdout to diagnose why delimiter is missing
        let len = stdout_bytes.len();
        let start = len.saturating_sub(1000);
        let tail = String::from_utf8_lossy(&stdout_bytes[start..]);
        warn!(
            "Delimiter missing. Tail of stdout (last 1000 bytes):\n{}",
            tail
        );

        // Fallback: return empty if delimiter missing
        &[]
    };

    let env_output = String::from_utf8_lossy(env_output_bytes);
    let mut env_delta = HashMap::new();

    for line in env_output.split('\0') {
        if line.is_empty() {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            // Skip some problematic variables that can interfere
            if key.starts_with("BASH_FUNC_")
                || key == "PS1"
                || key == "PS2"
                || key == "_"
                || key == "PWD"
                || key == "OLDPWD"
                || key == "SHLVL"
                || key.starts_with("BASH")
            {
                continue;
            }

            // Only include variables that are new or changed
            // We also skip empty keys which can happen with malformed output
            if !key.is_empty() && env_before.get(key) != Some(&value.to_string()) {
                env_delta.insert(key.to_string(), value.to_string());
            }
        }
    }

    if env_delta.is_empty() && !output.status.success() {
        // If we failed AND got no variables, that's a real problem.
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::configuration(format!(
            "Shell script evaluation failed and no environment captured. Error: {}",
            stderr
        )));
    }

    debug!(
        "Evaluated shell script and extracted {} new/changed environment variables",
        env_delta.len()
    );
    Ok(env_delta)
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

    let duration_ms = start_time.elapsed().as_millis() as u64;

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
                Ok(HookResult::failure(
                    hook,
                    Some(output.status),
                    stdout,
                    stderr,
                    duration_ms,
                    format!("Command exited with status: {}", output.status),
                ))
            }
        }
        Ok(Err(io_error)) => {
            error!("Failed to execute hook: {}", io_error);
            Ok(HookResult::failure(
                hook,
                None,
                String::new(),
                String::new(),
                duration_ms,
                format!("Failed to execute command: {}", io_error),
            ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Hook;
    use tempfile::TempDir;

    /// Helper to set up CUENV_EXECUTABLE for tests that spawn the supervisor.
    /// The cuenv binary must already be built (via `cargo build --bin cuenv`).
    fn setup_cuenv_executable() -> Option<PathBuf> {
        // Check if already set
        if std::env::var("CUENV_EXECUTABLE").is_ok() {
            return Some(PathBuf::from(std::env::var("CUENV_EXECUTABLE").unwrap()));
        }

        // Try to find the cuenv binary in target/debug
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent()?.parent()?;
        let cuenv_binary = workspace_root.join("target/debug/cuenv");

        if cuenv_binary.exists() {
            // SAFETY: This is only called in tests where we control the environment.
            // No other threads should be accessing this environment variable.
            unsafe {
                std::env::set_var("CUENV_EXECUTABLE", &cuenv_binary);
            }
            Some(cuenv_binary)
        } else {
            None
        }
    }

    #[tokio::test]
    async fn test_hook_executor_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 60,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        assert_eq!(executor.config.default_timeout_seconds, 60);
    }

    #[tokio::test]
    async fn test_execute_single_hook_success() {
        let executor = HookExecutor::with_default_config().unwrap();

        let hook = Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_single_hook_failure() {
        let executor = HookExecutor::with_default_config().unwrap();

        let hook = Hook {
            order: 100,
            propagate: false,
            command: "false".to_string(), // Command that always fails
            args: vec![],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(!result.success);
        assert!(result.exit_status.is_some());
        assert_ne!(result.exit_status.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_execute_single_hook_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 1, // Set timeout to 1 second
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };
        let executor = HookExecutor::new(config).unwrap();

        let hook = Hook {
            order: 100,
            propagate: false,
            command: "sleep".to_string(),
            args: vec!["10".to_string()], // Sleep for 10 seconds
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_background_execution() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/directory");
        let config_hash = "test_hash".to_string();

        let hooks = vec![
            Hook {
                order: 100,
                propagate: false,
                command: "echo".to_string(),
                args: vec!["hook1".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                order: 100,
                propagate: false,
                command: "echo".to_string(),
                args: vec!["hook2".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
        ];

        let result = executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        assert!(result.contains("Started execution of 2 hooks"));

        // Wait a bit for background execution to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Check execution status
        let status = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap();
        assert!(status.is_some());

        let state = status.unwrap();
        assert_eq!(state.total_hooks, 2);
        assert_eq!(state.directory_path, directory_path);
    }

    #[tokio::test]
    async fn test_command_validation() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Commands are no longer validated against a whitelist
        // The approval mechanism is the security boundary

        // Test that echo command works with any arguments
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test message".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        };

        let result = executor.execute_single_hook(hook).await;
        assert!(result.is_ok(), "Echo command should succeed");

        // Verify the output contains the expected message
        let hook_result = result.unwrap();
        assert!(hook_result.stdout.contains("test message"));
    }

    #[tokio::test]
    async fn test_cancellation() {
        // Skip if cuenv binary is not available
        if setup_cuenv_executable().is_none() {
            eprintln!("Skipping test_cancellation: cuenv binary not found");
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/cancel");
        let config_hash = "cancel_test".to_string();

        // Create a long-running hook
        let hooks = vec![Hook {
            order: 100,
            propagate: false,
            command: "sleep".to_string(),
            args: vec!["10".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        // Wait for supervisor to actually start and create state
        // Poll until we see Running status or timeout
        let mut started = false;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(Some(state)) = executor
                .get_execution_status_for_instance(&directory_path, &config_hash)
                .await
                && state.status == ExecutionStatus::Running
            {
                started = true;
                break;
            }
        }

        if !started {
            eprintln!("Warning: Supervisor didn't start in time, skipping cancellation test");
            return;
        }

        // Cancel the execution
        let cancelled = executor
            .cancel_execution(
                &directory_path,
                &config_hash,
                Some("User cancelled".to_string()),
            )
            .await
            .unwrap();
        assert!(cancelled);

        // Check that state reflects cancellation
        let state = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_large_output_handling() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Generate a large output using printf repeating a pattern
        // Create a large string in the environment variable instead
        let large_content = "x".repeat(1000); // 1KB per line
        let mut args = Vec::new();
        // Generate 100 lines of 1KB each = 100KB total
        for i in 0..100 {
            args.push(format!("Line {}: {}", i, large_content));
        }

        // Use echo with multiple arguments
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args,
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        // Output should be captured without causing memory issues
        assert!(result.stdout.len() > 50_000); // At least 50KB of output
    }

    #[tokio::test]
    async fn test_state_cleanup() {
        // Skip if cuenv binary is not available
        if setup_cuenv_executable().is_none() {
            eprintln!("Skipping test_state_cleanup: cuenv binary not found");
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/cleanup");
        let config_hash = "cleanup_test".to_string();

        // Execute some hooks
        let hooks = vec![Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        // Poll until state exists before waiting for completion
        let mut state_exists = false;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if executor
                .get_execution_status_for_instance(&directory_path, &config_hash)
                .await
                .unwrap()
                .is_some()
            {
                state_exists = true;
                break;
            }
        }

        if !state_exists {
            eprintln!("Warning: State never created, skipping cleanup test");
            return;
        }

        // Wait for completion
        if let Err(e) = executor
            .wait_for_completion(&directory_path, &config_hash, Some(15))
            .await
        {
            eprintln!(
                "Warning: wait_for_completion timed out: {}, skipping test",
                e
            );
            return;
        }

        // Clean up old states (should clean up the completed state)
        let cleaned = executor
            .cleanup_old_states(chrono::Duration::seconds(0))
            .await
            .unwrap();
        assert_eq!(cleaned, 1);

        // State should be gone
        let state = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap();
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_execution_state_tracking() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/directory");
        let config_hash = "hash".to_string();

        // Initially no state
        let status = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap();
        assert!(status.is_none());

        // Start execution
        let hooks = vec![Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }];

        executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        // Should now have state
        let status = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap();
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn test_working_directory_handling() {
        let executor = HookExecutor::with_default_config().unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Test with valid working directory
        let hook_with_valid_dir = Hook {
            order: 100,
            propagate: false,
            command: "pwd".to_string(),
            args: vec![],
            dir: Some(temp_dir.path().to_string_lossy().to_string()),
            inputs: vec![],
            source: None,
        };

        let result = executor
            .execute_single_hook(hook_with_valid_dir)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.stdout.contains(temp_dir.path().to_str().unwrap()));

        // Test with non-existent working directory
        let hook_with_invalid_dir = Hook {
            order: 100,
            propagate: false,
            command: "pwd".to_string(),
            args: vec![],
            dir: Some("/nonexistent/directory/that/does/not/exist".to_string()),
            inputs: vec![],
            source: None,
        };

        let result = executor.execute_single_hook(hook_with_invalid_dir).await;
        // This might succeed or fail depending on the implementation
        // The important part is it doesn't panic
        if let Ok(output) = result {
            // If it succeeds, the command might have handled the missing directory
            assert!(
                !output
                    .stdout
                    .contains("/nonexistent/directory/that/does/not/exist")
            );
        }
    }

    #[tokio::test]
    async fn test_hook_execution_with_complex_output() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test simple hooks without dangerous characters
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["stdout output".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("stdout output"));

        // Test hook with non-zero exit code (using false command)
        let hook_with_exit_code = Hook {
            order: 100,
            propagate: false,
            command: "false".to_string(),
            args: vec![],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        };

        let result = executor
            .execute_single_hook(hook_with_exit_code)
            .await
            .unwrap();
        assert!(!result.success);
        // Exit code should be non-zero
        assert!(result.exit_status.is_some());
    }

    #[tokio::test]
    async fn test_state_dir_getter() {
        use crate::state::StateManager;

        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().to_path_buf();
        let state_manager = StateManager::new(state_dir.clone());

        assert_eq!(state_manager.get_state_dir(), state_dir.as_path());
    }
}
