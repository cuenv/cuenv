//! CI Pipeline Executor
//!
//! Orchestrates CI pipeline execution with provider integration.

pub mod config;
mod hook_env;
mod orchestrator;
mod reporting;
pub mod runner;
pub mod secrets;
mod task_env;
mod task_execution;
mod tools;

pub use config::CIExecutorConfig;
pub use orchestrator::{RunCiRequest, run_ci};
pub use runner::TaskOutput;
pub use secrets::{EnvSecretResolver, MockSecretResolver, SaltConfig, SecretResolver};

pub use crate::error::ExecutorError;

/// Result of pipeline execution
#[derive(Debug)]
pub struct PipelineResult {
    /// Whether all tasks succeeded
    pub success: bool,
    /// Results for each task
    pub tasks: Vec<TaskOutput>,
    /// Total execution time in milliseconds
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // ExecutorError display tests
    // ==========================================================================

    #[test]
    fn test_executor_error_compilation_display() {
        let err = ExecutorError::Compilation("Syntax error in line 5".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Failed to compile"));
        assert!(msg.contains("Syntax error in line 5"));
    }

    #[test]
    fn test_executor_error_task_panic_display() {
        let err = ExecutorError::TaskPanic("thread 'main' panicked".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Task panicked"));
        assert!(msg.contains("thread 'main' panicked"));
    }

    #[test]
    fn test_executor_error_pipeline_not_found_display() {
        let err = ExecutorError::PipelineNotFound {
            name: "build".to_string(),
            available: "default, test, deploy".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Pipeline 'build' not found"));
        assert!(msg.contains("default, test, deploy"));
    }

    #[test]
    fn test_executor_error_no_ci_config_display() {
        let err = ExecutorError::NoCIConfig;
        let msg = err.to_string();
        assert!(msg.contains("has no CI configuration"));
    }

    // ==========================================================================
    // PipelineResult tests
    // ==========================================================================

    #[test]
    fn test_pipeline_result_fields() {
        let result = PipelineResult {
            success: true,
            tasks: vec![TaskOutput::dry_run("task1".to_string())],
            duration_ms: 1500,
        };

        assert!(result.success);
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.duration_ms, 1500);
    }

    #[test]
    fn test_pipeline_result_failed() {
        let result = PipelineResult {
            success: false,
            tasks: vec![],
            duration_ms: 0,
        };

        assert!(!result.success);
    }

    // ==========================================================================
    // CIExecutorConfig builder tests
    // ==========================================================================

    #[test]
    fn test_executor_config_builder() {
        let config = CIExecutorConfig::new(std::path::PathBuf::from("/project"))
            .with_max_parallel(8)
            .with_dry_run(cuenv_core::DryRun::Yes);

        assert_eq!(config.max_parallel, 8);
        assert!(config.dry_run.is_dry_run());
    }

    #[test]
    fn test_executor_config_default() {
        let config = CIExecutorConfig::default();
        assert!(!config.dry_run.is_dry_run());
        assert!(config.max_parallel >= 1);
    }

    #[test]
    fn test_executor_config_with_capture_output() {
        let config = CIExecutorConfig::new(std::path::PathBuf::from("/project"))
            .with_capture_output(cuenv_core::OutputCapture::Capture);

        assert!(config.capture_output.should_capture());
    }

    // ==========================================================================
    // TaskOutput helper tests
    // ==========================================================================

    #[test]
    fn test_task_output_dry_run() {
        let output = TaskOutput::dry_run("my-task".to_string());
        assert!(output.success);
        assert_eq!(output.task_id, "my-task");
    }

    #[test]
    fn test_task_output_from_cache() {
        let output = TaskOutput::from_cache("cached-task".to_string(), 500);
        assert!(output.success);
        assert_eq!(output.task_id, "cached-task");
        assert_eq!(output.duration_ms, 500);
    }
}
