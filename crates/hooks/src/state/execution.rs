use crate::types::{ExecutionStatus, Hook, HookResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{error, info};

/// Represents the state of hook execution for a specific directory
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookExecutionState {
    /// Hash combining directory path and config (instance identifier)
    pub instance_hash: String,
    /// Shell or CI invocation whose environment produced this run.
    #[serde(default)]
    pub session_id: uuid::Uuid,
    /// Identifies one execution of this instance; nil denotes a legacy state.
    #[serde(default)]
    pub run_id: uuid::Uuid,
    /// Path to the directory being processed
    pub directory_path: PathBuf,
    /// Input-aware identity of the hook execution configuration.
    pub config_hash: String,
    /// Current status of execution
    pub status: ExecutionStatus,
    /// Total number of hooks to execute
    pub total_hooks: usize,
    /// Number of hooks completed so far
    pub completed_hooks: usize,
    /// Index of currently executing hook (if any)
    pub current_hook_index: Option<usize>,
    /// The list of hooks being executed (for display purposes)
    #[serde(default)]
    pub hooks: Vec<Hook>,
    /// Results of completed hooks
    pub hook_results: HashMap<usize, HookResult>,
    /// Timestamp when execution started
    pub started_at: DateTime<Utc>,
    /// Timestamp when execution finished (if completed)
    pub finished_at: Option<DateTime<Utc>>,
    /// Timestamp when the current hook started (if running)
    pub current_hook_started_at: Option<DateTime<Utc>>,
    /// Timestamp until which completed state should be displayed
    pub completed_display_until: Option<DateTime<Utc>>,
    /// Error message if execution failed
    pub error_message: Option<String>,
    /// Environment variables captured from source hooks
    pub environment_vars: HashMap<String, String>,
}

impl std::fmt::Debug for HookExecutionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookExecutionState")
            .field("run_id", &self.run_id)
            .field("status", &self.status)
            .field("total_hooks", &self.total_hooks)
            .field("completed_hooks", &self.completed_hooks)
            .field("current_hook_index", &self.current_hook_index)
            .finish_non_exhaustive()
    }
}

/// Identity of a hook execution state.
pub struct HookExecutionIdentity {
    /// Directory being processed.
    pub directory_path: PathBuf,
    /// Hash identifying the directory and configuration.
    pub instance_hash: String,
    /// Shell or CI invocation whose environment will be used by the run.
    pub session_id: uuid::Uuid,
    /// Input-aware identity of the hook execution configuration.
    pub config_hash: String,
}

impl HookExecutionState {
    /// Create a new execution state
    #[must_use]
    pub fn new(identity: HookExecutionIdentity, hooks: Vec<Hook>) -> Self {
        let HookExecutionIdentity {
            directory_path,
            instance_hash,
            session_id,
            config_hash,
        } = identity;
        let total_hooks = hooks.len();
        Self {
            instance_hash,
            session_id,
            run_id: uuid::Uuid::new_v4(),
            directory_path,
            config_hash,
            status: ExecutionStatus::Running,
            total_hooks,
            completed_hooks: 0,
            current_hook_index: None,
            hooks,
            hook_results: HashMap::new(),
            started_at: Utc::now(),
            finished_at: None,
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: None,
            environment_vars: HashMap::new(),
        }
    }

    /// Mark a hook as currently executing
    pub fn mark_hook_running(&mut self, hook_index: usize) {
        self.current_hook_index = Some(hook_index);
        self.current_hook_started_at = Some(Utc::now());
        info!(
            "Started executing hook {} of {}",
            hook_index + 1,
            self.total_hooks
        );
    }

    /// Record the result of a hook execution
    pub fn record_hook_result(&mut self, hook_index: usize, result: &HookResult) {
        self.hook_results.insert(hook_index, result.clone());
        self.completed_hooks += 1;
        self.current_hook_index = None;
        self.current_hook_started_at = None;

        if result.success {
            info!(
                "Hook {} of {} completed successfully",
                hook_index + 1,
                self.total_hooks
            );
        } else {
            error!("Hook {} of {} failed", hook_index + 1, self.total_hooks);
            self.status = ExecutionStatus::Failed;
            self.error_message.clone_from(&result.error);
            self.finished_at = Some(Utc::now());
            self.completed_display_until = Some(Utc::now() + chrono::Duration::seconds(2));
            return;
        }

        if self.completed_hooks == self.total_hooks {
            self.status = ExecutionStatus::Completed;
            let now = Utc::now();
            self.finished_at = Some(now);
            self.completed_display_until = Some(now + chrono::Duration::seconds(2));
            info!("All {} hooks completed successfully", self.total_hooks);
        }
    }

    /// Mark execution as cancelled
    pub fn mark_cancelled(&mut self, reason: Option<String>) {
        self.status = ExecutionStatus::Cancelled;
        self.finished_at = Some(Utc::now());
        self.error_message = reason;
        self.current_hook_index = None;
    }

    /// Check if execution is complete (success, failure, or cancelled)
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        )
    }

    /// Get a human-readable progress display
    #[must_use]
    pub fn progress_display(&self) -> String {
        match &self.status {
            ExecutionStatus::Running => {
                if let Some(current) = self.current_hook_index {
                    format!(
                        "Executing hook {} of {} ({})",
                        current + 1,
                        self.total_hooks,
                        self.status
                    )
                } else {
                    format!(
                        "{} of {} hooks completed",
                        self.completed_hooks, self.total_hooks
                    )
                }
            }
            ExecutionStatus::Cancelling => "Cancelling hook execution".to_string(),
            ExecutionStatus::Completed => "All hooks completed successfully".to_string(),
            ExecutionStatus::Failed => {
                if let Some(error) = &self.error_message {
                    format!("Hook execution failed: {}", error)
                } else {
                    "Hook execution failed".to_string()
                }
            }
            ExecutionStatus::Cancelled => {
                if let Some(reason) = &self.error_message {
                    format!("Hook execution cancelled: {}", reason)
                } else {
                    "Hook execution cancelled".to_string()
                }
            }
        }
    }

    /// Get execution duration
    pub fn duration(&self) -> chrono::Duration {
        let end = self.finished_at.unwrap_or_else(Utc::now);
        end - self.started_at
    }

    /// Get current hook duration (if a hook is currently running)
    #[must_use]
    pub fn current_hook_duration(&self) -> Option<chrono::Duration> {
        self.current_hook_started_at
            .map(|started| Utc::now() - started)
    }

    /// Get the currently executing hook
    #[must_use]
    pub fn current_hook(&self) -> Option<&Hook> {
        self.current_hook_index.and_then(|idx| self.hooks.get(idx))
    }

    /// Format duration in human-readable format (e.g., "2.3s", "1m 15s", "2h 5m")
    #[must_use]
    pub fn format_duration(duration: chrono::Duration) -> String {
        let total_secs = duration.num_seconds();

        if total_secs < 60 {
            format_sub_minute_duration(duration)
        } else if total_secs < 3600 {
            let mins = total_secs / 60;
            let secs = total_secs % 60;
            if secs == 0 {
                format!("{}m", mins)
            } else {
                format!("{}m {}s", mins, secs)
            }
        } else {
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            if mins == 0 {
                format!("{}h", hours)
            } else {
                format!("{}h {}m", hours, mins)
            }
        }
    }

    /// Get a short description of the current or next hook for display
    #[must_use]
    pub fn current_hook_display(&self) -> Option<String> {
        let hook = if let Some(hook) = self.current_hook() {
            Some(hook)
        } else if self.status == ExecutionStatus::Running && self.completed_hooks < self.total_hooks
        {
            self.hooks.get(self.completed_hooks)
        } else {
            None
        };

        hook.map(|h| {
            let cmd_name = h.command.split('/').next_back().unwrap_or(&h.command);
            format!("`{}`", cmd_name)
        })
    }

    /// Check if the completed state should still be displayed
    #[must_use]
    pub fn should_display_completed(&self) -> bool {
        if let Some(display_until) = self.completed_display_until {
            Utc::now() < display_until
        } else {
            false
        }
    }
}

fn format_sub_minute_duration(duration: chrono::Duration) -> String {
    let millis = duration.num_milliseconds();
    let tenths = if millis >= 0 {
        (millis + 50) / 100
    } else {
        (millis - 50) / 100
    };
    let sign = if tenths < 0 { "-" } else { "" };
    let abs_tenths = tenths.unsigned_abs();

    format!("{sign}{}.{:01}s", abs_tenths / 10, abs_tenths % 10)
}

/// Compute a hash for a unique execution instance (directory + config)
#[must_use]
pub fn compute_instance_hash(path: &Path, config_hash: &str, session_id: uuid::Uuid) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cuenv-hook-instance-v2\0");
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(config_hash.as_bytes());
    hasher.update(b":");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(session_id.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compute an execution identity from hook definitions and their input files.
///
/// Missing inputs are part of the identity. Other input errors are returned so
/// an unreadable file cannot be mistaken for a cache hit. Relative hook
/// directories are resolved against the project directory.
///
/// # Errors
///
/// Returns an error if hook definitions cannot be serialized or an input
/// cannot be read as a regular file.
pub fn compute_execution_hash(hooks: &[Hook], base_dir: &Path) -> crate::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"cuenv-hook-execution-v1\0");
    let definitions = serde_json::to_vec(hooks)
        .map_err(|error| crate::Error::serialization(format!("Cannot serialize hooks: {error}")))?;
    let length = u64::try_from(definitions.len())
        .map_err(|_| crate::Error::configuration("Hook definitions are too large to hash"))?;
    hasher.update(length.to_be_bytes());
    hasher.update(definitions);

    for hook in hooks {
        let hook_dir = hook.dir.as_ref().map_or_else(
            || base_dir.to_path_buf(),
            |directory| base_dir.join(directory),
        );

        for input in &hook.inputs {
            let input_path = hook_dir.join(input);
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            let mut file = match options.open(&input_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    hasher.update([0]);
                    continue;
                }
                Err(error) => {
                    return Err(crate::Error::io(error, Some(input_path), "open hook input"));
                }
            };
            let metadata = file.metadata().map_err(|error| {
                crate::Error::io(error, Some(input_path.clone()), "inspect hook input")
            })?;
            if !metadata.is_file() {
                return Err(crate::Error::configuration(format!(
                    "Hook input {} must be a regular file",
                    input_path.display()
                )));
            }
            let mut content_hash = Sha256::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = file.read(&mut buffer).map_err(|error| {
                    crate::Error::io(error, Some(input_path.clone()), "read hook input")
                })?;
                if read == 0 {
                    break;
                }
                content_hash.update(&buffer[..read]);
            }
            hasher.update([1]);
            hasher.update(content_hash.finalize());
        }
    }

    hasher.update(b"\0version\0");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
