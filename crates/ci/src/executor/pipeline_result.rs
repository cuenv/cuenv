//! The result of a CI pipeline execution.

use super::runner::TaskOutput;

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
