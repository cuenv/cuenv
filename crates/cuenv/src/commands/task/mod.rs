//! Task execution command implementation

mod arguments;
mod cache;
mod dag_export;
mod discovery;
mod execution;
pub mod list_builder;
mod remote_cache;
mod rendering;
mod runner;
mod types;

use cuenv_core::Result;
use tracing::instrument;

// Re-export types for the public API. Some types may not be used externally yet.
pub use types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};

/// Execute a task using the new structured request API.
///
/// This is the preferred entry point for task execution. It accepts a
/// `TaskExecutionRequest` which groups all parameters into a structured
/// format with type-safe selection modes.
///
/// # Errors
///
/// Returns an error if task resolution, validation, or execution fails.
///
/// # Example
///
/// ```ignore
/// let request = TaskExecutionRequest::named("./", "cuenv", "build")
///     .with_args(vec!["--release".to_string()])
///     .with_environment("prod");
///
/// let output = execute(request).await?;
/// ```
#[instrument(name = "task_execute", skip(request), fields(path = %request.path, package = %request.package))]
pub async fn execute(request: TaskExecutionRequest<'_>) -> Result<String> {
    execution::execute_task_impl(&request).await
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
