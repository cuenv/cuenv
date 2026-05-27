//! Task graph builder using petgraph.
//!
//! This module builds directed acyclic graphs (DAGs) from task definitions
//! to handle dependencies and determine execution order.

use crate::{Error, Result, TaskNodeData};
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::IntoNodeReferences;
use std::collections::{HashMap, HashSet};
use tracing::debug;

mod analysis;
mod resolver_build;

pub use analysis::compute_transitive_closure;

/// Discriminator for the kind of node in a mixed task/service graph.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash)]
pub enum NodeKind {
    /// A one-shot task that runs to completion.
    #[default]
    Task,
    /// A long-running service supervised by `cuenv up`.
    Service,
    /// A container image build managed by `cuenv build`.
    Image,
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Task => write!(f, "task"),
            Self::Service => write!(f, "service"),
            Self::Image => write!(f, "image"),
        }
    }
}

/// A node in the task graph.
#[derive(Debug, Clone)]
pub struct GraphNode<T> {
    /// Name of the task.
    pub name: String,
    /// The task data.
    pub task: T,
    /// Whether this node is a task or a service.
    pub kind: NodeKind,
}

/// Task graph for dependency resolution and execution ordering.
///
/// This is a generic graph that can hold any task type implementing [`TaskNodeData`].
/// It provides methods for building the graph, resolving dependencies, and
/// computing execution order.
pub struct TaskGraph<T: TaskNodeData> {
    /// The directed graph of tasks.
    graph: DiGraph<GraphNode<T>, ()>,
    /// Map from task names to node indices.
    name_to_node: HashMap<String, NodeIndex>,
    /// Map from group prefix to child task names (for dependency expansion).
    group_children: HashMap<String, Vec<String>>,
}

impl<T: TaskNodeData> TaskGraph<T> {
    /// Create a new empty task graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            name_to_node: HashMap::new(),
            group_children: HashMap::new(),
        }
    }

    /// Add a node to the graph with the given kind.
    ///
    /// If a node with the same name and kind already exists, returns the
    /// existing node index. If a node with the same name but a *different*
    /// kind exists, returns a [`DuplicateNodeName`](Error::DuplicateNodeName)
    /// error.
    fn add_node_with_kind(&mut self, name: &str, task: T, kind: NodeKind) -> Result<NodeIndex> {
        if let Some(&existing) = self.name_to_node.get(name) {
            let existing_kind = self.graph[existing].kind;
            if existing_kind != kind {
                return Err(Error::DuplicateNodeName {
                    name: name.to_string(),
                    existing_kind: existing_kind.to_string(),
                    new_kind: kind.to_string(),
                });
            }
            return Ok(existing);
        }

        let node = GraphNode {
            name: name.to_string(),
            task,
            kind,
        };

        let node_index = self.graph.add_node(node);
        self.name_to_node.insert(name.to_string(), node_index);
        debug!("Added {kind} node '{name}'");

        Ok(node_index)
    }

    /// Add a single task to the graph.
    ///
    /// # Errors
    ///
    /// Returns an error if a node with the same name but different kind exists.
    pub fn add_task(&mut self, name: &str, task: T) -> Result<NodeIndex> {
        self.add_node_with_kind(name, task, NodeKind::Task)
    }

    /// Add a single service node to the graph.
    ///
    /// # Errors
    ///
    /// Returns an error if a node with the same name but different kind exists.
    pub fn add_service(&mut self, name: &str, task: T) -> Result<NodeIndex> {
        self.add_node_with_kind(name, task, NodeKind::Service)
    }

    /// Add a single image node to the graph.
    ///
    /// # Errors
    ///
    /// Returns an error if a node with the same name but different kind exists.
    pub fn add_image(&mut self, name: &str, task: T) -> Result<NodeIndex> {
        self.add_node_with_kind(name, task, NodeKind::Image)
    }

    /// Get a mutable reference to a task node by index.
    pub fn get_node_mut(&mut self, index: NodeIndex) -> Option<&mut GraphNode<T>> {
        self.graph.node_weight_mut(index)
    }

    /// Get a reference to a task node by name.
    #[must_use]
    pub fn get_node_by_name(&self, name: &str) -> Option<&GraphNode<T>> {
        self.name_to_node
            .get(name)
            .and_then(|&idx| self.graph.node_weight(idx))
    }

    /// Register a group of child task names under a group prefix.
    ///
    /// This enables group-level dependency expansion where depending on
    /// a group name will expand to depend on all child tasks.
    pub fn register_group(&mut self, prefix: &str, children: Vec<String>) {
        if !children.is_empty() {
            self.group_children.insert(prefix.to_string(), children);
        }
    }

    /// Expand a dependency name to leaf task names.
    ///
    /// If the dependency is a direct task, returns it as-is.
    /// If it's a group name, recursively expands to all leaf tasks in that group.
    fn expand_dep_to_leaf_tasks(&self, dep_name: &str) -> Vec<String> {
        if self.name_to_node.contains_key(dep_name) {
            // It's a leaf task (exists directly in the graph)
            vec![dep_name.to_string()]
        } else if let Some(children) = self.group_children.get(dep_name) {
            // It's a group - recursively expand children
            children
                .iter()
                .flat_map(|child| self.expand_dep_to_leaf_tasks(child))
                .collect()
        } else {
            // Not found - will be caught as missing dependency later
            vec![dep_name.to_string()]
        }
    }

    /// Add dependency edges after all tasks have been added.
    ///
    /// This ensures proper cycle detection and missing dependency validation.
    ///
    /// # Errors
    ///
    /// Returns an error if any task depends on a non-existent task.
    pub fn add_dependency_edges(&mut self) -> Result<()> {
        let mut missing_deps = Vec::new();
        let mut edges_to_add = Vec::new();

        // Collect all dependency relationships
        for (node_index, node) in self.graph.node_references() {
            for dep_name in node.task.dependency_names() {
                // Expand group references to leaf tasks
                let expanded_deps = self.expand_dep_to_leaf_tasks(dep_name);

                for expanded_dep in expanded_deps {
                    if let Some(&dep_node_index) = self.name_to_node.get(&expanded_dep) {
                        // Record edge to add later
                        edges_to_add.push((dep_node_index, node_index));
                    } else {
                        missing_deps.push((node.name.clone(), expanded_dep));
                    }
                }
            }
        }

        // Report missing dependencies
        if !missing_deps.is_empty() {
            return Err(Error::MissingDependencies {
                missing: missing_deps,
            });
        }

        // Add all edges
        for (from, to) in edges_to_add {
            self.graph.add_edge(from, to, ());
        }

        Ok(())
    }

    /// Add a direct edge between two tasks.
    ///
    /// This is a low-level method for adding edges directly, typically used
    /// for sequential group ordering.
    pub fn add_edge(&mut self, from: NodeIndex, to: NodeIndex) {
        self.graph.add_edge(from, to, ());
    }

    /// Check if the graph has cycles.
    #[must_use]
    pub fn has_cycles(&self) -> bool {
        is_cyclic_directed(&self.graph)
    }

    /// Get topologically sorted list of tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph contains cycles.
    pub fn topological_sort(&self) -> Result<Vec<GraphNode<T>>> {
        if self.has_cycles() {
            return Err(Error::CycleDetected {
                message: "Task dependency graph contains cycles".to_string(),
            });
        }

        match toposort(&self.graph, None) {
            Ok(sorted_indices) => Ok(sorted_indices
                .into_iter()
                .map(|idx| self.graph[idx].clone())
                .collect()),
            Err(_) => Err(Error::TopologicalSortFailed {
                reason: "petgraph toposort failed".to_string(),
            }),
        }
    }

    /// Get all tasks that can run in parallel (no dependencies between them).
    ///
    /// Returns a vector of parallel groups, where each group contains tasks
    /// that can execute concurrently. Groups are ordered by dependency level.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph contains cycles.
    pub fn get_parallel_groups(&self) -> Result<Vec<Vec<GraphNode<T>>>> {
        let sorted = self.topological_sort()?;

        if sorted.is_empty() {
            return Ok(vec![]);
        }

        // Group tasks by their dependency level
        let mut groups: Vec<Vec<GraphNode<T>>> = vec![];
        let mut processed: HashMap<String, usize> = HashMap::new();

        for task in sorted {
            // Find the maximum level of all dependencies
            let mut level = 0;
            for dep in task.task.dependency_names() {
                if let Some(&dep_level) = processed.get(dep) {
                    level = level.max(dep_level + 1);
                }
            }

            // Add to appropriate group
            if level >= groups.len() {
                groups.resize(level + 1, vec![]);
            }
            groups[level].push(task.clone());
            processed.insert(task.name.clone(), level);
        }

        Ok(groups)
    }

    /// Get the number of tasks in the graph.
    #[must_use]
    pub fn task_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Check if a task exists in the graph.
    #[must_use]
    pub fn contains_task(&self, name: &str) -> bool {
        self.name_to_node.contains_key(name)
    }

    /// Get the node index for a task by name.
    #[must_use]
    pub fn get_node_index(&self, name: &str) -> Option<NodeIndex> {
        self.name_to_node.get(name).copied()
    }

    /// Get a mutable reference to a task's data by name.
    pub fn get_task_mut(&mut self, name: &str) -> Option<&mut T> {
        let idx = self.name_to_node.get(name).copied()?;
        self.graph.node_weight_mut(idx).map(|node| &mut node.task)
    }

    /// Iterate over all nodes in the graph.
    pub fn iter_nodes(&self) -> impl Iterator<Item = (NodeIndex, &GraphNode<T>)> {
        self.graph.node_references()
    }

    /// Build graph for a specific task and all its transitive dependencies.
    ///
    /// This method takes an iterator of all available tasks and builds
    /// only the subgraph needed for the requested task.
    ///
    /// # Arguments
    ///
    /// * `task_name` - The name of the task to build the graph for
    /// * `get_task` - Function that returns the task data for a given name
    ///
    /// # Errors
    ///
    /// Returns an error if dependencies cannot be resolved.
    pub fn build_for_task<F>(&mut self, task_name: &str, mut get_task: F) -> Result<()>
    where
        F: FnMut(&str) -> Option<T>,
    {
        let mut to_process = vec![task_name.to_string()];
        let mut processed = HashSet::new();

        debug!("Building graph for '{}'", task_name);

        // First pass: Collect all tasks that need to be included
        while let Some(current_name) = to_process.pop() {
            if processed.contains(&current_name) {
                continue;
            }
            processed.insert(current_name.clone());

            if let Some(task) = get_task(&current_name) {
                // Collect dependencies before adding the task
                let deps: Vec<String> = task.dependency_names().map(String::from).collect();

                self.add_task(&current_name, task)?;

                // Add dependencies to processing queue
                for dep in deps {
                    if !processed.contains(&dep) {
                        to_process.push(dep);
                    }
                }
            } else {
                debug!("Task '{}' not found while building graph", current_name);
            }
        }

        // Second pass: Add dependency edges
        self.add_dependency_edges()?;

        Ok(())
    }
}

impl<T: TaskNodeData> Default for TaskGraph<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod graph_tests;
