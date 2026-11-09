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
//! ```
//! use cuenv_task_graph::{TaskGraph, TaskNodeData};
//!
//! // Define a simple task type
//! #[derive(Clone)]
//! struct MyTask {
//!     depends_on: Vec<String>,
//! }
//!
//! impl TaskNodeData for MyTask {
//!     fn dependency_names(&self) -> impl Iterator<Item = &str> {
//!         self.depends_on.iter().map(String::as_str)
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
//! assert_eq!(sorted.iter().map(|node| node.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
//! # Ok::<(), cuenv_task_graph::Error>(())
//! ```

mod error;
mod graph;
mod resolver;
mod task_node_data;
mod traversal;
mod validation;

pub use error::{Error, Result};
pub use graph::{GraphNode, NodeKind, TaskGraph, compute_transitive_closure};
pub use resolver::{TaskResolution, TaskResolver};
pub use task_node_data::{MutableTaskNodeData, TaskNodeData};
pub use traversal::{ParallelGroups, TopologicalOrder};
pub use validation::ValidationResult;
