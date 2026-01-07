//! Validation utilities for task graphs.
//!
//! This module provides types and functions for validating task graph structure.

use crate::{Error, TaskGraph, TaskNodeData};

/// Result of graph validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the graph is valid (no cycles, no missing dependencies).
    pub is_valid: bool,
    /// List of validation errors, if any.
    pub errors: Vec<Error>,
}

impl ValidationResult {
    /// Create a valid result.
    #[must_use]
    pub fn valid() -> Self {
        Self {
            is_valid: true,
            errors: vec![],
        }
    }

    /// Create an invalid result with errors.
    #[must_use]
    pub fn invalid(errors: Vec<Error>) -> Self {
        Self {
            is_valid: false,
            errors,
        }
    }
}

impl<T: TaskNodeData> TaskGraph<T> {
    /// Validate the graph structure.
    ///
    /// Checks for:
    /// - Cycles in the dependency graph
    ///
    /// Note: Missing dependencies are caught during `add_dependency_edges()`,
    /// so this method primarily checks for cycles after edges are added.
    #[must_use]
    pub fn validate(&self) -> ValidationResult {
        let mut errors = Vec::new();

        if self.has_cycles() {
            errors.push(Error::CycleDetected {
                message: "Task dependency graph contains cycles".to_string(),
            });
        }

        if errors.is_empty() {
            ValidationResult::valid()
        } else {
            ValidationResult::invalid(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default)]
    struct TestTask {
        depends_on: Vec<String>,
    }

    impl TaskNodeData for TestTask {
        fn dependency_names(&self) -> impl Iterator<Item = &str> {
            self.depends_on.iter().map(String::as_str)
        }
    }

    #[test]
    fn test_validate_empty_graph() {
        let graph: TaskGraph<TestTask> = TaskGraph::new();
        let result = graph.validate();
        assert!(result.is_valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_valid_graph() {
        let mut graph = TaskGraph::new();
        graph
            .add_task("a", TestTask { depends_on: vec![] })
            .unwrap();
        graph
            .add_task(
                "b",
                TestTask {
                    depends_on: vec!["a".to_string()],
                },
            )
            .unwrap();
        graph.add_dependency_edges().unwrap();

        let result = graph.validate();
        assert!(result.is_valid);
    }

    #[test]
    fn test_validate_cyclic_graph() {
        let mut graph = TaskGraph::new();
        graph
            .add_task(
                "a",
                TestTask {
                    depends_on: vec!["b".to_string()],
                },
            )
            .unwrap();
        graph
            .add_task(
                "b",
                TestTask {
                    depends_on: vec!["a".to_string()],
                },
            )
            .unwrap();
        graph.add_dependency_edges().unwrap();

        let result = graph.validate();
        assert!(!result.is_valid);
        assert_eq!(result.errors.len(), 1);
    }
}
