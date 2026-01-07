//! Task graph builder using cuenv-task-graph.
//!
//! This module builds directed acyclic graphs (DAGs) from task definitions
//! to handle dependencies and determine execution order.
//!
//! It wraps the generic `cuenv_task_graph` crate with cuenv-core specific
//! types like `TaskNode`, `TaskGroup`, and `TaskList`.

use super::{Task, TaskDependency, TaskGroup, TaskList, TaskNode, Tasks};
use crate::Result;
use cuenv_task_graph::{GraphNode, TaskNodeData, TaskResolution, TaskResolver};
use petgraph::graph::NodeIndex;
use tracing::debug;

// ============================================================================
// TaskResolver Implementation for Tasks
// ============================================================================

impl TaskResolver<Task> for Tasks {
    fn resolve(&self, name: &str) -> Option<TaskResolution<Task>> {
        // Parse the path and walk down to find the TaskNode
        let node = self.resolve_path(name)?;
        Some(self.node_to_resolution(name, node))
    }
}

impl Tasks {
    /// Walk a dotted/bracketed path to find the TaskNode.
    ///
    /// This method first tries a direct lookup (for flat task names like `bun.setup`),
    /// then falls back to walking the nested path structure.
    ///
    /// Examples:
    /// - `"build"` → top-level lookup
    /// - `"build.frontend"` → first tries `tasks["build.frontend"]`, then `tasks["build"].parallel["frontend"]`
    /// - `"build[0]"` → first tries `tasks["build[0]"]`, then `tasks["build"].steps[0]`
    /// - `"build.frontend[0]"` → nested: parallel then sequential
    fn resolve_path(&self, path: &str) -> Option<&TaskNode> {
        // First: try direct lookup (handles flat task names like "bun.setup", "bun.hooks.beforeInstall[0]")
        if let Some(task) = self.tasks.get(path) {
            return Some(task);
        }

        // Second: try walking nested structure
        let segments = parse_path_segments(path);
        if segments.is_empty() {
            return None;
        }

        // Get the root definition
        let root_name = &segments[0];
        let root_segment = match root_name {
            PathSegment::Name(n) => n.as_str(),
            PathSegment::Index(_) => return None, // Can't start with index
        };

        let mut current = self.tasks.get(root_segment)?;

        // Walk remaining segments
        for segment in &segments[1..] {
            current = match (current, segment) {
                // Parallel group child access (dot notation)
                (TaskNode::Group(group), PathSegment::Name(name)) => group.parallel.get(name)?,
                // Sequential list child access (bracket notation)
                (TaskNode::List(list), PathSegment::Index(idx)) => list.steps.get(*idx)?,
                // Invalid access pattern
                _ => return None,
            };
        }

        Some(current)
    }

    /// Convert a TaskNode to a TaskResolution.
    fn node_to_resolution(&self, name: &str, node: &TaskNode) -> TaskResolution<Task> {
        match node {
            TaskNode::Task(task) => TaskResolution::Single(task.as_ref().clone()),
            TaskNode::List(list) => {
                let children: Vec<String> = (0..list.steps.len())
                    .map(|i| format!("{}[{}]", name, i))
                    .collect();
                TaskResolution::Sequential { children }
            }
            TaskNode::Group(group) => {
                let children: Vec<String> = group
                    .parallel
                    .keys()
                    .map(|k| format!("{}.{}", name, k))
                    .collect();
                TaskResolution::Parallel {
                    children,
                    depends_on: group
                        .depends_on
                        .iter()
                        .map(|d| d.task_name().to_string())
                        .collect(),
                }
            }
        }
    }
}

/// Segment of a task path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    /// Named segment (dot notation): `frontend` in `build.frontend`
    Name(String),
    /// Indexed segment (bracket notation): `0` in `build[0]`
    Index(usize),
}

/// Parse a task path into segments.
///
/// Examples:
/// - `"build"` → `[Name("build")]`
/// - `"build.frontend"` → `[Name("build"), Name("frontend")]`
/// - `"build[0]"` → `[Name("build"), Index(0)]`
/// - `"build.frontend[0]"` → `[Name("build"), Name("frontend"), Index(0)]`
fn parse_path_segments(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut current_name = String::new();
    let mut chars = path.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !current_name.is_empty() {
                    segments.push(PathSegment::Name(current_name.clone()));
                    current_name.clear();
                }
            }
            '[' => {
                if !current_name.is_empty() {
                    segments.push(PathSegment::Name(current_name.clone()));
                    current_name.clear();
                }
                // Parse index
                let mut index_str = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    index_str.push(c);
                }
                if let Ok(idx) = index_str.parse::<usize>() {
                    segments.push(PathSegment::Index(idx));
                }
            }
            _ => {
                current_name.push(c);
            }
        }
    }

    // Push final name if any
    if !current_name.is_empty() {
        segments.push(PathSegment::Name(current_name));
    }

    segments
}

// Implement the TaskNodeData trait for Task
impl TaskNodeData for Task {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.depends_on.iter().map(|d| d.task_name())
    }

    fn add_dependency(&mut self, dep: String) {
        if !self.has_dependency(&dep) {
            self.depends_on.push(TaskDependency::from_name(dep));
        }
    }
}

/// A node in the task graph containing a task name and the task itself.
pub type TaskGraphNode = GraphNode<Task>;

/// Task graph for dependency resolution and execution ordering.
///
/// This wraps `cuenv_task_graph::TaskGraph` with cuenv-core specific
/// functionality for building graphs from `TaskDefinition`, `TaskGroup`, etc.
pub struct TaskGraph {
    /// The underlying generic task graph.
    inner: cuenv_task_graph::TaskGraph<Task>,
}

impl TaskGraph {
    /// Create a new empty task graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: cuenv_task_graph::TaskGraph::new(),
        }
    }

    /// Build a graph from a task node.
    pub fn build_from_node(
        &mut self,
        name: &str,
        node: &TaskNode,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        match node {
            TaskNode::Task(task) => {
                let idx = self.add_task(name, task.as_ref().clone())?;
                Ok(vec![idx])
            }
            TaskNode::Group(group) => self.build_parallel_group(name, group, all_tasks),
            TaskNode::List(list) => self.build_sequential_list(name, list, all_tasks),
        }
    }

    /// Build a sequential task list (steps run one after another).
    fn build_sequential_list(
        &mut self,
        prefix: &str,
        list: &TaskList,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        let mut nodes = Vec::new();
        let mut previous: Option<NodeIndex> = None;

        // Track child names for group dependency expansion
        let child_names: Vec<String> = (0..list.steps.len())
            .map(|i| format!("{}[{}]", prefix, i))
            .collect();

        for (i, step) in list.steps.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_nodes = self.build_from_node(&task_name, step, all_tasks)?;

            // For sequential execution, link previous task to current
            if let Some(prev) = previous
                && let Some(first) = task_nodes.first()
            {
                self.inner.add_edge(prev, *first);
            }

            if let Some(last) = task_nodes.last() {
                previous = Some(*last);
            }

            nodes.extend(task_nodes);
        }

        // Register this group for dependency expansion
        self.inner.register_group(prefix, child_names);

        Ok(nodes)
    }

    /// Build a parallel task group (tasks can run concurrently).
    fn build_parallel_group(
        &mut self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        let mut nodes = Vec::new();

        // Track child names for group dependency expansion
        let child_names: Vec<String> = group
            .parallel
            .keys()
            .map(|name| format!("{}.{}", prefix, name))
            .collect();

        for (name, child_node) in &group.parallel {
            let task_name = format!("{}.{}", prefix, name);
            let task_nodes = self.build_from_node(&task_name, child_node, all_tasks)?;

            // Apply group-level dependencies to each subtask
            if !group.depends_on.is_empty() {
                for node_idx in &task_nodes {
                    if let Some(node) = self.inner.get_node_mut(*node_idx) {
                        for dep in &group.depends_on {
                            node.task.add_dependency(dep.task_name().to_string());
                        }
                    }
                }
            }

            nodes.extend(task_nodes);
        }

        // Register this group for dependency expansion
        self.inner.register_group(prefix, child_names);

        Ok(nodes)
    }

    /// Add a single task to the graph.
    pub fn add_task(&mut self, name: &str, task: Task) -> Result<NodeIndex> {
        self.inner
            .add_task(name, task)
            .map_err(|e| crate::Error::configuration(e.to_string()))
    }

    /// Add dependency edges after all tasks have been added.
    /// This ensures proper cycle detection and missing dependency validation.
    pub fn add_dependency_edges(&mut self) -> Result<()> {
        self.inner
            .add_dependency_edges()
            .map_err(|e| crate::Error::configuration(e.to_string()))
    }

    /// Check if the graph has cycles.
    #[must_use]
    pub fn has_cycles(&self) -> bool {
        self.inner.has_cycles()
    }

    /// Get topologically sorted list of tasks.
    pub fn topological_sort(&self) -> Result<Vec<TaskGraphNode>> {
        self.inner
            .topological_sort()
            .map_err(|e| crate::Error::configuration(e.to_string()))
    }

    /// Get all tasks that can run in parallel (no dependencies between them).
    pub fn get_parallel_groups(&self) -> Result<Vec<Vec<TaskGraphNode>>> {
        self.inner
            .get_parallel_groups()
            .map_err(|e| crate::Error::configuration(e.to_string()))
    }

    /// Get the number of tasks in the graph.
    #[must_use]
    pub fn task_count(&self) -> usize {
        self.inner.task_count()
    }

    /// Check if a task exists in the graph.
    #[must_use]
    pub fn contains_task(&self, name: &str) -> bool {
        self.inner.contains_task(name)
    }

    /// Build a complete graph from tasks with proper dependency resolution.
    /// This performs a two-pass build: first adding all nodes, then all edges.
    pub fn build_complete_graph(&mut self, tasks: &Tasks) -> Result<()> {
        // First pass: Add all tasks as nodes
        for (name, node) in tasks.tasks.iter() {
            if let TaskNode::Task(task) = node {
                self.add_task(name, task.as_ref().clone())?;
            }
            // Groups and Lists are handled by build_from_node
        }

        // Second pass: Add all dependency edges
        self.add_dependency_edges()
    }

    /// Build graph for a specific task and all its transitive dependencies.
    ///
    /// This uses the [`TaskResolver`] trait implementation for [`Tasks`] to handle
    /// nested paths and group expansion in a unified way.
    pub fn build_for_task(&mut self, task_name: &str, all_tasks: &Tasks) -> Result<()> {
        debug!(
            "Building graph for '{}' with tasks {:?}",
            task_name,
            all_tasks.list_tasks()
        );

        self.inner
            .build_for_task_with_resolver(task_name, all_tasks)
            .map_err(|e| crate::Error::configuration(e.to_string()))
    }
}

impl Default for TaskGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "graph_advanced_tests.rs"]
mod graph_advanced_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskDependency, TaskGroup, TaskList, TaskNode};
    use crate::test_utils::create_task;
    use std::collections::HashMap;

    #[test]
    fn test_task_graph_new() {
        let graph = TaskGraph::new();
        assert_eq!(graph.task_count(), 0);
    }

    #[test]
    fn test_add_single_task() {
        let mut graph = TaskGraph::new();
        let task = create_task("test", vec![], vec![]);

        let node = graph.add_task("test", task).unwrap();
        assert!(graph.contains_task("test"));
        assert_eq!(graph.task_count(), 1);

        // Adding same task again should return same node
        let task2 = create_task("test", vec![], vec![]);
        let node2 = graph.add_task("test", task2).unwrap();
        assert_eq!(node, node2);
        assert_eq!(graph.task_count(), 1);
    }

    #[test]
    fn test_task_dependencies() {
        let mut graph = TaskGraph::new();

        // Add tasks with dependencies
        let task1 = create_task("task1", vec![], vec![]);
        let task2 = create_task("task2", vec!["task1"], vec![]);
        let task3 = create_task("task3", vec!["task1", "task2"], vec![]);

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
        let task1 = create_task("task1", vec!["task3"], vec![]);
        let task2 = create_task("task2", vec!["task1"], vec![]);
        let task3 = create_task("task3", vec!["task2"], vec![]);

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

        let task1 = create_task("task1", vec![], vec![]);
        let task2 = create_task("task2", vec![], vec![]);
        let task3 = create_task("task3", vec!["task1"], vec![]);
        let task4 = create_task("task4", vec!["task2"], vec![]);
        let task5 = create_task("task5", vec!["task3", "task4"], vec![]);

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
    fn test_build_from_sequential_group() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        let task1 = create_task("t1", vec![], vec![]);
        let task2 = create_task("t2", vec![], vec![]);

        let list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        let node = TaskNode::List(list);
        let nodes = graph.build_from_node("seq", &node, &tasks).unwrap();
        assert_eq!(nodes.len(), 2);

        // Sequential tasks should have dependency chain
        let sorted = graph.topological_sort().unwrap();
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].name, "seq[0]");
        assert_eq!(sorted[1].name, "seq[1]");
    }

    #[test]
    fn test_build_from_parallel_group() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        let task1 = create_task("t1", vec![], vec![]);
        let task2 = create_task("t2", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("first".to_string(), TaskNode::Task(Box::new(task1)));
        parallel_tasks.insert("second".to_string(), TaskNode::Task(Box::new(task2)));

        let group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let node = TaskNode::Group(group);
        let nodes = graph.build_from_node("par", &node, &tasks).unwrap();
        assert_eq!(nodes.len(), 2);

        // Parallel tasks should not have dependencies between them
        assert!(!graph.has_cycles());

        let groups = graph.get_parallel_groups().unwrap();
        assert_eq!(groups.len(), 1); // All in same level
        assert_eq!(groups[0].len(), 2); // Both can run in parallel
    }

    #[test]
    fn test_three_way_cycle_detection() {
        let mut graph = TaskGraph::new();

        // Create cyclic dependencies: A -> B -> C -> A
        let task_a = create_task("task_a", vec!["task_c"], vec![]);
        let task_b = create_task("task_b", vec!["task_a"], vec![]);
        let task_c = create_task("task_c", vec!["task_b"], vec![]);

        graph.add_task("task_a", task_a).unwrap();
        graph.add_task("task_b", task_b).unwrap();
        graph.add_task("task_c", task_c).unwrap();
        graph.add_dependency_edges().unwrap();

        // This should create a cycle
        assert!(graph.has_cycles());

        // Should fail when trying to get parallel groups
        assert!(graph.get_parallel_groups().is_err());
    }

    #[test]
    fn test_self_dependency_cycle() {
        let mut graph = TaskGraph::new();

        // Create self-referencing task
        let task = create_task("self_ref", vec!["self_ref"], vec![]);
        graph.add_task("self_ref", task).unwrap();
        graph.add_dependency_edges().unwrap();

        assert!(graph.has_cycles());
        assert!(graph.get_parallel_groups().is_err());
    }

    #[test]
    fn test_complex_dependency_graph() {
        let mut graph = TaskGraph::new();

        // Create a diamond dependency pattern:
        //     A
        //    / \
        //   B   C
        //    \ /
        //     D
        let task_a = create_task("a", vec![], vec![]);
        let task_b = create_task("b", vec!["a"], vec![]);
        let task_c = create_task("c", vec!["a"], vec![]);
        let task_d = create_task("d", vec!["b", "c"], vec![]);

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
    fn test_missing_dependency() {
        let mut graph = TaskGraph::new();

        // Create task with dependency that doesn't exist
        let task = create_task("dependent", vec!["missing"], vec![]);
        graph.add_task("dependent", task).unwrap();

        // Should fail to get parallel groups due to missing dependency
        assert!(graph.add_dependency_edges().is_err());
    }

    #[test]
    fn test_empty_graph() {
        let graph = TaskGraph::new();

        assert_eq!(graph.task_count(), 0);
        assert!(!graph.has_cycles());

        let groups = graph.get_parallel_groups().unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn test_single_task_no_deps() {
        let mut graph = TaskGraph::new();

        let task = create_task("solo", vec![], vec![]);
        graph.add_task("solo", task).unwrap();

        assert_eq!(graph.task_count(), 1);
        assert!(!graph.has_cycles());

        let groups = graph.get_parallel_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 1);
    }

    #[test]
    fn test_linear_chain() {
        let mut graph = TaskGraph::new();

        // Create linear chain: A -> B -> C -> D
        let task_a = create_task("a", vec![], vec![]);
        let task_b = create_task("b", vec!["a"], vec![]);
        let task_c = create_task("c", vec!["b"], vec![]);
        let task_d = create_task("d", vec!["c"], vec![]);

        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("d", task_d).unwrap();
        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 4);

        let groups = graph.get_parallel_groups().unwrap();

        // Should be 4 sequential groups
        assert_eq!(groups.len(), 4);
        for group in &groups {
            assert_eq!(group.len(), 1);
        }
    }

    #[test]
    fn test_group_as_dependency_parallel() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        // Create a parallel group "build" with two children
        let deps_task = create_task("deps", vec![], vec![]);
        let compile_task = create_task("compile", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("deps".to_string(), TaskNode::Task(Box::new(deps_task)));
        parallel_tasks.insert(
            "compile".to_string(),
            TaskNode::Task(Box::new(compile_task)),
        );

        let build_group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        // Build the group first
        let build_node = TaskNode::Group(build_group);
        graph.build_from_node("build", &build_node, &tasks).unwrap();

        // Add a task that depends on the group name "build"
        let test_task = create_task("test", vec!["build"], vec![]);
        graph.add_task("test", test_task).unwrap();

        // This should succeed - "build" should expand to ["build.deps", "build.compile"]
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
    fn test_group_as_dependency_sequential() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        // Create a sequential group "setup" with two children
        let task1 = create_task("s1", vec![], vec![]);
        let task2 = create_task("s2", vec![], vec![]);

        let setup_list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        // Build the list first
        let setup_node = TaskNode::List(setup_list);
        graph.build_from_node("setup", &setup_node, &tasks).unwrap();

        // Add a task that depends on the group name "setup"
        let run_task = create_task("run", vec!["setup"], vec![]);
        graph.add_task("run", run_task).unwrap();

        // This should succeed - "setup" should expand to ["setup[0]", "setup[1]"]
        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 3);

        // run should come after both setup[0] and setup[1]
        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        assert!(positions["setup[0]"] < positions["run"]);
        assert!(positions["setup[1]"] < positions["run"]);
    }

    #[test]
    fn test_nested_group_as_dependency() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        // Create a nested structure:
        // build (parallel)
        //   ├── frontend (sequential)
        //   │   ├── frontend[0]
        //   │   └── frontend[1]
        //   └── backend (single task)

        let frontend_t1 = create_task("fe1", vec![], vec![]);
        let frontend_t2 = create_task("fe2", vec![], vec![]);
        let frontend_list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(frontend_t1)),
                TaskNode::Task(Box::new(frontend_t2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        let backend_task = create_task("be", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("frontend".to_string(), TaskNode::List(frontend_list));
        parallel_tasks.insert(
            "backend".to_string(),
            TaskNode::Task(Box::new(backend_task)),
        );

        let build_group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        // Build the nested group
        let build_node = TaskNode::Group(build_group);
        graph.build_from_node("build", &build_node, &tasks).unwrap();

        // Add a task that depends on "build"
        let deploy_task = create_task("deploy", vec!["build"], vec![]);
        graph.add_task("deploy", deploy_task).unwrap();

        // This should expand "build" -> ["build.frontend", "build.backend"]
        // And further expand "build.frontend" -> ["build.frontend[0]", "build.frontend[1]"]
        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 4); // frontend[0], frontend[1], backend, deploy

        // deploy should come after all leaf tasks
        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        assert!(positions["build.frontend[0]"] < positions["deploy"]);
        assert!(positions["build.frontend[1]"] < positions["deploy"]);
        assert!(positions["build.backend"] < positions["deploy"]);
    }

    #[test]
    fn test_mixed_exact_and_group_dependencies() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        // Add a standalone task
        let lint_task = create_task("lint", vec![], vec![]);
        graph.add_task("lint", lint_task).unwrap();

        // Create a parallel group
        let deps_task = create_task("deps", vec![], vec![]);
        let compile_task = create_task("compile", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("deps".to_string(), TaskNode::Task(Box::new(deps_task)));
        parallel_tasks.insert(
            "compile".to_string(),
            TaskNode::Task(Box::new(compile_task)),
        );

        let build_group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let build_node = TaskNode::Group(build_group);
        graph.build_from_node("build", &build_node, &tasks).unwrap();

        // Add a task that depends on both an exact task and a group
        let test_task = create_task("test", vec!["lint", "build"], vec![]);
        graph.add_task("test", test_task).unwrap();

        graph.add_dependency_edges().unwrap();

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 4);

        // test should come after lint, build.deps, and build.compile
        let sorted = graph.topological_sort().unwrap();
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        assert!(positions["lint"] < positions["test"]);
        assert!(positions["build.deps"] < positions["test"]);
        assert!(positions["build.compile"] < positions["test"]);
    }

    #[test]
    fn test_cycle_with_group_expansion() {
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        // Create a group where a child depends on a task that depends on the group
        // This creates a cycle: setup[0] -> test, test -> setup (expands to setup[0], setup[1])

        // First, add the task that will depend on the group
        let test_task = create_task("test", vec!["setup"], vec![]);
        graph.add_task("test", test_task).unwrap();

        // Create the group where one child depends on test
        let task1 = create_task("s1", vec!["test"], vec![]);
        let task2 = create_task("s2", vec![], vec![]);

        let setup_list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        let setup_node = TaskNode::List(setup_list);
        graph.build_from_node("setup", &setup_node, &tasks).unwrap();

        graph.add_dependency_edges().unwrap();

        // This creates a cycle: test -> setup[0] -> test
        assert!(graph.has_cycles());
        assert!(graph.topological_sort().is_err());
    }

    // =========================================================================
    // Behavioral Contract Tests
    // =========================================================================
    // These tests document and verify behavioral contracts that must hold.
    // They are written to catch subtle bugs that might not break line coverage.

    #[test]
    fn contract_diamond_dependency_executes_shared_dep_once() {
        // Contract: In a diamond dependency (A -> B, A -> C, B -> D, C -> D),
        // task D should appear exactly once in the topological sort.
        let mut graph = TaskGraph::new();

        let task_d = create_task("d", vec![], vec![]);
        let task_b = create_task("b", vec!["d"], vec![]);
        let task_c = create_task("c", vec!["d"], vec![]);
        let task_a = create_task("a", vec!["b", "c"], vec![]);

        graph.add_task("d", task_d).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("a", task_a).unwrap();
        graph.add_dependency_edges().unwrap();

        let sorted = graph.topological_sort().unwrap();
        let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();

        // D should appear exactly once
        let d_count = names.iter().filter(|&&n| n == "d").count();
        assert_eq!(
            d_count, 1,
            "Diamond dependency: shared task should appear exactly once"
        );

        // D must come before B and C
        let d_pos = names.iter().position(|&n| n == "d").unwrap();
        let b_pos = names.iter().position(|&n| n == "b").unwrap();
        let c_pos = names.iter().position(|&n| n == "c").unwrap();
        let a_pos = names.iter().position(|&n| n == "a").unwrap();

        assert!(d_pos < b_pos, "D must execute before B");
        assert!(d_pos < c_pos, "D must execute before C");
        assert!(b_pos < a_pos, "B must execute before A");
        assert!(c_pos < a_pos, "C must execute before A");
    }

    #[test]
    fn contract_parallel_group_children_have_no_implicit_ordering() {
        // Contract: Children in a parallel group should NOT have implicit
        // ordering dependencies between them (they can run concurrently).
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        let task1 = create_task("task1", vec![], vec![]);
        let task2 = create_task("task2", vec![], vec![]);
        let task3 = create_task("task3", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("task1".to_string(), TaskNode::Task(Box::new(task1)));
        parallel_tasks.insert("task2".to_string(), TaskNode::Task(Box::new(task2)));
        parallel_tasks.insert("task3".to_string(), TaskNode::Task(Box::new(task3)));

        let parallel = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let parallel_node = TaskNode::Group(parallel);
        graph
            .build_from_node("parallel", &parallel_node, &tasks)
            .unwrap();
        graph.add_dependency_edges().unwrap();

        // All three tasks should be in the first (and only) parallel group
        let groups = graph.get_parallel_groups().unwrap();
        assert_eq!(groups.len(), 1, "All tasks should be in one parallel group");
        assert_eq!(
            groups[0].len(),
            3,
            "All three tasks should be executable in parallel"
        );
    }

    #[test]
    fn contract_sequential_group_children_have_strict_ordering() {
        // Contract: Children in a sequential group MUST execute in order,
        // with each task depending on the previous one.
        let mut graph = TaskGraph::new();
        let tasks = Tasks::new();

        let task1 = create_task("first", vec![], vec![]);
        let task2 = create_task("second", vec![], vec![]);
        let task3 = create_task("third", vec![], vec![]);

        let sequential = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
                TaskNode::Task(Box::new(task3)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        let seq_node = TaskNode::List(sequential);
        graph.build_from_node("seq", &seq_node, &tasks).unwrap();

        // Topological sort should maintain strict order
        let sorted = graph.topological_sort().unwrap();
        let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();

        // Strict ordering: seq[0] < seq[1] < seq[2]
        let first_pos = names.iter().position(|&n| n == "seq[0]").unwrap();
        let second_pos = names.iter().position(|&n| n == "seq[1]").unwrap();
        let third_pos = names.iter().position(|&n| n == "seq[2]").unwrap();

        assert!(first_pos < second_pos, "seq[0] must execute before seq[1]");
        assert!(second_pos < third_pos, "seq[1] must execute before seq[2]");
    }

    #[test]
    fn contract_task_with_label_is_discoverable() {
        // Contract: Tasks with labels can be found by label
        let task = create_task("build", vec![], vec!["ci", "fast"]);
        assert!(task.labels.contains(&"ci".to_string()));
        assert!(task.labels.contains(&"fast".to_string()));
    }

    #[test]
    fn contract_topological_sort_is_deterministic() {
        // Contract: Multiple calls to topological_sort on the same graph
        // should produce the same order (deterministic for reproducibility).
        let mut graph = TaskGraph::new();

        let task_a = create_task("a", vec![], vec![]);
        let task_b = create_task("b", vec!["a"], vec![]);
        let task_c = create_task("c", vec!["a"], vec![]);
        let task_d = create_task("d", vec!["b", "c"], vec![]);

        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("d", task_d).unwrap();
        graph.add_dependency_edges().unwrap();

        let sort1 = graph.topological_sort().unwrap();
        let sort2 = graph.topological_sort().unwrap();
        let sort3 = graph.topological_sort().unwrap();

        let names1: Vec<&str> = sort1.iter().map(|n| n.name.as_str()).collect();
        let names2: Vec<&str> = sort2.iter().map(|n| n.name.as_str()).collect();
        let names3: Vec<&str> = sort3.iter().map(|n| n.name.as_str()).collect();

        assert_eq!(names1, names2, "Topological sort should be deterministic");
        assert_eq!(names2, names3, "Topological sort should be deterministic");
    }

    #[test]
    fn contract_cycle_detection_catches_all_cycle_types() {
        // Contract: Cycle detection must work for:
        // 1. Self-cycles (A -> A)
        // 2. Two-node cycles (A -> B -> A)
        // 3. Multi-node cycles (A -> B -> C -> A)

        // Self-cycle
        let mut graph1 = TaskGraph::new();
        let self_loop = create_task("self", vec!["self"], vec![]);
        graph1.add_task("self", self_loop).unwrap();
        graph1.add_dependency_edges().unwrap();
        assert!(graph1.has_cycles(), "Self-cycle must be detected");

        // Two-node cycle
        let mut graph2 = TaskGraph::new();
        let a = create_task("a", vec!["b"], vec![]);
        let b = create_task("b", vec!["a"], vec![]);
        graph2.add_task("a", a).unwrap();
        graph2.add_task("b", b).unwrap();
        graph2.add_dependency_edges().unwrap();
        assert!(graph2.has_cycles(), "Two-node cycle must be detected");

        // Three-node cycle
        let mut graph3 = TaskGraph::new();
        let x = create_task("x", vec!["y"], vec![]);
        let y = create_task("y", vec!["z"], vec![]);
        let z = create_task("z", vec!["x"], vec![]);
        graph3.add_task("x", x).unwrap();
        graph3.add_task("y", y).unwrap();
        graph3.add_task("z", z).unwrap();
        graph3.add_dependency_edges().unwrap();
        assert!(graph3.has_cycles(), "Three-node cycle must be detected");
    }

    #[test]
    fn contract_missing_dependency_is_reported() {
        // Contract: If a task depends on a non-existent task, an error
        // should be returned with the missing task name.
        let mut graph = TaskGraph::new();
        let task = create_task("build", vec!["nonexistent"], vec![]);
        graph.add_task("build", task).unwrap();

        let result = graph.add_dependency_edges();
        assert!(result.is_err(), "Missing dependency should be an error");

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent") || err.contains("not found"),
            "Error should mention the missing task name: {err}"
        );
    }

    // =========================================================================
    // TaskResolver Tests
    // =========================================================================

    #[test]
    fn test_parse_path_segments_simple_name() {
        let segments = super::parse_path_segments("build");
        assert_eq!(segments, vec![super::PathSegment::Name("build".into())]);
    }

    #[test]
    fn test_parse_path_segments_dotted() {
        let segments = super::parse_path_segments("build.frontend");
        assert_eq!(
            segments,
            vec![
                super::PathSegment::Name("build".into()),
                super::PathSegment::Name("frontend".into()),
            ]
        );
    }

    #[test]
    fn test_parse_path_segments_indexed() {
        let segments = super::parse_path_segments("build[0]");
        assert_eq!(
            segments,
            vec![
                super::PathSegment::Name("build".into()),
                super::PathSegment::Index(0),
            ]
        );
    }

    #[test]
    fn test_parse_path_segments_nested() {
        let segments = super::parse_path_segments("build.frontend[0]");
        assert_eq!(
            segments,
            vec![
                super::PathSegment::Name("build".into()),
                super::PathSegment::Name("frontend".into()),
                super::PathSegment::Index(0),
            ]
        );
    }

    #[test]
    fn test_task_resolver_single_task() {
        use cuenv_task_graph::TaskResolver;

        let task = create_task("build", vec![], vec![]);
        let mut tasks = Tasks::new();
        tasks
            .tasks
            .insert("build".into(), TaskNode::Task(Box::new(task)));

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Single(t) => assert_eq!(t.command, "echo build"),
            _ => panic!("Expected Single resolution"),
        }
    }

    #[test]
    fn test_task_resolver_parallel_group() {
        use cuenv_task_graph::TaskResolver;

        let frontend = create_task("frontend", vec![], vec![]);
        let backend = create_task("backend", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("frontend".into(), TaskNode::Task(Box::new(frontend)));
        parallel_tasks.insert("backend".into(), TaskNode::Task(Box::new(backend)));

        let group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![TaskDependency::from_name("setup")],
            description: None,
            max_concurrency: None,
        };

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), TaskNode::Group(group));

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Parallel {
                children,
                depends_on,
            } => {
                assert_eq!(children.len(), 2);
                assert!(children.contains(&"build.frontend".to_string()));
                assert!(children.contains(&"build.backend".to_string()));
                assert_eq!(depends_on, vec!["setup"]);
            }
            _ => panic!("Expected Parallel resolution"),
        }
    }

    #[test]
    fn test_task_resolver_sequential_group() {
        use cuenv_task_graph::TaskResolver;

        let task1 = create_task("t1", vec![], vec![]);
        let task2 = create_task("t2", vec![], vec![]);

        let list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), TaskNode::List(list));

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Sequential { children } => {
                assert_eq!(children, vec!["build[0]", "build[1]"]);
            }
            _ => panic!("Expected Sequential resolution"),
        }
    }

    #[test]
    fn test_task_resolver_nested_path() {
        use cuenv_task_graph::TaskResolver;

        let task = create_task("fe", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("frontend".into(), TaskNode::Task(Box::new(task)));

        let group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), TaskNode::Group(group));

        // Resolve nested path
        let resolution = tasks.resolve("build.frontend");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Single(t) => assert_eq!(t.command, "echo fe"),
            _ => panic!("Expected Single resolution"),
        }
    }

    #[test]
    fn test_task_resolver_nonexistent() {
        use cuenv_task_graph::TaskResolver;

        let tasks = Tasks::new();
        assert!(tasks.resolve("nonexistent").is_none());
    }
}
