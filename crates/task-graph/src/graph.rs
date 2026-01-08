//! Task graph builder using petgraph.
//!
//! This module builds directed acyclic graphs (DAGs) from task definitions
//! to handle dependencies and determine execution order.

use crate::{Error, Result, TaskNodeData, TaskResolution, TaskResolver};
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::IntoNodeReferences;
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// A node in the task graph.
#[derive(Debug, Clone)]
pub struct GraphNode<T> {
    /// Name of the task.
    pub name: String,
    /// The task data.
    pub task: T,
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

    /// Add a single task to the graph.
    ///
    /// If a task with the same name already exists, returns the existing node index.
    ///
    /// # Errors
    ///
    /// Currently infallible, but returns `Result` for API consistency.
    pub fn add_task(&mut self, name: &str, task: T) -> Result<NodeIndex> {
        // Check if task already exists
        if let Some(&node) = self.name_to_node.get(name) {
            return Ok(node);
        }

        let node = GraphNode {
            name: name.to_string(),
            task,
        };

        let node_index = self.graph.add_node(node);
        self.name_to_node.insert(name.to_string(), node_index);
        debug!("Added task node '{}'", name);

        Ok(node_index)
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

    /// Build graph for a specific task using a resolver that handles group expansion.
    ///
    /// This method uses the [`TaskResolver`] trait to resolve task names, which enables
    /// unified handling of single tasks and groups (sequential/parallel).
    ///
    /// # Arguments
    ///
    /// * `task_name` - The name of the task to build the graph for
    /// * `resolver` - Implementation of [`TaskResolver`] that provides task lookup and group expansion
    ///
    /// # Errors
    ///
    /// Returns an error if dependencies cannot be resolved.
    pub fn build_for_task_with_resolver<R>(&mut self, task_name: &str, resolver: &R) -> Result<()>
    where
        R: TaskResolver<T>,
    {
        let mut to_process = vec![task_name.to_string()];
        let mut processed = HashSet::new();
        // Track sequential orderings for second pass
        let mut sequential_orderings: Vec<Vec<String>> = Vec::new();
        // Track parallel group depends_on to apply to leaf tasks
        let mut pending_group_deps: HashMap<String, Vec<String>> = HashMap::new();

        debug!("Building graph with resolver for '{}'", task_name);

        // First pass: Collect all tasks and track sequential groups
        while let Some(current_name) = to_process.pop() {
            if processed.contains(&current_name) {
                continue;
            }
            processed.insert(current_name.clone());

            match resolver.resolve(&current_name) {
                Some(TaskResolution::Single(mut task)) => {
                    // Apply any pending group-level dependencies
                    // Walk up the path to find parent groups
                    let path_parts: Vec<&str> = current_name.split('.').collect();
                    for i in 1..path_parts.len() {
                        let parent_path = path_parts[..i].join(".");
                        if let Some(deps) = pending_group_deps.get(&parent_path) {
                            for dep in deps {
                                task.add_dependency(dep.clone());
                            }
                        }
                    }
                    // Also check for bracket notation parents (e.g., "build[0]" -> "build")
                    if let Some(bracket_idx) = current_name.find('[') {
                        let parent_path = &current_name[..bracket_idx];
                        if let Some(deps) = pending_group_deps.get(parent_path) {
                            for dep in deps {
                                task.add_dependency(dep.clone());
                            }
                        }
                    }

                    self.add_task(&current_name, task.clone())?;

                    // Add dependencies to processing queue
                    for dep in task.dependency_names() {
                        if !processed.contains(dep) {
                            to_process.push(dep.to_string());
                        }
                    }
                }
                Some(TaskResolution::Sequential { children }) => {
                    self.register_group(&current_name, children.clone());
                    // Track ordering for second pass
                    sequential_orderings.push(children.clone());
                    for child in children {
                        if !processed.contains(&child) {
                            to_process.push(child);
                        }
                    }
                }
                Some(TaskResolution::Parallel {
                    children,
                    depends_on,
                }) => {
                    self.register_group(&current_name, children.clone());
                    // Store group-level deps to apply to leaf tasks
                    if !depends_on.is_empty() {
                        pending_group_deps.insert(current_name.clone(), depends_on.clone());
                        // Also add the group deps to processing queue
                        for dep in &depends_on {
                            if !processed.contains(dep) {
                                to_process.push(dep.clone());
                            }
                        }
                    }
                    for child in children {
                        if !processed.contains(&child) {
                            to_process.push(child);
                        }
                    }
                }
                None => {
                    debug!("Task '{}' not found while building graph", current_name);
                }
            }
        }

        // Second pass: Add sequential ordering edges
        for ordering in sequential_orderings {
            for window in ordering.windows(2) {
                if let [prev, next] = window {
                    // Add edge from prev to next (prev must complete before next)
                    if let (Some(prev_idx), Some(next_idx)) =
                        (self.get_node_index(prev), self.get_node_index(next))
                    {
                        self.add_edge(prev_idx, next_idx);
                    }
                }
            }
        }

        // Third pass: Add dependency edges from task.depends_on
        self.add_dependency_edges()
    }

    /// Compute which tasks from a pipeline are affected, using transitive dependency propagation.
    ///
    /// This method determines which tasks need to run based on:
    /// 1. Direct effect: The predicate returns true for the task
    /// 2. Transitive effect: A task depends on an affected task
    /// 3. External effect: An external dependency (e.g., `#project:task`) is affected
    ///
    /// # Arguments
    ///
    /// * `pipeline_tasks` - The names of tasks in the pipeline to check
    /// * `is_directly_affected` - Predicate that returns true if a task is directly affected
    /// * `is_external_affected` - Optional predicate for external dependencies (starting with `#`)
    ///
    /// # Returns
    ///
    /// A vector of task names that are affected, in pipeline order.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Without external dependency checking
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.is_affected_by(&changed_files, &project_root),
    ///     None::<fn(&str) -> bool>,
    /// );
    ///
    /// // With external dependency checking (for CI cross-project deps)
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.is_affected_by(&changed_files, &project_root),
    ///     Some(|dep: &str| check_external_dependency(dep, &all_projects, &changed_files)),
    /// );
    /// ```
    #[allow(clippy::needless_pass_by_value)] // Option<E> is intentionally by-value for ergonomic API
    pub fn compute_affected<F, E>(
        &self,
        pipeline_tasks: &[impl AsRef<str>],
        is_directly_affected: F,
        is_external_affected: Option<E>,
    ) -> Vec<String>
    where
        F: Fn(&T) -> bool,
        E: Fn(&str) -> bool,
    {
        use std::collections::HashSet;

        let mut affected = HashSet::new();

        // 1. Find directly affected tasks
        for task_name in pipeline_tasks {
            let task_name = task_name.as_ref();
            if let Some(node) = self.get_node_by_name(task_name)
                && is_directly_affected(&node.task)
            {
                affected.insert(task_name.to_string());
            }
        }

        // 2. Propagate through dependencies (tasks that depend on affected tasks become affected)
        let mut changed = true;
        while changed {
            changed = false;
            for task_name in pipeline_tasks {
                let task_name = task_name.as_ref();
                if affected.contains(task_name) {
                    continue;
                }

                if let Some(node) = self.get_node_by_name(task_name) {
                    for dep in node.task.dependency_names() {
                        // Check if this is an external dependency (starts with #)
                        // Note: Cross-project refs are no longer supported, but keeping check for safety
                        if dep.starts_with('#') {
                            if is_external_affected
                                .as_ref()
                                .is_some_and(|resolver| resolver(dep))
                            {
                                affected.insert(task_name.to_string());
                                changed = true;
                                break;
                            }
                            continue;
                        }

                        // Expand group dependencies to their leaf tasks
                        let leaf_deps = self.expand_dep_to_leaf_tasks(dep);
                        for leaf_dep in leaf_deps {
                            if affected.contains(&leaf_dep) {
                                affected.insert(task_name.to_string());
                                changed = true;
                                break;
                            }
                        }
                        if changed {
                            break;
                        }
                    }
                }
            }
        }

        // Return in pipeline order
        pipeline_tasks
            .iter()
            .map(|t| t.as_ref().to_string())
            .filter(|t| affected.contains(t))
            .collect()
    }
}

impl<T: TaskNodeData> Default for TaskGraph<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the transitive closure of dependencies from an initial set.
///
/// Given a set of starting nodes and a function to retrieve dependencies,
/// returns all nodes reachable by following dependency edges.
///
/// # Arguments
///
/// * `initial` - Starting set of node names
/// * `get_deps` - Function that returns dependencies for a given node name
///
/// # Example
///
/// ```ignore
/// use cuenv_task_graph::compute_transitive_closure;
/// use std::collections::HashMap;
///
/// let deps: HashMap<&str, Vec<String>> = [
///     ("build", vec![]),
///     ("test", vec!["build".to_string()]),
///     ("deploy", vec!["test".to_string()]),
/// ].into_iter().collect();
///
/// let closure = compute_transitive_closure(
///     ["deploy"],
///     |name| deps.get(name).map(|v| v.as_slice()),
/// );
/// // closure contains: {"deploy", "test", "build"}
/// ```
#[must_use]
pub fn compute_transitive_closure<'a>(
    initial: impl IntoIterator<Item = &'a str>,
    get_deps: impl Fn(&str) -> Option<&'a [String]>,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let mut all = HashSet::new();
    let mut frontier: Vec<&str> = Vec::new();

    // Initialize with starting set
    for name in initial {
        if all.insert(name.to_string()) {
            frontier.push(name);
        }
    }

    // BFS through dependencies
    while let Some(task_id) = frontier.pop() {
        if let Some(deps) = get_deps(task_id) {
            for dep in deps {
                if all.insert(dep.clone()) {
                    // Use string slice from the set we just inserted
                    // This is safe because we're doing BFS and won't revisit
                    frontier.push(dep.as_str());
                }
            }
        }
    }

    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple test task implementation
    #[derive(Clone, Debug, Default)]
    struct TestTask {
        depends_on: Vec<String>,
    }

    impl TestTask {
        fn new(deps: &[&str]) -> Self {
            Self {
                depends_on: deps.iter().map(|s| (*s).to_string()).collect(),
            }
        }
    }

    impl TaskNodeData for TestTask {
        fn dependency_names(&self) -> impl Iterator<Item = &str> {
            self.depends_on.iter().map(String::as_str)
        }

        fn add_dependency(&mut self, dep: String) {
            if !self.depends_on.contains(&dep) {
                self.depends_on.push(dep);
            }
        }
    }

    #[test]
    fn test_task_graph_new() {
        let graph: TaskGraph<TestTask> = TaskGraph::new();
        assert_eq!(graph.task_count(), 0);
    }

    #[test]
    fn test_add_single_task() {
        let mut graph = TaskGraph::new();
        let task = TestTask::new(&[]);

        let node = graph.add_task("test", task).unwrap();
        assert!(graph.contains_task("test"));
        assert_eq!(graph.task_count(), 1);

        // Adding same task again should return same node
        let task2 = TestTask::new(&[]);
        let node2 = graph.add_task("test", task2).unwrap();
        assert_eq!(node, node2);
        assert_eq!(graph.task_count(), 1);
    }

    #[test]
    fn test_task_dependencies() {
        let mut graph = TaskGraph::new();

        // Add tasks with dependencies
        let task1 = TestTask::new(&[]);
        let task2 = TestTask::new(&["task1"]);
        let task3 = TestTask::new(&["task1", "task2"]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_dependency_edges().unwrap();

        assert_eq!(graph.task_count(), 3);
        assert!(!graph.has_cycles());

        let sorted = graph.topological_sort().unwrap();
        assert_eq!(sorted.len(), 3);

        // task1 should come before task2 and task3
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        assert!(positions["task1"] < positions["task2"]);
        assert!(positions["task1"] < positions["task3"]);
        assert!(positions["task2"] < positions["task3"]);
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = TaskGraph::new();

        // Create a cycle: task1 -> task2 -> task3 -> task1
        let task1 = TestTask::new(&["task3"]);
        let task2 = TestTask::new(&["task1"]);
        let task3 = TestTask::new(&["task2"]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_dependency_edges().unwrap();

        assert!(graph.has_cycles());
        assert!(graph.topological_sort().is_err());
    }

    #[test]
    fn test_parallel_groups() {
        let mut graph = TaskGraph::new();

        // Create tasks that can run in parallel
        // Level 0: task1, task2 (no dependencies)
        // Level 1: task3 (depends on task1), task4 (depends on task2)
        // Level 2: task5 (depends on task3 and task4)

        let task1 = TestTask::new(&[]);
        let task2 = TestTask::new(&[]);
        let task3 = TestTask::new(&["task1"]);
        let task4 = TestTask::new(&["task2"]);
        let task5 = TestTask::new(&["task3", "task4"]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_task("task4", task4).unwrap();
        graph.add_task("task5", task5).unwrap();
        graph.add_dependency_edges().unwrap();

        let groups = graph.get_parallel_groups().unwrap();

        // Should have 3 levels
        assert_eq!(groups.len(), 3);

        // Level 0 should have 2 tasks
        assert_eq!(groups[0].len(), 2);

        // Level 1 should have 2 tasks
        assert_eq!(groups[1].len(), 2);

        // Level 2 should have 1 task
        assert_eq!(groups[2].len(), 1);
        assert_eq!(groups[2][0].name, "task5");
    }

    #[test]
    fn test_group_dependency_expansion() {
        let mut graph = TaskGraph::new();

        // Register a group "build" with two children
        graph.register_group(
            "build",
            vec!["build.deps".to_string(), "build.compile".to_string()],
        );

        // Add the child tasks
        let deps_task = TestTask::new(&[]);
        let compile_task = TestTask::new(&[]);
        graph.add_task("build.deps", deps_task).unwrap();
        graph.add_task("build.compile", compile_task).unwrap();

        // Add a task that depends on the group name "build"
        let test_task = TestTask::new(&["build"]);
        graph.add_task("test", test_task).unwrap();

        // This should succeed - "build" expands to both children
        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 3);

        // test should come after both build.deps and build.compile
        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        assert!(positions["build.deps"] < positions["test"]);
        assert!(positions["build.compile"] < positions["test"]);
    }

    #[test]
    fn test_missing_dependency() {
        let mut graph = TaskGraph::new();

        // Create task with dependency that doesn't exist
        let task = TestTask::new(&["missing"]);
        graph.add_task("dependent", task).unwrap();

        // Should fail to add edges due to missing dependency
        assert!(graph.add_dependency_edges().is_err());
    }

    #[test]
    fn test_empty_graph() {
        let graph: TaskGraph<TestTask> = TaskGraph::new();

        assert_eq!(graph.task_count(), 0);
        assert!(!graph.has_cycles());

        let groups = graph.get_parallel_groups().unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn test_diamond_dependency() {
        let mut graph = TaskGraph::new();

        // Create a diamond dependency pattern:
        //     A
        //    / \
        //   B   C
        //    \ /
        //     D
        let task_a = TestTask::new(&[]);
        let task_b = TestTask::new(&["a"]);
        let task_c = TestTask::new(&["a"]);
        let task_d = TestTask::new(&["b", "c"]);

        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("d", task_d).unwrap();
        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 4);

        let groups = graph.get_parallel_groups().unwrap();

        // Should have 3 levels: [A], [B,C], [D]
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].len(), 1); // A
        assert_eq!(groups[1].len(), 2); // B and C can run in parallel
        assert_eq!(groups[2].len(), 1); // D
    }

    #[test]
    fn test_self_dependency_cycle() {
        let mut graph = TaskGraph::new();

        // Create self-referencing task
        let task = TestTask::new(&["self_ref"]);
        graph.add_task("self_ref", task).unwrap();
        graph.add_dependency_edges().unwrap();

        assert!(graph.has_cycles());
        assert!(graph.get_parallel_groups().is_err());
    }

    /// Test that shared dependencies appear only once in the DAG.
    ///
    /// When task A and task B both depend on task C, task C should only
    /// appear once in the task graph (deduplication).
    #[test]
    fn test_shared_dependency_deduplication() {
        let mut graph = TaskGraph::new();

        // Create pattern where both A and B depend on C:
        //     C
        //    / \
        //   A   B
        let task_c = TestTask::new(&[]);
        let task_a = TestTask::new(&["c"]);
        let task_b = TestTask::new(&["c"]);

        graph.add_task("c", task_c).unwrap();
        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_dependency_edges().unwrap();

        // Verify task C appears exactly once in the graph
        assert_eq!(graph.task_count(), 3, "Should have exactly 3 tasks");

        // Count occurrences of task C in the topological sort
        let sorted = graph.topological_sort().unwrap();
        let c_count = sorted.iter().filter(|node| node.name == "c").count();
        assert_eq!(c_count, 1, "Task C should appear exactly once in the DAG");

        // Verify execution order: C comes before both A and B
        let positions: std::collections::HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();
        assert!(positions["c"] < positions["a"], "C should execute before A");
        assert!(positions["c"] < positions["b"], "C should execute before B");

        // Verify parallel groups: C in level 0, A and B in level 1
        let groups = graph.get_parallel_groups().unwrap();
        assert_eq!(groups.len(), 2, "Should have 2 execution levels");
        assert_eq!(groups[0].len(), 1, "Level 0 should have 1 task (C)");
        assert_eq!(groups[0][0].name, "c");
        assert_eq!(groups[1].len(), 2, "Level 1 should have 2 tasks (A and B)");
    }

    #[test]
    fn test_build_for_task() {
        let mut graph = TaskGraph::new();

        // Create a map of available tasks
        let mut all_tasks = HashMap::new();
        all_tasks.insert("a".to_string(), TestTask::new(&[]));
        all_tasks.insert("b".to_string(), TestTask::new(&["a"]));
        all_tasks.insert("c".to_string(), TestTask::new(&["b"]));
        all_tasks.insert("d".to_string(), TestTask::new(&[])); // Not a dependency of c

        // Build graph for "c" - should include a, b, c but not d
        graph
            .build_for_task("c", |name| all_tasks.get(name).cloned())
            .unwrap();

        assert_eq!(graph.task_count(), 3);
        assert!(graph.contains_task("a"));
        assert!(graph.contains_task("b"));
        assert!(graph.contains_task("c"));
        assert!(!graph.contains_task("d"));
    }

    // Tests for TaskResolver functionality

    use crate::{TaskResolution, TaskResolver};

    /// Test resolver that supports groups
    struct TestResolver {
        tasks: HashMap<String, TestTask>,
        sequential_groups: HashMap<String, Vec<String>>,
        parallel_groups: HashMap<String, (Vec<String>, Vec<String>)>, // (children, depends_on)
    }

    impl TestResolver {
        fn new() -> Self {
            Self {
                tasks: HashMap::new(),
                sequential_groups: HashMap::new(),
                parallel_groups: HashMap::new(),
            }
        }

        fn add_task(&mut self, name: &str, task: TestTask) {
            self.tasks.insert(name.to_string(), task);
        }

        fn add_sequential_group(&mut self, name: &str, children: &[&str]) {
            self.sequential_groups.insert(
                name.to_string(),
                children.iter().map(|s| (*s).to_string()).collect(),
            );
        }

        fn add_parallel_group(&mut self, name: &str, children: &[&str], depends_on: &[&str]) {
            self.parallel_groups.insert(
                name.to_string(),
                (
                    children.iter().map(|s| (*s).to_string()).collect(),
                    depends_on.iter().map(|s| (*s).to_string()).collect(),
                ),
            );
        }
    }

    impl TaskResolver<TestTask> for TestResolver {
        fn resolve(&self, name: &str) -> Option<TaskResolution<TestTask>> {
            // Check if it's a direct task
            if let Some(task) = self.tasks.get(name) {
                return Some(TaskResolution::Single(task.clone()));
            }
            // Check if it's a sequential group
            if let Some(children) = self.sequential_groups.get(name) {
                return Some(TaskResolution::Sequential {
                    children: children.clone(),
                });
            }
            // Check if it's a parallel group
            if let Some((children, depends_on)) = self.parallel_groups.get(name) {
                return Some(TaskResolution::Parallel {
                    children: children.clone(),
                    depends_on: depends_on.clone(),
                });
            }
            None
        }
    }

    #[test]
    fn test_resolver_single_task() {
        let mut resolver = TestResolver::new();
        resolver.add_task("build", TestTask::new(&[]));
        resolver.add_task("test", TestTask::new(&["build"]));

        let mut graph = TaskGraph::new();
        graph
            .build_for_task_with_resolver("test", &resolver)
            .unwrap();

        assert_eq!(graph.task_count(), 2);
        assert!(graph.contains_task("build"));
        assert!(graph.contains_task("test"));

        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, n)| (n.name.clone(), i))
            .collect();

        assert!(positions["build"] < positions["test"]);
    }

    #[test]
    fn test_resolver_sequential_group() {
        let mut resolver = TestResolver::new();
        // Sequential group: build[0] -> build[1] -> build[2]
        resolver.add_sequential_group("build", &["build[0]", "build[1]", "build[2]"]);
        resolver.add_task("build[0]", TestTask::new(&[]));
        resolver.add_task("build[1]", TestTask::new(&[]));
        resolver.add_task("build[2]", TestTask::new(&[]));

        let mut graph = TaskGraph::new();
        graph
            .build_for_task_with_resolver("build", &resolver)
            .unwrap();

        assert_eq!(graph.task_count(), 3);

        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, n)| (n.name.clone(), i))
            .collect();

        // Sequential ordering must be preserved
        assert!(positions["build[0]"] < positions["build[1]"]);
        assert!(positions["build[1]"] < positions["build[2]"]);
    }

    #[test]
    fn test_resolver_parallel_group() {
        let mut resolver = TestResolver::new();
        // Parallel group with children
        resolver.add_parallel_group(
            "build",
            &["build.frontend", "build.backend"],
            &[], // no group-level deps
        );
        resolver.add_task("build.frontend", TestTask::new(&[]));
        resolver.add_task("build.backend", TestTask::new(&[]));

        let mut graph = TaskGraph::new();
        graph
            .build_for_task_with_resolver("build", &resolver)
            .unwrap();

        assert_eq!(graph.task_count(), 2);
        assert!(graph.contains_task("build.frontend"));
        assert!(graph.contains_task("build.backend"));

        // Both should be at same level (can run in parallel)
        let groups = graph.get_parallel_groups().unwrap();
        assert_eq!(groups.len(), 1); // Single level
        assert_eq!(groups[0].len(), 2); // Both tasks
    }

    #[test]
    fn test_resolver_parallel_group_with_depends_on() {
        let mut resolver = TestResolver::new();
        // Setup task first
        resolver.add_task("setup", TestTask::new(&[]));
        // Parallel group with group-level depends_on
        resolver.add_parallel_group(
            "build",
            &["build.frontend", "build.backend"],
            &["setup"], // group depends on setup
        );
        resolver.add_task("build.frontend", TestTask::new(&[]));
        resolver.add_task("build.backend", TestTask::new(&[]));

        let mut graph = TaskGraph::new();
        graph
            .build_for_task_with_resolver("build", &resolver)
            .unwrap();

        assert_eq!(graph.task_count(), 3);

        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, n)| (n.name.clone(), i))
            .collect();

        // Setup must come before both children
        assert!(positions["setup"] < positions["build.frontend"]);
        assert!(positions["setup"] < positions["build.backend"]);
    }

    #[test]
    fn test_resolver_nested_groups() {
        let mut resolver = TestResolver::new();
        // Top level parallel group
        resolver.add_parallel_group("build", &["build.frontend", "build.backend"], &[]);
        // Nested sequential group
        resolver.add_sequential_group(
            "build.frontend",
            &["build.frontend[0]", "build.frontend[1]"],
        );
        resolver.add_task("build.frontend[0]", TestTask::new(&[]));
        resolver.add_task("build.frontend[1]", TestTask::new(&[]));
        resolver.add_task("build.backend", TestTask::new(&[]));

        let mut graph = TaskGraph::new();
        graph
            .build_for_task_with_resolver("build", &resolver)
            .unwrap();

        assert_eq!(graph.task_count(), 3);

        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, n)| (n.name.clone(), i))
            .collect();

        // Sequential ordering within frontend must be preserved
        assert!(positions["build.frontend[0]"] < positions["build.frontend[1]"]);
    }

    // ==========================================================================
    // compute_affected tests
    // ==========================================================================

    #[test]
    fn test_compute_affected_direct() {
        let mut graph = TaskGraph::new();
        graph.add_task("build", TestTask::new(&[])).unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
        graph.add_dependency_edges().unwrap();

        // Only build is directly affected
        let affected = graph.compute_affected(
            &["build", "test", "deploy"],
            |task| {
                // Simulate: build has no deps (directly affected), others don't
                task.depends_on.is_empty()
            },
            None::<fn(&str) -> bool>,
        );

        // build is directly affected, test and deploy are transitively affected
        assert_eq!(affected, vec!["build", "test", "deploy"]);
    }

    #[test]
    fn test_compute_affected_none() {
        let mut graph = TaskGraph::new();
        graph.add_task("build", TestTask::new(&[])).unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        graph.add_dependency_edges().unwrap();

        // Nothing is directly affected
        let affected =
            graph.compute_affected(&["build", "test"], |_task| false, None::<fn(&str) -> bool>);

        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_preserves_pipeline_order() {
        let mut graph = TaskGraph::new();
        graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        graph.add_task("build", TestTask::new(&[])).unwrap();
        graph.add_dependency_edges().unwrap();

        // All directly affected
        let affected = graph.compute_affected(
            &["build", "test", "deploy"],
            |_| true,
            None::<fn(&str) -> bool>,
        );

        // Should preserve pipeline order, not graph order
        assert_eq!(affected, vec!["build", "test", "deploy"]);
    }

    #[test]
    fn test_compute_affected_transitive_only() {
        let mut graph = TaskGraph::new();
        graph.add_task("build", TestTask::new(&[])).unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
        graph.add_dependency_edges().unwrap();

        // Only test is directly affected, but deploy depends on it
        let affected = graph.compute_affected(
            &["build", "test", "deploy"],
            |task| {
                // Only "test" has exactly one dependency
                task.depends_on.len() == 1 && task.depends_on[0] == "build"
            },
            None::<fn(&str) -> bool>,
        );

        // test is directly affected, deploy is transitively affected
        // build is not affected because nothing depends on what build does
        assert_eq!(affected, vec!["test", "deploy"]);
    }

    #[test]
    fn test_compute_affected_with_external_resolver() {
        let mut graph = TaskGraph::new();
        // build depends on an external project task, test depends on build
        graph
            .add_task("build", TestTask::new(&["#external:lib"]))
            .unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        // Don't call add_dependency_edges() - external deps would fail validation
        // We manually add the internal edge
        let build_idx = *graph.name_to_node.get("build").unwrap();
        let test_idx = *graph.name_to_node.get("test").unwrap();
        graph.add_edge(build_idx, test_idx);

        // External resolver: #external:lib is affected
        let affected = graph.compute_affected(
            &["build", "test"],
            |_task| false, // Nothing directly affected
            Some(|dep: &str| dep == "#external:lib"),
        );

        // build is affected via external dep, test is transitively affected
        assert_eq!(affected, vec!["build", "test"]);
    }

    #[test]
    fn test_compute_affected_external_not_affected() {
        let mut graph = TaskGraph::new();
        graph
            .add_task("build", TestTask::new(&["#external:lib"]))
            .unwrap();
        graph.add_task("test", TestTask::new(&["build"])).unwrap();
        // Don't call add_dependency_edges() - external deps would fail validation
        let build_idx = *graph.name_to_node.get("build").unwrap();
        let test_idx = *graph.name_to_node.get("test").unwrap();
        graph.add_edge(build_idx, test_idx);

        // External resolver: nothing is affected
        let affected =
            graph.compute_affected(&["build", "test"], |_task| false, Some(|_dep: &str| false));

        assert!(affected.is_empty());
    }

    // ==========================================================================
    // compute_transitive_closure tests
    // ==========================================================================

    #[test]
    fn test_transitive_closure_empty() {
        let deps: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
        let closure = compute_transitive_closure(std::iter::empty::<&str>(), |name| {
            deps.get(name).map(|v| v.as_slice())
        });
        assert!(closure.is_empty());
    }

    #[test]
    fn test_transitive_closure_single_node_no_deps() {
        let deps: std::collections::HashMap<&str, Vec<String>> =
            [("build", vec![])].into_iter().collect();
        let closure =
            compute_transitive_closure(["build"], |name| deps.get(name).map(|v| v.as_slice()));
        assert_eq!(closure.len(), 1);
        assert!(closure.contains("build"));
    }

    #[test]
    fn test_transitive_closure_chain() {
        // deploy -> test -> build
        let deps: std::collections::HashMap<&str, Vec<String>> = [
            ("build", vec![]),
            ("test", vec!["build".to_string()]),
            ("deploy", vec!["test".to_string()]),
        ]
        .into_iter()
        .collect();

        let closure =
            compute_transitive_closure(["deploy"], |name| deps.get(name).map(|v| v.as_slice()));

        assert_eq!(closure.len(), 3);
        assert!(closure.contains("deploy"));
        assert!(closure.contains("test"));
        assert!(closure.contains("build"));
    }

    #[test]
    fn test_transitive_closure_diamond() {
        //      A
        //     / \
        //    B   C
        //     \ /
        //      D
        let deps: std::collections::HashMap<&str, Vec<String>> = [
            ("D", vec![]),
            ("B", vec!["D".to_string()]),
            ("C", vec!["D".to_string()]),
            ("A", vec!["B".to_string(), "C".to_string()]),
        ]
        .into_iter()
        .collect();

        let closure =
            compute_transitive_closure(["A"], |name| deps.get(name).map(|v| v.as_slice()));

        assert_eq!(closure.len(), 4);
        assert!(closure.contains("A"));
        assert!(closure.contains("B"));
        assert!(closure.contains("C"));
        assert!(closure.contains("D"));
    }

    #[test]
    fn test_transitive_closure_multiple_initial() {
        // Two separate chains: A -> B, C -> D
        let deps: std::collections::HashMap<&str, Vec<String>> = [
            ("B", vec![]),
            ("A", vec!["B".to_string()]),
            ("D", vec![]),
            ("C", vec!["D".to_string()]),
        ]
        .into_iter()
        .collect();

        let closure =
            compute_transitive_closure(["A", "C"], |name| deps.get(name).map(|v| v.as_slice()));

        assert_eq!(closure.len(), 4);
        assert!(closure.contains("A"));
        assert!(closure.contains("B"));
        assert!(closure.contains("C"));
        assert!(closure.contains("D"));
    }

    #[test]
    fn test_transitive_closure_missing_dep() {
        // A depends on nonexistent B - should just include A
        let deps: std::collections::HashMap<&str, Vec<String>> =
            [("A", vec!["B".to_string()])].into_iter().collect();

        let closure =
            compute_transitive_closure(["A"], |name| deps.get(name).map(|v| v.as_slice()));

        // A is included, B is added to closure even though it has no entry (it's a valid node name)
        assert_eq!(closure.len(), 2);
        assert!(closure.contains("A"));
        assert!(closure.contains("B"));
    }
}
