//! Task graph builder using petgraph
//!
//! This module builds directed acyclic graphs (DAGs) from task definitions
//! to handle dependencies and determine execution order.

use super::{Task, TaskDefinition, TaskGroup, Tasks};
use crate::Result;
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::IntoNodeReferences;
use std::collections::{HashMap, HashSet};

/// A node in the task graph
#[derive(Debug, Clone)]
pub struct TaskNode {
    /// Name of the task
    pub name: String,
    /// The task to execute
    pub task: Task,
}

/// Task graph for dependency resolution and execution ordering
pub struct TaskGraph {
    /// The directed graph of tasks
    graph: DiGraph<TaskNode, ()>,
    /// Map from task names to node indices
    name_to_node: HashMap<String, NodeIndex>,
}

impl TaskGraph {
    /// Create a new empty task graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            name_to_node: HashMap::new(),
        }
    }

    /// Build a graph from a task definition
    pub fn build_from_definition(
        &mut self,
        name: &str,
        definition: &TaskDefinition,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        match definition {
            TaskDefinition::Single(task) => {
                let node = self.add_task(name, task.as_ref().clone())?;
                Ok(vec![node])
            }
            TaskDefinition::Group(group) => self.build_from_group(name, group, all_tasks),
        }
    }

    /// Build a graph from a task group
    fn build_from_group(
        &mut self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        match group {
            TaskGroup::Sequential(tasks) => self.build_sequential_group(prefix, tasks, all_tasks),
            TaskGroup::Parallel(tasks) => self.build_parallel_group(prefix, tasks, all_tasks),
        }
    }

    /// Build a sequential task group (tasks run one after another)
    fn build_sequential_group(
        &mut self,
        prefix: &str,
        tasks: &[TaskDefinition],
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        let mut nodes = Vec::new();
        let mut previous: Option<NodeIndex> = None;

        for (i, task_def) in tasks.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_nodes = self.build_from_definition(&task_name, task_def, all_tasks)?;

            // For sequential execution, link previous task to current
            if let Some(prev) = previous
                && let Some(first) = task_nodes.first()
            {
                self.graph.add_edge(prev, *first, ());
            }

            if let Some(last) = task_nodes.last() {
                previous = Some(*last);
            }

            nodes.extend(task_nodes);
        }

        Ok(nodes)
    }

    /// Build a parallel task group (tasks can run concurrently)
    fn build_parallel_group(
        &mut self,
        prefix: &str,
        tasks: &HashMap<String, TaskDefinition>,
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        let mut nodes = Vec::new();

        for (name, task_def) in tasks {
            let task_name = format!("{}.{}", prefix, name);
            let task_nodes = self.build_from_definition(&task_name, task_def, all_tasks)?;
            nodes.extend(task_nodes);
        }

        Ok(nodes)
    }

    /// Add a single task to the graph
    pub fn add_task(&mut self, name: &str, task: Task) -> Result<NodeIndex> {
        // Check if task already exists
        if let Some(&node) = self.name_to_node.get(name) {
            return Ok(node);
        }

        let node = TaskNode {
            name: name.to_string(),
            task,
        };

        let node_index = self.graph.add_node(node);
        self.name_to_node.insert(name.to_string(), node_index);

        Ok(node_index)
    }

    /// Add dependency edges after all tasks have been added
    /// This ensures proper cycle detection and missing dependency validation
    fn add_dependency_edges(&mut self) -> Result<()> {
        let mut missing_deps = Vec::new();
        let mut edges_to_add = Vec::new();

        // Collect all dependency relationships
        for (node_index, node) in self.graph.node_references() {
            for dep_name in &node.task.depends_on {
                if let Some(&dep_node_index) = self.name_to_node.get(dep_name as &str) {
                    // Record edge to add later
                    edges_to_add.push((dep_node_index, node_index));
                } else {
                    missing_deps.push((node.name.clone(), dep_name.clone()));
                }
            }
        }

        // Report missing dependencies
        if !missing_deps.is_empty() {
            let missing_list = missing_deps
                .iter()
                .map(|(task, dep)| format!("Task '{}' depends on missing task '{}'", task, dep))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(crate::Error::configuration(format!(
                "Missing dependencies: {}",
                missing_list
            )));
        }

        // Add all edges
        for (from, to) in edges_to_add {
            self.graph.add_edge(from, to, ());
        }

        Ok(())
    }

    /// Check if the graph has cycles
    pub fn has_cycles(&self) -> bool {
        is_cyclic_directed(&self.graph)
    }

    /// Get topologically sorted list of tasks
    pub fn topological_sort(&self) -> Result<Vec<TaskNode>> {
        if self.has_cycles() {
            return Err(crate::Error::configuration(
                "Task dependency graph contains cycles".to_string(),
            ));
        }

        match toposort(&self.graph, None) {
            Ok(sorted_indices) => Ok(sorted_indices
                .into_iter()
                .map(|idx| self.graph[idx].clone())
                .collect()),
            Err(_) => Err(crate::Error::configuration(
                "Failed to sort tasks topologically".to_string(),
            )),
        }
    }

    /// Get all tasks that can run in parallel (no dependencies between them)
    pub fn get_parallel_groups(&self) -> Result<Vec<Vec<TaskNode>>> {
        let sorted = self.topological_sort()?;

        if sorted.is_empty() {
            return Ok(vec![]);
        }

        // Group tasks by their dependency level
        let mut groups: Vec<Vec<TaskNode>> = vec![];
        let mut processed: HashMap<String, usize> = HashMap::new();

        for task in sorted {
            // Find the maximum level of all dependencies
            let mut level = 0;
            for dep in &task.task.depends_on {
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

    /// Get the number of tasks in the graph
    pub fn task_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Check if a task exists in the graph
    pub fn contains_task(&self, name: &str) -> bool {
        self.name_to_node.contains_key(name)
    }

    /// Build a complete graph from tasks with proper dependency resolution
    /// This performs a two-pass build: first adding all nodes, then all edges
    pub fn build_complete_graph(&mut self, tasks: &Tasks) -> Result<()> {
        // First pass: Add all tasks as nodes
        for (name, definition) in tasks.tasks.iter() {
            match definition {
                TaskDefinition::Single(task) => {
                    self.add_task(name, task.as_ref().clone())?;
                }
                TaskDefinition::Group(_) => {
                    // For groups, we'd need to expand them - this is more complex
                    // and not needed for the current fix. Groups should be handled
                    // by build_from_definition which already works correctly.
                }
            }
        }

        // Second pass: Add all dependency edges
        self.add_dependency_edges()?;

        Ok(())
    }

    /// Build graph for a specific task and all its transitive dependencies
    pub fn build_for_task(&mut self, task_name: &str, all_tasks: &Tasks) -> Result<()> {
        let mut to_process = vec![task_name.to_string()];
        let mut processed = HashSet::new();

        // First pass: Collect all tasks that need to be included
        while let Some(current_name) = to_process.pop() {
            if processed.contains(&current_name) {
                continue;
            }
            processed.insert(current_name.clone());

            if let Some(definition) = all_tasks.get(&current_name) {
                match definition {
                    TaskDefinition::Single(task) => {
                        self.add_task(&current_name, task.as_ref().clone())?;
                        // Add dependencies to processing queue
                        for dep in &task.depends_on {
                            if !processed.contains(dep) {
                                to_process.push(dep.clone());
                            }
                        }
                    }
                    TaskDefinition::Group(_) => {
                        // Handle groups with build_from_definition
                        self.build_from_definition(&current_name, definition, all_tasks)?;
                    }
                }
            }
        }

        // Second pass: Add dependency edges
        self.add_dependency_edges()?;

        Ok(())
    }
}

impl Default for TaskGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_task(name: &str, deps: Vec<String>) -> Task {
        Task {
            command: format!("echo {}", name),
            args: vec![],
            shell: None,
            env: HashMap::new(),
            depends_on: deps,
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some(format!("Test task {}", name)),
        }
    }

    #[test]
    fn test_task_graph_new() {
        let graph = TaskGraph::new();
        assert_eq!(graph.task_count(), 0);
    }

    #[test]
    fn test_add_single_task() {
        let mut graph = TaskGraph::new();
        let task = create_test_task("test", vec![]);

        let node = graph.add_task("test", task).unwrap();
        assert!(graph.contains_task("test"));
        assert_eq!(graph.task_count(), 1);

        // Adding same task again should return same node
        let task2 = create_test_task("test", vec![]);
        let node2 = graph.add_task("test", task2).unwrap();
        assert_eq!(node, node2);
        assert_eq!(graph.task_count(), 1);
    }

    #[test]
    fn test_task_dependencies() {
        let mut graph = TaskGraph::new();

        // Add tasks with dependencies
        let task1 = create_test_task("task1", vec![]);
        let task2 = create_test_task("task2", vec!["task1".to_string()]);
        let task3 = create_test_task("task3", vec!["task1".to_string(), "task2".to_string()]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

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
        let task1 = create_test_task("task1", vec!["task3".to_string()]);
        let task2 = create_test_task("task2", vec!["task1".to_string()]);
        let task3 = create_test_task("task3", vec!["task2".to_string()]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

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

        let task1 = create_test_task("task1", vec![]);
        let task2 = create_test_task("task2", vec![]);
        let task3 = create_test_task("task3", vec!["task1".to_string()]);
        let task4 = create_test_task("task4", vec!["task2".to_string()]);
        let task5 = create_test_task("task5", vec!["task3".to_string(), "task4".to_string()]);

        graph.add_task("task1", task1).unwrap();
        graph.add_task("task2", task2).unwrap();
        graph.add_task("task3", task3).unwrap();
        graph.add_task("task4", task4).unwrap();
        graph.add_task("task5", task5).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

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

        let task1 = create_test_task("t1", vec![]);
        let task2 = create_test_task("t2", vec![]);

        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(Box::new(task1)),
            TaskDefinition::Single(Box::new(task2)),
        ]);

        let nodes = graph.build_from_group("seq", &group, &tasks).unwrap();
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

        let task1 = create_test_task("t1", vec![]);
        let task2 = create_test_task("t2", vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("first".to_string(), TaskDefinition::Single(Box::new(task1)));
        parallel_tasks.insert(
            "second".to_string(),
            TaskDefinition::Single(Box::new(task2)),
        );

        let group = TaskGroup::Parallel(parallel_tasks);

        let nodes = graph.build_from_group("par", &group, &tasks).unwrap();
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
        let task_a = create_test_task("task_a", vec!["task_c".to_string()]);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()]);
        let task_c = create_test_task("task_c", vec!["task_b".to_string()]);

        graph.add_task("task_a", task_a).unwrap();
        graph.add_task("task_b", task_b).unwrap();
        graph.add_task("task_c", task_c).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

        // This should create a cycle
        assert!(graph.has_cycles());

        // Should fail when trying to get parallel groups
        assert!(graph.get_parallel_groups().is_err());
    }

    #[test]
    fn test_self_dependency_cycle() {
        let mut graph = TaskGraph::new();

        // Create self-referencing task
        let task = create_test_task("self_ref", vec!["self_ref".to_string()]);
        graph.add_task("self_ref", task).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

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
        let task_a = create_test_task("a", vec![]);
        let task_b = create_test_task("b", vec!["a".to_string()]);
        let task_c = create_test_task("c", vec!["a".to_string()]);
        let task_d = create_test_task("d", vec!["b".to_string(), "c".to_string()]);

        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("d", task_d).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

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
        let task = create_test_task("dependent", vec!["missing".to_string()]);
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

        let task = create_test_task("solo", vec![]);
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
        let task_a = create_test_task("a", vec![]);
        let task_b = create_test_task("b", vec!["a".to_string()]);
        let task_c = create_test_task("c", vec!["b".to_string()]);
        let task_d = create_test_task("d", vec!["c".to_string()]);

        graph.add_task("a", task_a).unwrap();
        graph.add_task("b", task_b).unwrap();
        graph.add_task("c", task_c).unwrap();
        graph.add_task("d", task_d).unwrap();
        graph.add_dependency_edges().unwrap(); // Add dependency edges after adding all tasks

        assert!(!graph.has_cycles());
        assert_eq!(graph.task_count(), 4);

        let groups = graph.get_parallel_groups().unwrap();

        // Should be 4 sequential groups
        assert_eq!(groups.len(), 4);
        for group in &groups {
            assert_eq!(group.len(), 1);
        }
    }
}
