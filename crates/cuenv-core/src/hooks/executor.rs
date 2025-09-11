//! Hook executor for sequential execution with fail-fast behavior

use super::{Hook, HookExecutionConfig, HookResult, HookStatus};
use crate::Result;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Sequential hook executor with fail-fast behavior
pub struct HookExecutor {
    config: HookExecutionConfig,
}

impl HookExecutor {
    /// Create a new hook executor with the given configuration
    pub fn new(config: HookExecutionConfig) -> Self {
        Self { config }
    }

    /// Execute a list of hooks sequentially
    ///
    /// Returns a vector of results for all executed hooks.
    /// If fail_fast is enabled, stops at the first failure.
    pub async fn execute_hooks(&self, hooks: Vec<Hook>) -> Result<Vec<HookResult>> {
        let mut results = Vec::new();
        let total = hooks.len();

        info!("Starting execution of {} hooks", total);

        for (index, hook) in hooks.into_iter().enumerate() {
            info!(
                "Executing hook {}/{}: {}",
                index + 1,
                total,
                hook.command
            );

            let result = self.execute_single_hook(hook).await?;
            let failed = matches!(result.status, HookStatus::Failed(_));

            results.push(result);

            if failed && self.config.fail_fast {
                warn!(
                    "Hook {} failed, stopping execution (fail_fast enabled)",
                    index + 1
                );
                break;
            }
        }

        Ok(results)
    }

    /// Execute a single hook
    pub async fn execute_single_hook(&self, hook: Hook) -> Result<HookResult> {
        let mut result = HookResult::pending(hook.clone());
        result.start();

        debug!(
            "Executing command: {} {:?} in {}",
            hook.command,
            hook.args,
            hook.dir.display()
        );

        // Build the command
        let mut cmd = Command::new(&hook.command);
        cmd.args(&hook.args)
            .current_dir(&hook.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        // Execute with timeout
        match timeout(self.config.timeout, cmd.output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                debug!("Hook completed with exit code: {}", exit_code);
                result.complete(exit_code, stdout, stderr);
            }
            Ok(Err(e)) => {
                error!("Failed to execute hook: {}", e);
                result.fail(format!("Failed to execute command: {}", e));
            }
            Err(_) => {
                error!("Hook execution timed out after {:?}", self.config.timeout);
                result.fail(format!("Command timed out after {:?}", self.config.timeout));
            }
        }

        Ok(result)
    }

    /// Execute hooks and return a summary
    pub async fn execute_with_summary(&self, hooks: Vec<Hook>) -> Result<ExecutionSummary> {
        let total = hooks.len();
        let results = self.execute_hooks(hooks).await?;

        let completed = results
            .iter()
            .filter(|r| matches!(r.status, HookStatus::Completed))
            .count();
        let failed = results
            .iter()
            .filter(|r| matches!(r.status, HookStatus::Failed(_)))
            .count();
        let pending = results
            .iter()
            .filter(|r| matches!(r.status, HookStatus::Pending))
            .count();

        Ok(ExecutionSummary {
            total,
            completed,
            failed,
            pending,
            results,
        })
    }
}

/// Summary of hook execution
#[derive(Debug, Clone)]
pub struct ExecutionSummary {
    /// Total number of hooks
    pub total: usize,
    /// Number of completed hooks
    pub completed: usize,
    /// Number of failed hooks
    pub failed: usize,
    /// Number of pending hooks (not executed)
    pub pending: usize,
    /// Individual results
    pub results: Vec<HookResult>,
}

impl ExecutionSummary {
    /// Check if all hooks completed successfully
    pub fn all_succeeded(&self) -> bool {
        self.failed == 0 && self.completed == self.total
    }

    /// Get a status string like "2/5 Completed"
    pub fn status_string(&self) -> String {
        if self.all_succeeded() {
            "cuenv Activated".to_string()
        } else if self.failed > 0 {
            "cuenv Failed".to_string()
        } else {
            format!("{}/{} Completed", self.completed, self.total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_test_hook(command: &str) -> Hook {
        Hook {
            command: command.to_string(),
            args: vec![],
            dir: PathBuf::from("."),
            source: false,
            preload: false,
        }
    }

    #[tokio::test]
    async fn test_executor_creation() {
        let config = HookExecutionConfig::default();
        let executor = HookExecutor::new(config);
        assert!(matches!(executor, HookExecutor { .. }));
    }

    #[tokio::test]
    async fn test_execute_single_success() {
        let config = HookExecutionConfig::default();
        let executor = HookExecutor::new(config);

        // Use a command that should exist on most systems
        let hook = Hook {
            command: "echo".to_string(),
            args: vec!["test".to_string()],
            dir: PathBuf::from("."),
            source: false,
            preload: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert_eq!(result.status, HookStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.unwrap().contains("test"));
    }

    #[tokio::test]
    async fn test_execute_single_failure() {
        let config = HookExecutionConfig::default();
        let executor = HookExecutor::new(config);

        // Use a command that should fail
        let hook = Hook {
            command: "false".to_string(),
            args: vec![],
            dir: PathBuf::from("."),
            source: false,
            preload: false,
        };

        let result = executor.execute_single_hook(hook).await.unwrap();
        assert!(matches!(result.status, HookStatus::Failed(_)));
        assert_ne!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn test_execution_summary() {
        let summary = ExecutionSummary {
            total: 5,
            completed: 3,
            failed: 1,
            pending: 1,
            results: vec![],
        };

        assert!(!summary.all_succeeded());
        assert_eq!(summary.status_string(), "cuenv Failed");

        let summary = ExecutionSummary {
            total: 3,
            completed: 3,
            failed: 0,
            pending: 0,
            results: vec![],
        };

        assert!(summary.all_succeeded());
        assert_eq!(summary.status_string(), "cuenv Activated");

        let summary = ExecutionSummary {
            total: 5,
            completed: 2,
            failed: 0,
            pending: 3,
            results: vec![],
        };

        assert_eq!(summary.status_string(), "2/5 Completed");
    }
}
