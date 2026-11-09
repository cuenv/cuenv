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
pub use types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskLocation, TaskSelection};

/// Execute a task using a structured request.
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
/// ```
/// use cuenv::commands::{CommandExecutor, task::{self, TaskExecutionRequest, TaskLocation}};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let directory = tempfile::Builder::new().prefix("cuenv-doc-").tempdir()?;
/// # std::fs::create_dir(directory.path().join("cue.mod"))?;
/// # std::fs::write(directory.path().join("cue.mod/module.cue"), "module: \"example.com/task-doc\"\nlanguage: version: \"v0.9.0\"\n")?;
/// # std::fs::write(directory.path().join("env.cue"), "package cuenv\nname: \"task-doc\"\ntasks: build: {command: \"echo\", args: [\"build\"], hermetic: false}\n")?;
/// let executor = CommandExecutor::new("cuenv".to_string());
/// let location = TaskLocation::new(directory.path().to_string_lossy(), "cuenv");
/// let request = TaskExecutionRequest::named(location, "build", &executor)
///     .with_args(vec!["--release".to_string()]);
///
/// tokio::runtime::Runtime::new()?.block_on(task::execute(request))?;
/// # Ok(())
/// # }
/// ```
#[instrument(name = "task_execute", skip(request), fields(path = %request.path, package = %request.package))]
pub async fn execute(request: TaskExecutionRequest<'_>) -> Result<String> {
    execution::execute_task_impl(&request).await
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
