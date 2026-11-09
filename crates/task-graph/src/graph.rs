//! Task graph builder using petgraph.

mod analysis;
mod resolver_build;
mod task_graph;

pub use analysis::compute_transitive_closure;
pub use task_graph::{GraphNode, NodeKind, TaskGraph};

#[cfg(test)]
mod graph_tests;
