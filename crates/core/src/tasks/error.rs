//! Task-domain error type (RFC-0006 phase 2e).
//!
//! Owns the execution, task-failure, task-graph, and timeout variants of the
//! former monolithic `cuenv_core::Error`. Folds into the top-level error via
//! `#[from]`; display output and diagnostic codes are unchanged.

use miette::Diagnostic;
use thiserror::Error as ThisError;

/// Errors raised while building the task graph or executing tasks.
#[derive(ThisError, Debug, Diagnostic)]
pub enum TaskError {
    #[error("Task execution failed: {message}")]
    #[diagnostic(code(cuenv::task::execution))]
    Execution {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Task '{task_name}' failed with exit code {exit_code}")]
    #[diagnostic(code(cuenv::task::failed))]
    TaskFailed {
        task_name: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        #[help]
        help: Option<String>,
    },

    #[error("Task graph error: {message}")]
    #[diagnostic(code(cuenv::task::graph))]
    TaskGraph {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Operation timed out after {seconds} seconds")]
    #[diagnostic(
        code(cuenv::timeout),
        help("Try increasing the timeout or check if the operation is stuck")
    )]
    Timeout { seconds: u64 },
}

impl TaskError {
    #[must_use]
    pub fn execution(msg: impl Into<String>) -> Self {
        TaskError::Execution {
            message: msg.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn execution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::Execution {
            message: msg.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn task_failed(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        TaskError::TaskFailed {
            task_name: task_name.into(),
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn task_failed_with_help(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        TaskError::TaskFailed {
            task_name: task_name.into(),
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn graph(message: impl Into<String>) -> Self {
        TaskError::TaskGraph {
            message: message.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn graph_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::TaskGraph {
            message: message.into(),
            help: Some(help.into()),
        }
    }
}

impl From<cuenv_task_graph::Error> for TaskError {
    fn from(err: cuenv_task_graph::Error) -> Self {
        let help = match &err {
            cuenv_task_graph::Error::CycleDetected { .. } => {
                Some("Check for circular dependencies between tasks".into())
            }
            cuenv_task_graph::Error::MissingDependency { task, dependency } => Some(format!(
                "Add task '{}' or remove it from {}'s dependsOn",
                dependency, task
            )),
            cuenv_task_graph::Error::MissingDependencies { missing } => {
                let suggestions: Vec<String> = missing
                    .iter()
                    .map(|(task, dep)| {
                        format!("  - Add '{}' or remove from {}'s dependsOn", dep, task)
                    })
                    .collect();
                Some(format!(
                    "Fix missing dependencies:\n{}",
                    suggestions.join("\n")
                ))
            }
            cuenv_task_graph::Error::TopologicalSortFailed { .. } => None,
            cuenv_task_graph::Error::DuplicateNodeName {
                name,
                existing_kind,
                new_kind,
            } => Some(format!(
                "Rename the {new_kind} '{name}' to avoid collision with the existing {existing_kind}"
            )),
        };
        TaskError::TaskGraph {
            message: err.to_string(),
            help,
        }
    }
}
