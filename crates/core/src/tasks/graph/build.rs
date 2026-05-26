use super::TaskGraph;
use crate::Result;
use crate::tasks::{TaskGroup, TaskNode, Tasks};
use cuenv_task_graph::TaskNodeData;
use petgraph::graph::NodeIndex;
use tracing::debug;

impl TaskGraph {
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
            TaskNode::Sequence(steps) => self.build_sequential_list(name, steps, all_tasks),
        }
    }

    /// Build a complete graph from tasks with proper dependency resolution.
    /// This performs a two-pass build: first adding all nodes, then all edges.
    pub fn build_complete_graph(&mut self, tasks: &Tasks) -> Result<()> {
        // First pass: Add all tasks as nodes
        for (name, node) in &tasks.tasks {
            if let TaskNode::Task(task) = node {
                self.add_task(name, task.as_ref().clone())?;
            }
            // Groups and sequences are handled by build_from_node
        }

        // Second pass: Add all dependency edges
        self.add_dependency_edges()
    }

    /// Build graph for a specific task and all its transitive dependencies.
    ///
    /// This uses the [`cuenv_task_graph::TaskResolver`] trait implementation for [`Tasks`] to handle
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

    /// Build a sequential task list (steps run one after another).
    fn build_sequential_list(
        &mut self,
        prefix: &str,
        steps: &[TaskNode],
        all_tasks: &Tasks,
    ) -> Result<Vec<NodeIndex>> {
        let mut nodes = Vec::new();
        let mut previous: Option<NodeIndex> = None;

        // Track child names for group dependency expansion
        let child_names: Vec<String> = (0..steps.len())
            .map(|i| format!("{}[{}]", prefix, i))
            .collect();

        for (i, step) in steps.iter().enumerate() {
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
            .children
            .keys()
            .map(|name| format!("{}.{}", prefix, name))
            .collect();

        for (name, child_node) in &group.children {
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
}
