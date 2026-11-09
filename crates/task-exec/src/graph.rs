//! Task graph builder using cuenv-task-graph.
//!
//! This module builds directed acyclic graphs (DAGs) from task definitions
//! to handle dependencies and determine execution order.
//!
//! It wraps the generic `cuenv_task_graph` crate with cuenv-core specific
//! types like `TaskNode`, `TaskGroup`, and `Tasks`.

mod build;
mod output_refs;
mod task_graph;

pub use task_graph::{TaskGraph, TaskGraphNode};

#[cfg(test)]
#[path = "graph_advanced_tests.rs"]
mod graph_advanced_tests;
