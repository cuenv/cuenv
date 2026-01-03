//! Task graph DAG algorithms and dependency resolution for cuenv.
//!
//! This crate provides a directed acyclic graph (DAG) implementation for
//! task dependency resolution and execution ordering using petgraph.
//!
//! # Key Types
//!
//! - [`TaskGraph`]: The main graph structure for building and querying task dependencies
//! - [`TaskNodeData`]: Trait that task types must implement to be stored in the graph
//! - [`GraphNode`]: A node in the graph containing the task name and data
//!
//! # Example
//!
//! ```ignore
//! use cuenv_task_graph::{TaskGraph, TaskNodeData};
//!
//! // Define a simple task type
//! struct MyTask {
//!     depends_on: Vec<String>,
//! }
//!
//! impl TaskNodeData for MyTask {
//!     fn depends_on(&self) -> &[String] {
//!         &self.depends_on
//!     }
//! }
//!
//! // Build a graph
//! let mut graph = TaskGraph::new();
//! graph.add_task("build", MyTask { depends_on: vec![] })?;
//! graph.add_task("test", MyTask { depends_on: vec!["build".to_string()] })?;
//! graph.add_dependency_edges()?;
//!
//! // Get execution order
//! let sorted = graph.topological_sort()?;
//! ```

mod error;
mod graph;
mod traversal;
mod validation;

pub use error::{Error, Result};
pub use graph::{GraphNode, TaskGraph};
pub use traversal::{ParallelGroups, TopologicalOrder};
pub use validation::ValidationResult;

/// Trait for task data that can be stored in the task graph.
///
/// Implement this trait for your task type to enable it to be stored
/// in a [`TaskGraph`] and participate in dependency resolution.
pub trait TaskNodeData: Clone {
    /// Returns the names of tasks this task depends on.
    fn depends_on(&self) -> &[String];

    /// Adds a dependency to this task.
    ///
    /// Default implementation panics. Override this method if mutation is needed
    /// (e.g., for applying group-level dependencies to subtasks).
    ///
    /// # Panics
    ///
    /// Panics if not overridden - implement for task types that need mutable dependency addition.
    #[allow(clippy::unimplemented)]
    fn add_dependency(&mut self, _dep: String) {
        unreachable!("add_dependency not supported for this task type - override in impl")
    }
}
