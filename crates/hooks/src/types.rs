//! Type definitions for hooks and hook execution

pub use cuenv_manifest::hook::{Hook, Hooks};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::ExitStatus;

/// Result of executing a single hook
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookResult {
    /// The hook that was executed
    pub hook: Hook,
    /// Whether the execution was successful
    pub success: bool,
    /// Exit status of the command
    pub exit_status: Option<i32>,
    /// Standard output captured from the command
    pub stdout: String,
    /// Standard error captured from the command
    pub stderr: String,
    /// Duration of execution in milliseconds
    pub duration_ms: u64,
    /// Error message if execution failed
    pub error: Option<String>,
}

impl std::fmt::Debug for HookResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookResult")
            .field("success", &self.success)
            .field("exit_status", &self.exit_status)
            .field("duration_ms", &self.duration_ms)
            .finish_non_exhaustive()
    }
}

/// Context captured when a hook execution fails.
#[derive(Debug)]
pub struct HookFailure {
    /// The hook that was executed
    pub hook: Hook,
    /// Exit status of the command
    pub exit_status: Option<ExitStatus>,
    /// Standard output captured from the command
    pub stdout: String,
    /// Standard error captured from the command
    pub stderr: String,
    /// Duration of execution in milliseconds
    pub duration_ms: u64,
    /// Error message describing the failure
    pub error: String,
}

/// Details of a successfully completed hook.
#[derive(Debug)]
pub struct HookSuccess {
    /// The hook that ran.
    pub hook: Hook,
    /// Successful process exit status.
    pub exit_status: ExitStatus,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Execution time in milliseconds.
    pub duration_ms: u64,
}

/// Details of a timed-out hook.
#[derive(Debug)]
pub struct HookTimeout {
    /// The hook that ran.
    pub hook: Hook,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Configured timeout in seconds.
    pub timeout_seconds: u64,
}

impl HookResult {
    /// Create a successful hook result
    #[must_use]
    pub fn success(result: HookSuccess) -> Self {
        Self {
            hook: result.hook,
            success: true,
            exit_status: result.exit_status.code(),
            stdout: result.stdout,
            stderr: result.stderr,
            duration_ms: result.duration_ms,
            error: None,
        }
    }

    /// Create a failed hook result
    #[must_use]
    pub fn failure(failure: HookFailure) -> Self {
        Self {
            hook: failure.hook,
            success: false,
            exit_status: failure.exit_status.and_then(|s| s.code()),
            stdout: failure.stdout,
            stderr: failure.stderr,
            duration_ms: failure.duration_ms,
            error: Some(failure.error),
        }
    }

    /// Create a timeout hook result
    #[must_use]
    pub fn timeout(result: HookTimeout) -> Self {
        Self {
            hook: result.hook,
            success: false,
            exit_status: None,
            stdout: result.stdout,
            stderr: result.stderr,
            duration_ms: result.timeout_seconds * 1000,
            error: Some(format!(
                "Command timed out after {} seconds",
                result.timeout_seconds
            )),
        }
    }
}

/// Configuration for hook execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionConfig {
    /// Default timeout for hooks that don't specify one
    pub default_timeout_seconds: u64,
    /// Whether to stop executing remaining hooks if one fails
    pub fail_fast: bool,
    /// Directory to store execution state
    pub state_dir: Option<PathBuf>,
}

impl Default for HookExecutionConfig {
    fn default() -> Self {
        Self {
            default_timeout_seconds: 300, // 5 minutes
            fail_fast: true,
            state_dir: None, // Will use default state dir
        }
    }
}

/// Status of hook execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Hooks are currently being executed
    Running,
    /// Cancellation was requested; the worker has not yet stopped.
    Cancelling,
    /// All hooks completed successfully
    Completed,
    /// Hook execution failed
    Failed,
    /// Hook execution was cancelled
    Cancelled,
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Cancelling => write!(f, "Cancelling"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_result_success() {
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        };

        // Use Command::new to create a platform-compatible successful exit status
        let exit_status = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                vec!["/C", "exit 0"]
            } else {
                vec![]
            })
            .output()
            .unwrap()
            .status;

        let result = HookResult::success(crate::HookSuccess {
            hook: hook.clone(),
            exit_status,
            stdout: "test\n".to_string(),
            stderr: String::new(),
            duration_ms: 100,
        });

        assert!(result.success);
        assert_eq!(result.hook, hook);
        assert_eq!(result.exit_status, Some(0));
        assert_eq!(result.stdout, "test\n");
        assert_eq!(result.stderr, "");
        assert_eq!(result.duration_ms, 100);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_hook_result_failure() {
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "false".to_string(),
            args: vec![],
            dir: None,
            inputs: vec![],
            source: None,
        };

        // Use Command::new to create a platform-compatible failed exit status
        let exit_status = Some(
            std::process::Command::new(if cfg!(windows) { "cmd" } else { "false" })
                .args(if cfg!(windows) {
                    vec!["/C", "exit 1"]
                } else {
                    vec![]
                })
                .output()
                .unwrap()
                .status,
        );

        let result = HookResult::failure(HookFailure {
            hook: hook.clone(),
            exit_status,
            stdout: String::new(),
            stderr: "command failed".to_string(),
            duration_ms: 50,
            error: "Process exited with non-zero status".to_string(),
        });

        assert!(!result.success);
        assert_eq!(result.hook, hook);
        assert_eq!(result.exit_status, Some(1));
        assert_eq!(result.stderr, "command failed");
        assert_eq!(result.duration_ms, 50);
        assert_eq!(
            result.error,
            Some("Process exited with non-zero status".to_string())
        );
    }

    #[test]
    fn test_hook_result_timeout() {
        let hook = Hook {
            order: 100,
            propagate: false,
            command: "sleep".to_string(),
            args: vec!["1000".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        };

        let result = HookResult::timeout(crate::HookTimeout {
            hook: hook.clone(),
            stdout: String::new(),
            stderr: String::new(),
            timeout_seconds: 10,
        });

        assert!(!result.success);
        assert_eq!(result.hook, hook);
        assert!(result.exit_status.is_none());
        assert_eq!(result.duration_ms, 10000);
        assert!(result.error.as_ref().unwrap().contains("timed out"));
    }

    #[test]
    fn test_execution_config_default() {
        let config = HookExecutionConfig::default();

        assert_eq!(config.default_timeout_seconds, 300);
        assert!(config.fail_fast);
        assert!(config.state_dir.is_none());
    }

    #[test]
    fn test_execution_status_display() {
        assert_eq!(ExecutionStatus::Running.to_string(), "Running");
        assert_eq!(ExecutionStatus::Cancelling.to_string(), "Cancelling");
        assert_eq!(ExecutionStatus::Completed.to_string(), "Completed");
        assert_eq!(ExecutionStatus::Failed.to_string(), "Failed");
        assert_eq!(ExecutionStatus::Cancelled.to_string(), "Cancelled");
    }
}
