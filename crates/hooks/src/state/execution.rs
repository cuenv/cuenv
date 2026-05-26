use crate::types::{ExecutionStatus, Hook, HookResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info};

/// Represents the state of hook execution for a specific directory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookExecutionState {
    /// Hash combining directory path and config (instance identifier)
    pub instance_hash: String,
    /// Path to the directory being processed
    pub directory_path: PathBuf,
    /// Hash of the configuration that was approved
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
    /// Previous environment variables (for diff/unset support)
    pub previous_env: Option<HashMap<String, String>>,
}

impl HookExecutionState {
    /// Create a new execution state
    #[must_use]
    pub fn new(
        directory_path: PathBuf,
        instance_hash: String,
        config_hash: String,
        hooks: Vec<Hook>,
    ) -> Self {
        let total_hooks = hooks.len();
        Self {
            instance_hash,
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
            previous_env: None,
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Takes ownership for API clarity, cloning is intentional"
    )]
    pub fn record_hook_result(&mut self, hook_index: usize, result: HookResult) {
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
            error!(
                "Hook {} of {} failed: {:?}",
                hook_index + 1,
                self.total_hooks,
                result.error
            );
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
            let millis = duration.num_milliseconds();
            #[expect(
                clippy::cast_precision_loss,
                reason = "Display formatting, precision loss is acceptable"
            )]
            let secs = millis as f64 / 1000.0;
            format!("{secs:.1}s")
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

/// Compute a hash for a unique execution instance (directory + config)
#[must_use]
pub fn compute_instance_hash(path: &Path, config_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(config_hash.as_bytes());
    hasher.update(b":");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Compute a hash for hook execution that includes input file contents.
///
/// This is separate from the approval hash - approval only cares about the hook
/// definition, but execution cache needs to invalidate when input files change.
pub fn compute_execution_hash(hooks: &[Hook], base_dir: &Path) -> String {
    let mut hasher = Sha256::new();

    if let Ok(hooks_json) = serde_json::to_string(hooks) {
        hasher.update(hooks_json.as_bytes());
    }

    for hook in hooks {
        let hook_dir = hook
            .dir
            .as_ref()
            .map_or_else(|| base_dir.to_path_buf(), PathBuf::from);

        for input in &hook.inputs {
            let input_path = hook_dir.join(input);
            if let Ok(content) = std::fs::read(&input_path) {
                hasher.update(b"file:");
                hasher.update(input.as_bytes());
                hasher.update(b":");
                hasher.update(&content);
            }
        }
    }

    hasher.update(b":version:");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());

    format!("{:x}", hasher.finalize())[..16].to_string()
}
