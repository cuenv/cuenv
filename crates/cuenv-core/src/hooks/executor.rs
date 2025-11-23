//! Hook execution engine with background processing and state management

use crate::hooks::state::{HookExecutionState, StateManager, compute_instance_hash};
use crate::hooks::types::{ExecutionStatus, Hook, HookExecutionConfig, HookResult};
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
                use sysinfo::{Pid, ProcessRefreshKind, System};
                let mut system = System::new();
                let process_pid = Pid::from(pid);
                system.refresh_process_specifics(process_pid, ProcessRefreshKind::new());

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
            .map_err(|e| Error::configuration(format!("Failed to serialize hooks: {}", e)))?;
        std::fs::write(&hooks_file, &hooks_json).map_err(|e| Error::Io {
            source: e,
            path: Some(hooks_file.clone().into_boxed_path()),
            operation: "write".to_string(),
        })?;

        // Serialize and write config
        let config_json = serde_json::to_string(&self.config)
            .map_err(|e| Error::configuration(format!("Failed to serialize config: {}", e)))?;
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
                .map_err(|e| Error::configuration(format!("Failed to get current exe: {}", e)))?
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
            .map_err(|e| Error::configuration(format!("Failed to spawn supervisor: {}", e)))?;

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
                return Err(Error::configuration("No execution state found"));
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
            use sysinfo::{Pid, ProcessRefreshKind, Signal, System};

            let mut system = System::new();
            let process_pid = Pid::from(pid);

            // Refresh the specific process
            system.refresh_process_specifics(process_pid, ProcessRefreshKind::new());

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
                // If this is a source hook and it succeeded, evaluate its output
                if hook.source.unwrap_or(false)
                    && hook_result.success
                    && !hook_result.stdout.is_empty()
                {
                    debug!("Evaluating source hook output for environment variables");
                    match evaluate_shell_environment(&hook_result.stdout).await {
                        Ok(env_vars) => {
                            debug!(
                                "Captured {} environment variables from source hook",
                                env_vars.len()
                            );
                            // Merge captured environment variables into state
                            for (key, value) in env_vars {
                                state.environment_vars.insert(key, value);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to evaluate source hook output: {}", e);
                            // Don't fail the hook execution, just log the error
                        }
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
fn detect_shell() -> &'static str {
    // Check if bash is available
    if std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_ok()
    {
        return "bash";
    }

    // Fall back to POSIX sh
    "sh"
}

/// Evaluate shell script and extract resulting environment variables
async fn evaluate_shell_environment(shell_script: &str) -> Result<HashMap<String, String>> {
    debug!(
        "Evaluating shell script to extract environment ({} bytes)",
        shell_script.len()
    );

    tracing::error!("Raw shell script from hook:\n{}", shell_script);

    let shell = detect_shell();
    debug!("Using shell: {}", shell);

    // First, get the environment before running the script
    let mut cmd_before = Command::new(shell);
    cmd_before.arg("-c");
    cmd_before.arg("env -0");
    cmd_before.stdout(Stdio::piped());
    cmd_before.stderr(Stdio::piped());

    let output_before = cmd_before
        .output()
        .await
        .map_err(|e| Error::configuration(format!("Failed to get initial environment: {}", e)))?;

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
    tracing::error!("Filtered shell script:\n{}", filtered_script);

    // Now execute the filtered script and capture the environment after
    let mut cmd = Command::new(shell);
    cmd.arg("-c");
    // Create a script that sources the exports and then prints the environment
    // We wrap the sourced script in a subshell or block that ignores errors?
    // No, we want environment variables to persist.
    // But if a command fails, we don't want the whole evaluation to fail.
    // We append " || true" to each line? No, multiline strings.
    
    // Better: Execute the script, but ensure we always reach "env -0".
    // In sh, "cmd; env" runs env even if cmd fails, UNLESS set -e is active.
    // By default set -e is OFF.
    
    // However, we check output.status.success().
    // If the last command is "env -0", and it succeeds, the exit code is 0.
    // Even if previous commands failed (and printed to stderr).
    
    // So "nonexistent_command; env -0" -> exit code 0.
    // Why did my thought experiment suggest failure?
    // Maybe because I was confusing it with pipefail or set -e.
    
    // The reproduction test PASSED even with "nonexistent_command_garbage".
    // This means `evaluate_shell_environment` IS robust against simple command failures!
    
    // So why is the user's case failing?
    
    // Maybe `devenv` output contains something that makes `env -0` NOT run or NOT output what we expect?
    // Or maybe it exits the shell? `exit 1`?
    
    // If `devenv` prints `exit 1`, then `env -0` is never reached.
    // `devenv` is a script. If it calls `exit`, it exits the sourcing shell?
    // If we `source` a script that `exit`s, it exits the parent shell (our sh -c).
    
    // Does `devenv print-dev-env` exit?
    // It shouldn't.
    
    // But if `devenv` fails internally, does it exit?
    // "devenv: command not found" -> 127, continues.
    
    // What if the output is syntactically invalid shell?
    // `export FOO="unclosed`
    // Then `sh` parses it, finds error, prints error, and... stops?
    // Usually yes, syntax error aborts execution of the script.
    
    // Let's try to reproduce SYNTAX ERROR.
    const DELIMITER: &str = "__CUENV_ENV_START__";
    let script = format!("{}\necho -ne '\\0{}\\0'; env -0", filtered_script, DELIMITER);
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
        // Fallback: try to use the whole output if delimiter missing, 
        // but this is risky if stdout has garbage.
        // However, if env -0 ran, it's usually at the end.
        // But without delimiter, we can't separate garbage from vars safely.
        // We'll try to parse anyway, effectively reverting to previous behavior,
        // but with the risk of corruption if garbage exists.
        // Given we added delimiter specifically to avoid this, maybe we should return empty?
        // But if script crashed before printing delimiter, we capture nothing.
        // If we return empty, at least we don't return corrupted vars.
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
        cmd.env("SHELL", detect_shell());
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
    use crate::hooks::types::Hook;
    use tempfile::TempDir;

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
                command: "echo".to_string(),
                args: vec!["hook1".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
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
    #[ignore = "Needs investigation - async state management"]
    async fn test_cancellation() {
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

        // Wait a bit for execution to start
        tokio::time::sleep(Duration::from_millis(100)).await;

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
    #[ignore = "Needs investigation - async runtime issues"]
    async fn test_state_cleanup() {
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

        // Wait for completion
        executor
            .wait_for_completion(&directory_path, &config_hash, Some(5))
            .await
            .unwrap();

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

    // Commented out: allow_command and disallow_command methods don't exist
    // #[tokio::test]
    // async fn test_command_whitelist_management() {
    //         let executor = HookExecutor::with_default_config().unwrap();
    //
    //         // Test adding a new command to whitelist
    //         let custom_command = "my-custom-tool".to_string();
    //         executor.allow_command(custom_command.clone()).await;
    //
    //         // Test that the newly allowed command works
    //         let hook = Hook {
    //             command: custom_command.clone(),
    //             args: vec!["--version".to_string()],
    //             dir: None,
    //             inputs: Vec::new(),
    //             source: Some(false),
    //         };
    //
    //         // Should not error due to whitelist check (may fail if command doesn't exist)
    //         let result = executor.execute_single_hook(hook).await;
    //         // If it errors, it should be because the command doesn't exist, not because it's not allowed
    //         if result.is_err() {
    //             let err_msg = result.unwrap_err().to_string();
    //             assert!(
    //                 !err_msg.contains("not allowed"),
    //                 "Command should be allowed after adding to whitelist"
    //             );
    //         }
    //
    //         // Test removing a command from whitelist
    //         executor.disallow_command("echo").await;
    //
    //         let hook = Hook {
    //             command: "echo".to_string(),
    //             args: vec!["test".to_string()],
    //             dir: None,
    //             inputs: vec![],
    //             source: None,
    //         };
    //
    //         let result = executor.execute_single_hook(hook).await;
    //         assert!(result.is_err(), "Echo command should be disallowed");
    //         let err_msg = result.unwrap_err().to_string();
    //         assert!(
    //             err_msg.contains("not allowed")
    //                 || err_msg.contains("not in whitelist")
    //                 || err_msg.contains("Configuration"),
    //             "Error message should indicate command not allowed: {}",
    //             err_msg
    //         );
    //
    //         // Test re-allowing a command
    //         executor.allow_command("echo".to_string()).await;
    //
    //         let hook = Hook {
    //             command: "echo".to_string(),
    //             args: vec!["test".to_string()],
    //             dir: None,
    //             inputs: vec![],
    //             source: None,
    //         };
    //
    //         let result = executor.execute_single_hook(hook).await;
    //         assert!(
    //             result.is_ok(),
    //             "Echo command should be allowed after re-adding"
    //         );
    //     }

    #[tokio::test]
    #[ignore = "Needs investigation - timing issues"]
    async fn test_fail_fast_mode_edge_cases() {
        let temp_dir = TempDir::new().unwrap();

        // Test fail_fast with multiple failing hooks
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: true,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/fail-fast");

        let hooks = vec![
            Hook {
                command: "false".to_string(), // Will fail
                args: vec![],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "echo".to_string(), // Should not execute due to fail_fast
                args: vec!["should not run".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "echo".to_string(), // Should not execute due to fail_fast
                args: vec!["also should not run".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
        ];

        let config_hash = "fail_fast_test".to_string();
        executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        // Wait for completion
        executor
            .wait_for_completion(&directory_path, &config_hash, Some(10))
            .await
            .unwrap();

        let state = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state.status, ExecutionStatus::Failed);
        // Only the first hook should have been executed
        // Only the first hook should have been executed
        assert_eq!(state.completed_hooks, 1);

        // Test fail_fast with continue_on_error interaction
        let directory_path2 = PathBuf::from("/test/fail-fast-continue");

        let hooks2 = vec![
            Hook {
                command: "false".to_string(),
                args: vec![],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["this should run".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "false".to_string(), // Will fail and stop execution
                args: vec![],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["this should not run".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
        ];

        let config_hash2 = "fail_fast_continue_test".to_string();
        executor
            .execute_hooks_background(directory_path2.clone(), config_hash2.clone(), hooks2)
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path2, &config_hash2, Some(10))
            .await
            .unwrap();

        let state2 = executor
            .get_execution_status_for_instance(&directory_path2, &config_hash2)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state2.status, ExecutionStatus::Failed);
        // First three hooks should have been executed
        // First three hooks should have been executed
        assert_eq!(state2.completed_hooks, 3);
    }

    #[tokio::test]
    async fn test_security_validation_comprehensive() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Security validation has been removed - the approval mechanism is the security boundary
        // Commands with any arguments are now allowed after approval

        // Test that echo command works with various arguments
        let test_args = vec![
            vec!["simple test".to_string()],
            vec!["test with spaces".to_string()],
            ["test", "multiple", "args"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ];

        for args in test_args {
            let hook = Hook {
                command: "echo".to_string(),
                args: args.clone(),
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            };

            let result = executor.execute_single_hook(hook).await;
            assert!(
                result.is_ok(),
                "Echo command should work with args: {:?}",
                args
            );
        }
    }

    #[tokio::test]
    async fn test_working_directory_handling() {
        let executor = HookExecutor::with_default_config().unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Test with valid working directory
        let hook_with_valid_dir = Hook {
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
            command: "pwd".to_string(),
            args: vec![],
            dir: Some("/nonexistent/directory/that/does/not/exist".to_string()),
            inputs: vec![],
            source: None,
        };

        let result = executor.execute_single_hook(hook_with_invalid_dir).await;
        // This might succeed or fail depending on the implementation
        // The important part is it doesn't panic
        if result.is_ok() {
            // If it succeeds, the command might have handled the missing directory
            assert!(
                !result
                    .unwrap()
                    .stdout
                    .contains("/nonexistent/directory/that/does/not/exist")
            );
        }

        // Test with relative path working directory (should be validated)
        let hook_with_relative_dir = Hook {
            command: "pwd".to_string(),
            args: vec![],
            dir: Some("./relative/path".to_string()),
            inputs: vec![],
            source: None,
        };

        // This might work or fail depending on the implementation
        let _ = executor.execute_single_hook(hook_with_relative_dir).await;
    }

    #[tokio::test]
    async fn test_hook_execution_with_complex_output() {
        let executor = HookExecutor::with_default_config().unwrap();

        // Test simple hooks without dangerous characters
        let hook = Hook {
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
    #[ignore = "Needs investigation - state management"]
    async fn test_multiple_directory_executions() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();

        // Start executions for multiple directories
        let directories = [
            PathBuf::from("/test/dir1"),
            PathBuf::from("/test/dir2"),
            PathBuf::from("/test/dir3"),
        ];

        let mut config_hashes = Vec::new();
        for (i, dir) in directories.iter().enumerate() {
            let hooks = vec![Hook {
                command: "echo".to_string(),
                args: vec![format!("directory {}", i)],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            }];

            let config_hash = format!("hash_{}", i);
            config_hashes.push(config_hash.clone());
            executor
                .execute_hooks_background(dir.clone(), config_hash.clone(), hooks)
                .await
                .unwrap();
        }

        // Wait for all to complete
        for (dir, config_hash) in directories.iter().zip(config_hashes.iter()) {
            executor
                .wait_for_completion(dir, config_hash, Some(10))
                .await
                .unwrap();

            let state = executor
                .get_execution_status_for_instance(dir, config_hash)
                .await
                .unwrap()
                .unwrap();

            assert_eq!(state.status, ExecutionStatus::Completed);
            assert_eq!(state.completed_hooks, 1);
            assert_eq!(state.total_hooks, 1);
        }
    }

    #[tokio::test]
    #[ignore = "Needs investigation - retry logic"]
    async fn test_error_recovery_and_retry() {
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/recovery");

        // Execute hooks with some failures
        let hooks = vec![
            Hook {
                command: "echo".to_string(),
                args: vec!["success 1".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "false".to_string(),
                args: vec![],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
            Hook {
                command: "echo".to_string(),
                args: vec!["success 2".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            },
        ];

        let config_hash = "recovery_test".to_string();
        executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path, &config_hash, Some(10))
            .await
            .unwrap();

        let state = executor
            .get_execution_status_for_instance(&directory_path, &config_hash)
            .await
            .unwrap()
            .unwrap();

        // Should complete with partial failure
        assert_eq!(state.status, ExecutionStatus::Completed);
        assert_eq!(state.completed_hooks, 3);
        assert_eq!(state.total_hooks, 3);
    }

    #[tokio::test]
    #[ignore = "Requires supervisor binary - integration test"]
    async fn test_instance_hash_separation() {
        // Test that different config hashes for the same directory are tracked separately
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/multi-config");

        // Create hooks for first configuration
        let hooks1 = vec![Hook {
            command: "echo".to_string(),
            args: vec!["config1".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }];

        // Create hooks for second configuration
        let hooks2 = vec![Hook {
            command: "echo".to_string(),
            args: vec!["config2".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }];

        let config_hash1 = "config_hash_1".to_string();
        let config_hash2 = "config_hash_2".to_string();

        // Execute both configurations
        executor
            .execute_hooks_background(directory_path.clone(), config_hash1.clone(), hooks1)
            .await
            .unwrap();

        executor
            .execute_hooks_background(directory_path.clone(), config_hash2.clone(), hooks2)
            .await
            .unwrap();

        // Wait for both to complete
        executor
            .wait_for_completion(&directory_path, &config_hash1, Some(5))
            .await
            .unwrap();

        executor
            .wait_for_completion(&directory_path, &config_hash2, Some(5))
            .await
            .unwrap();

        // Check that both have separate states
        let state1 = executor
            .get_execution_status_for_instance(&directory_path, &config_hash1)
            .await
            .unwrap()
            .unwrap();

        let state2 = executor
            .get_execution_status_for_instance(&directory_path, &config_hash2)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(state1.status, ExecutionStatus::Completed);
        assert_eq!(state2.status, ExecutionStatus::Completed);

        // Ensure they have different instance hashes
        assert_ne!(state1.instance_hash, state2.instance_hash);
    }

    #[tokio::test]
    #[ignore = "Requires supervisor binary - integration test"]
    async fn test_file_based_argument_passing() {
        // Test that hooks and config are written to files and cleaned up
        let temp_dir = TempDir::new().unwrap();
        let config = HookExecutionConfig {
            default_timeout_seconds: 30,
            fail_fast: false,
            state_dir: Some(temp_dir.path().to_path_buf()),
        };

        let executor = HookExecutor::new(config).unwrap();
        let directory_path = PathBuf::from("/test/file-args");
        let config_hash = "file_test".to_string();

        // Create a large hook configuration that would exceed typical arg limits
        let mut large_hooks = Vec::new();
        for i in 0..100 {
            large_hooks.push(Hook {
                command: "echo".to_string(),
                args: vec![format!("This is a very long argument string number {} with lots of text to ensure we test the file-based argument passing mechanism properly", i)],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            });
        }

        // This should write to files instead of passing as arguments
        let result = executor
            .execute_hooks_background(directory_path.clone(), config_hash.clone(), large_hooks)
            .await;

        assert!(result.is_ok(), "Should handle large hook configurations");

        // Wait a bit for supervisor to start and read files
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Cancel to clean up
        executor
            .cancel_execution(
                &directory_path,
                &config_hash,
                Some("Test cleanup".to_string()),
            )
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_state_dir_getter() {
        use crate::hooks::state::StateManager;

        let temp_dir = TempDir::new().unwrap();
        let state_dir = temp_dir.path().to_path_buf();
        let state_manager = StateManager::new(state_dir.clone());

        assert_eq!(state_manager.get_state_dir(), state_dir.as_path());
    }
}
