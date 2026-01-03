//! Type definitions for hooks and hook execution

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

/// Default order for hooks (100)
fn default_order() -> i32 {
    100
}

/// A hook represents a command that can be executed when entering or exiting environments
/// Based on schema/hooks.cue #ExecHook definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hook {
    /// Execution order within a single env.cue (lower runs first, default 100)
    #[serde(default = "default_order")]
    pub order: i32,
    /// Whether this hook propagates to child directories (default false)
    #[serde(default)]
    pub propagate: bool,
    /// The command to execute
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for command execution (defaults to ".")
    #[serde(default)]
    pub dir: Option<String>,
    /// Input files that trigger re-execution when changed
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Whether to source the command output as shell script to capture environment changes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<bool>,
}

/// Result of executing a single hook
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

impl HookResult {
    /// Create a successful hook result
    #[must_use]
    pub fn success(
        hook: Hook,
        exit_status: ExitStatus,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    ) -> Self {
        Self {
            hook,
            success: true,
            exit_status: exit_status.code(),
            stdout,
            stderr,
            duration_ms,
            error: None,
        }
    }

    /// Create a failed hook result
    #[allow(clippy::too_many_arguments)] // Hook result requires full execution context
    #[must_use]
    pub fn failure(
        hook: Hook,
        exit_status: Option<ExitStatus>,
        stdout: String,
        stderr: String,
        duration_ms: u64,
        error: String,
    ) -> Self {
        Self {
            hook,
            success: false,
            exit_status: exit_status.and_then(|s| s.code()),
            stdout,
            stderr,
            duration_ms,
            error: Some(error),
        }
    }

    /// Create a timeout hook result
    #[must_use]
    pub fn timeout(hook: Hook, stdout: String, stderr: String, timeout_seconds: u64) -> Self {
        Self {
            hook,
            success: false,
            exit_status: None,
            stdout,
            stderr,
            duration_ms: timeout_seconds * 1000,
            error: Some(format!(
                "Command timed out after {} seconds",
                timeout_seconds
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
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Collection of hooks that can be executed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Hooks {
    /// Named hooks to execute when entering an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onEnter")]
    pub on_enter: Option<HashMap<String, Hook>>,

    /// Named hooks to execute when exiting an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onExit")]
    pub on_exit: Option<HashMap<String, Hook>>,

    /// Named hooks to execute before git push (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "prePush")]
    pub pre_push: Option<HashMap<String, Hook>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_serialization() {
        let hook = Hook {
            order: 50,
            propagate: false,
            command: "npm".to_string(),
            args: vec!["install".to_string()],
            dir: Some("/tmp".to_string()),
            inputs: vec![],
            source: Some(false),
        };

        let json = serde_json::to_string(&hook).unwrap();
        let deserialized: Hook = serde_json::from_str(&json).unwrap();

        assert_eq!(hook, deserialized);
    }

    #[test]
    fn test_hook_defaults() {
        let json = r#"{"command": "echo", "args": ["hello"]}"#;
        let hook: Hook = serde_json::from_str(json).unwrap();

        assert_eq!(hook.order, 100); // default order
        assert_eq!(hook.command, "echo");
        assert_eq!(hook.args, vec!["hello"]);
        assert_eq!(hook.dir, None);
        assert!(hook.inputs.is_empty());
        assert_eq!(hook.source, None); // default
    }

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

        let result = HookResult::success(
            hook.clone(),
            exit_status,
            "test\n".to_string(),
            String::new(),
            100,
        );

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

        let result = HookResult::failure(
            hook.clone(),
            exit_status,
            String::new(),
            "command failed".to_string(),
            50,
            "Process exited with non-zero status".to_string(),
        );

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

        let result = HookResult::timeout(hook.clone(), String::new(), String::new(), 10);

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
        assert_eq!(ExecutionStatus::Completed.to_string(), "Completed");
        assert_eq!(ExecutionStatus::Failed.to_string(), "Failed");
        assert_eq!(ExecutionStatus::Cancelled.to_string(), "Cancelled");
    }
}
