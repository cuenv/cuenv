//! Hook execution system for cuenv
//!
//! This module provides the core types and functionality for executing hooks
//! in the background with status tracking and persistence.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;
use chrono::{DateTime, Utc};

pub mod executor;
pub mod state;

/// Status of hook execution
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookStatus {
    /// Hooks are pending execution
    Pending,
    /// Hooks are currently running
    Running,
    /// All hooks completed successfully
    Completed,
    /// Hook execution failed with error message
    Failed(String),
}

impl std::fmt::Display for HookStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookStatus::Pending => write!(f, "Pending"),
            HookStatus::Running => write!(f, "Running"),
            HookStatus::Completed => write!(f, "cuenv Activated"),
            HookStatus::Failed(msg) => write!(f, "cuenv Failed: {msg}"),
        }
    }
}

/// Result of executing a single hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    /// Index of the hook in the execution sequence
    pub index: usize,
    /// Exit code of the hook command
    pub exit_code: i32,
    /// Standard output from the hook
    pub stdout: String,
    /// Standard error from the hook
    pub stderr: String,
    /// Duration of hook execution in milliseconds
    pub duration_ms: u64,
    /// Timestamp when hook completed
    pub completed_at: DateTime<Utc>,
}

impl HookResult {
    /// Check if this hook result represents success
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }
}

/// State of hook execution for a directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionState {
    /// Unique ID for this execution session
    pub session_id: Uuid,
    /// Directory where hooks are being executed
    pub directory: PathBuf,
    /// Hash of the configuration that was approved
    pub config_hash: String,
    /// Total number of hooks to execute
    pub total_hooks: usize,
    /// Number of hooks completed (successfully or not)
    pub completed_hooks: usize,
    /// Current execution status
    pub status: HookStatus,
    /// Index of currently executing hook (if running)
    pub current_hook_index: Option<usize>,
    /// Results from completed hooks
    pub results: Vec<HookResult>,
    /// Timestamp when execution started
    pub started_at: DateTime<Utc>,
    /// Timestamp when execution finished (if completed or failed)
    pub finished_at: Option<DateTime<Utc>>,
}

impl HookExecutionState {
    /// Create a new hook execution state
    #[must_use]
    pub fn new(directory: PathBuf, config_hash: String, total_hooks: usize) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            directory,
            config_hash,
            total_hooks,
            completed_hooks: 0,
            status: HookStatus::Pending,
            current_hook_index: None,
            results: Vec::new(),
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    /// Get progress as a display string (e.g., "1/3 Completed")
    #[must_use]
    pub fn progress_display(&self) -> String {
        match &self.status {
            HookStatus::Pending => format!("0/{} Completed", self.total_hooks),
            HookStatus::Running => format!("{}/{} Completed", self.completed_hooks, self.total_hooks),
            HookStatus::Completed => "cuenv Activated".to_string(),
            HookStatus::Failed(msg) => format!("cuenv Failed: {msg}"),
        }
    }

    /// Check if execution is finished (completed or failed)
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self.status, HookStatus::Completed | HookStatus::Failed(_))
    }

    /// Mark execution as started
    pub fn mark_started(&mut self) {
        self.status = HookStatus::Running;
    }

    /// Mark a hook as starting execution
    pub fn mark_hook_started(&mut self, hook_index: usize) {
        self.current_hook_index = Some(hook_index);
    }

    /// Add a completed hook result
    pub fn add_result(&mut self, result: HookResult) {
        let hook_index = result.index;
        let exit_code = result.exit_code;
        let is_success = result.is_success();
        
        self.results.push(result);
        self.completed_hooks = self.results.len();
        self.current_hook_index = None;

        // Check if this hook failed
        if !is_success {
            self.status = HookStatus::Failed(format!("Hook {} failed with exit code {}", 
                hook_index, exit_code));
            self.finished_at = Some(Utc::now());
        } else if self.completed_hooks >= self.total_hooks {
            // All hooks completed successfully
            self.status = HookStatus::Completed;
            self.finished_at = Some(Utc::now());
        }
    }
}

/// Hook execution command (matches schema/hooks.cue ExecHook)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecHook {
    /// Command to execute
    pub command: String,
    /// Command arguments
    pub args: Option<Vec<String>>,
    /// Working directory (defaults to ".")
    pub dir: Option<String>,
    /// Input files/dependencies
    pub inputs: Option<Vec<String>>,
    /// Whether to source the command output
    pub source: Option<bool>,
    /// Whether to preload before execution
    pub preload: Option<bool>,
}

impl Default for ExecHook {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: None,
            dir: Some(".".to_string()),
            inputs: None,
            source: None,
            preload: Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_status_display() {
        assert_eq!(HookStatus::Pending.to_string(), "Pending");
        assert_eq!(HookStatus::Running.to_string(), "Running");  
        assert_eq!(HookStatus::Completed.to_string(), "cuenv Activated");
        assert_eq!(HookStatus::Failed("test error".to_string()).to_string(), "cuenv Failed: test error");
    }

    #[test]
    fn test_hook_result_is_success() {
        let success_result = HookResult {
            index: 0,
            exit_code: 0,
            stdout: "output".to_string(),
            stderr: "".to_string(),
            duration_ms: 100,
            completed_at: Utc::now(),
        };
        assert!(success_result.is_success());

        let failure_result = HookResult {
            index: 1,
            exit_code: 1,
            stdout: "".to_string(),
            stderr: "error".to_string(),
            duration_ms: 50,
            completed_at: Utc::now(),
        };
        assert!(!failure_result.is_success());
    }

    #[test]
    fn test_hook_execution_state_new() {
        let directory = PathBuf::from("/test/dir");
        let config_hash = "abc123".to_string();
        let state = HookExecutionState::new(directory.clone(), config_hash.clone(), 3);

        assert_eq!(state.directory, directory);
        assert_eq!(state.config_hash, config_hash);
        assert_eq!(state.total_hooks, 3);
        assert_eq!(state.completed_hooks, 0);
        assert_eq!(state.status, HookStatus::Pending);
        assert_eq!(state.current_hook_index, None);
        assert!(state.results.is_empty());
        assert!(state.finished_at.is_none());
    }

    #[test]
    fn test_hook_execution_state_progress_display() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 2);
        
        assert_eq!(state.progress_display(), "0/2 Completed");
        
        state.mark_started();
        assert_eq!(state.progress_display(), "0/2 Completed");
        
        let result = HookResult {
            index: 0,
            exit_code: 0,
            stdout: "".to_string(),
            stderr: "".to_string(),
            duration_ms: 100,
            completed_at: Utc::now(),
        };
        state.add_result(result);
        assert_eq!(state.progress_display(), "1/2 Completed");
    }

    #[test]
    fn test_hook_execution_state_mark_started() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 1);
        
        state.mark_started();
        assert_eq!(state.status, HookStatus::Running);
    }

    #[test]
    fn test_hook_execution_state_mark_hook_started() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 2);
        
        state.mark_hook_started(1);
        assert_eq!(state.current_hook_index, Some(1));
    }

    #[test]
    fn test_hook_execution_state_add_result_success() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 1);
        
        let result = HookResult {
            index: 0,
            exit_code: 0,
            stdout: "success".to_string(),
            stderr: "".to_string(),
            duration_ms: 200,
            completed_at: Utc::now(),
        };
        
        state.add_result(result.clone());
        
        assert_eq!(state.completed_hooks, 1);
        assert_eq!(state.status, HookStatus::Completed);
        assert!(state.finished_at.is_some());
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].exit_code, 0);
        assert_eq!(state.current_hook_index, None);
    }

    #[test]
    fn test_hook_execution_state_add_result_failure() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 2);
        
        let result = HookResult {
            index: 0,
            exit_code: 1,
            stdout: "".to_string(),
            stderr: "error occurred".to_string(),
            duration_ms: 150,
            completed_at: Utc::now(),
        };
        
        state.add_result(result);
        
        assert_eq!(state.completed_hooks, 1);
        assert!(matches!(state.status, HookStatus::Failed(_)));
        assert!(state.finished_at.is_some());
        assert!(state.is_finished());
    }

    #[test]
    fn test_hook_execution_state_is_finished() {
        let mut state = HookExecutionState::new(PathBuf::from("/test"), "hash".to_string(), 1);
        
        assert!(!state.is_finished());
        
        state.status = HookStatus::Running;
        assert!(!state.is_finished());
        
        state.status = HookStatus::Completed;
        assert!(state.is_finished());
        
        state.status = HookStatus::Failed("error".to_string());
        assert!(state.is_finished());
    }

    #[test]
    fn test_exec_hook_default() {
        let hook = ExecHook::default();
        assert!(hook.command.is_empty());
        assert_eq!(hook.args, None);
        assert_eq!(hook.dir, Some(".".to_string()));
        assert_eq!(hook.inputs, None);
        assert_eq!(hook.source, None);
        assert_eq!(hook.preload, Some(false));
    }

    #[test]
    fn test_hook_status_serialization() {
        // Test that HookStatus can be serialized/deserialized
        let status = HookStatus::Failed("test error".to_string());
        let serialized = serde_json::to_string(&status).unwrap();
        let deserialized: HookStatus = serde_json::from_str(&serialized).unwrap();
        assert_eq!(status, deserialized);
    }

    #[test]
    fn test_hook_execution_state_serialization() {
        // Test that HookExecutionState can be serialized/deserialized
        let state = HookExecutionState::new(PathBuf::from("/test"), "hash123".to_string(), 3);
        let serialized = serde_json::to_string(&state).unwrap();
        let deserialized: HookExecutionState = serde_json::from_str(&serialized).unwrap();
        
        assert_eq!(state.directory, deserialized.directory);
        assert_eq!(state.config_hash, deserialized.config_hash);
        assert_eq!(state.total_hooks, deserialized.total_hooks);
        assert_eq!(state.status, deserialized.status);
    }
}