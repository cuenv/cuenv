//! Hook execution system for cuenv
//!
//! This module provides the infrastructure for executing hooks in cuenv configurations,
//! including sequential execution with fail-fast behavior, status tracking, and
//! background process management.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub mod executor;
pub mod state;

/// A hook to be executed in an environment
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hook {
    /// The command to execute
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Directory to execute the command in
    #[serde(default = "default_dir")]
    pub dir: PathBuf,
    /// Whether to source the command into the environment
    #[serde(default)]
    pub source: bool,
    /// Whether this hook should be preloaded
    #[serde(default)]
    pub preload: bool,
}

fn default_dir() -> PathBuf {
    PathBuf::from(".")
}

/// The type of hook event
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// Executed when entering a directory
    OnEnter,
    /// Executed when exiting a directory
    OnExit,
}

/// Status of hook execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookStatus {
    /// Hook has not started execution
    Pending,
    /// Hook is currently running
    Running,
    /// Hook completed successfully
    Completed,
    /// Hook failed with an error message
    Failed(String),
}

/// Result of a single hook execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    /// The hook that was executed
    pub hook: Hook,
    /// Status of the execution
    pub status: HookStatus,
    /// Standard output from the hook
    pub stdout: Option<String>,
    /// Standard error from the hook
    pub stderr: Option<String>,
    /// Exit code from the hook
    pub exit_code: Option<i32>,
    /// Duration of the execution in milliseconds
    pub duration_ms: Option<u64>,
    /// When the hook started executing (Unix timestamp)
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the hook finished executing (Unix timestamp)
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl HookResult {
    /// Create a new pending hook result
    pub fn pending(hook: Hook) -> Self {
        Self {
            hook,
            status: HookStatus::Pending,
            stdout: None,
            stderr: None,
            exit_code: None,
            duration_ms: None,
            started_at: None,
            finished_at: None,
        }
    }

    /// Mark the hook as running
    pub fn start(&mut self) {
        self.status = HookStatus::Running;
        self.started_at = Some(chrono::Utc::now());
    }

    /// Mark the hook as completed
    pub fn complete(&mut self, exit_code: i32, stdout: String, stderr: String) {
        let now = chrono::Utc::now();
        if let Some(started) = self.started_at {
            self.duration_ms = Some((now - started).num_milliseconds() as u64);
        }
        self.finished_at = Some(now);
        self.exit_code = Some(exit_code);
        self.stdout = Some(stdout);
        self.stderr = Some(stderr);
        
        if exit_code == 0 {
            self.status = HookStatus::Completed;
        } else {
            self.status = HookStatus::Failed(format!("Hook exited with code {}", exit_code));
        }
    }

    /// Mark the hook as failed
    pub fn fail(&mut self, error: String) {
        let now = chrono::Utc::now();
        if let Some(started) = self.started_at {
            self.duration_ms = Some((now - started).num_milliseconds() as u64);
        }
        self.finished_at = Some(now);
        self.status = HookStatus::Failed(error);
    }
}

/// Configuration for hook execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionConfig {
    /// Maximum time to wait for a hook to complete
    pub timeout: Duration,
    /// Whether to run hooks in parallel (not used for sequential execution)
    pub parallel: bool,
    /// Whether to stop on first failure
    pub fail_fast: bool,
    /// Working directory for hook execution
    pub working_dir: PathBuf,
}

impl Default for HookExecutionConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300), // 5 minutes default
            parallel: false,
            fail_fast: true,
            working_dir: PathBuf::from("."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_creation() {
        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            dir: PathBuf::from("/tmp"),
            source: false,
            preload: false,
        };

        assert_eq!(hook.command, "echo");
        assert_eq!(hook.args, vec!["hello"]);
        assert_eq!(hook.dir, PathBuf::from("/tmp"));
        assert!(!hook.source);
        assert!(!hook.preload);
    }

    #[test]
    fn test_hook_result_lifecycle() {
        let hook = Hook {
            command: "test".to_string(),
            args: vec![],
            dir: PathBuf::from("."),
            source: false,
            preload: false,
        };

        let mut result = HookResult::pending(hook);
        assert_eq!(result.status, HookStatus::Pending);
        assert!(result.started_at.is_none());

        result.start();
        assert_eq!(result.status, HookStatus::Running);
        assert!(result.started_at.is_some());

        result.complete(0, "output".to_string(), "".to_string());
        assert_eq!(result.status, HookStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, Some("output".to_string()));
        assert!(result.duration_ms.is_some());
    }

    #[test]
    fn test_hook_result_failure() {
        let hook = Hook {
            command: "test".to_string(),
            args: vec![],
            dir: PathBuf::from("."),
            source: false,
            preload: false,
        };

        let mut result = HookResult::pending(hook);
        result.start();
        result.complete(1, "".to_string(), "error".to_string());
        
        assert!(matches!(result.status, HookStatus::Failed(_)));
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn test_hook_execution_config_default() {
        let config = HookExecutionConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(300));
        assert!(!config.parallel);
        assert!(config.fail_fast);
        assert_eq!(config.working_dir, PathBuf::from("."));
    }
}