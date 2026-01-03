//! Traversal algorithms and types for task graphs.
//!
//! This module provides types and utilities for traversing task graphs
//! in various orders.

use crate::GraphNode;

/// A topologically sorted sequence of task nodes.
///
/// This type represents tasks in an order where all dependencies
/// come before the tasks that depend on them.
pub type TopologicalOrder<T> = Vec<GraphNode<T>>;

/// Groups of tasks that can execute in parallel.
///
/// Each inner vector contains tasks that have no dependencies on each other
/// and can safely execute concurrently. The outer vector is ordered by
/// dependency level - all tasks in group N must complete before tasks
/// in group N+1 can start.
pub type ParallelGroups<T> = Vec<Vec<GraphNode<T>>>;
