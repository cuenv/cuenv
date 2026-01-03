//! Error types for task graph operations.

use std::fmt;

/// Result type for task graph operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during task graph operations.
#[derive(Debug, Clone)]
pub enum Error {
    /// A dependency cycle was detected in the graph.
    CycleDetected {
        /// Human-readable description of the cycle.
        message: String,
    },

    /// A task depends on another task that doesn't exist.
    MissingDependency {
        /// The task that has the missing dependency.
        task: String,
        /// The name of the missing dependency.
        dependency: String,
    },

    /// Multiple missing dependencies were found.
    MissingDependencies {
        /// List of (task, missing_dependency) pairs.
        missing: Vec<(String, String)>,
    },

    /// Failed to perform topological sort.
    TopologicalSortFailed {
        /// Reason for the failure.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CycleDetected { message } => {
                write!(f, "Cycle detected in task graph: {message}")
            }
            Self::MissingDependency { task, dependency } => {
                write!(f, "Task '{task}' depends on missing task '{dependency}'")
            }
            Self::MissingDependencies { missing } => {
                let list = missing
                    .iter()
                    .map(|(task, dep)| format!("Task '{task}' depends on missing task '{dep}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "Missing dependencies: {list}")
            }
            Self::TopologicalSortFailed { reason } => {
                write!(f, "Failed to sort tasks topologically: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {}
