//! DAG export functionality for dry-run mode
//!
//! Exports the task dependency graph in JSON format for analysis and assertions.

use cuenv_core::tasks::{TaskGraph, TaskNode as CoreTaskNode};
use serde::Serialize;

/// Represents an exported task dependency graph.
#[derive(Debug, Serialize)]
pub struct DagExport {
    /// All tasks in the graph with their dependencies.
    pub tasks: Vec<TaskNode>,
    /// Tasks in topologically sorted order (respecting dependencies).
    pub execution_order: Vec<String>,
    /// Tasks grouped by execution level (tasks in same group can run in parallel).
    pub parallel_groups: Vec<Vec<String>>,
}

/// A single task node in the exported DAG.
#[derive(Debug, Serialize)]
pub struct TaskNode {
    /// Task name.
    pub name: String,
    /// Names of tasks this task depends on.
    pub dependencies: Vec<String>,
    /// Task command (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Task description (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl DagExport {
    /// Create a `DagExport` from a `TaskGraph`.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph cannot be topologically sorted (e.g., contains cycles).
    pub fn from_task_graph(graph: &TaskGraph) -> Result<Self, cuenv_core::Error> {
        // Get topologically sorted tasks
        let sorted_nodes = graph.topological_sort().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to sort task graph: {e}"))
        })?;

        // Build task nodes
        let tasks: Vec<TaskNode> = sorted_nodes
            .iter()
            .map(|node| TaskNode {
                name: node.name.clone(),
                dependencies: node.task.depends_on.clone(),
                command: Some(node.task.command.clone()),
                description: node.task.description.clone(),
            })
            .collect();

        // Get execution order as just the names
        let execution_order: Vec<String> = sorted_nodes.iter().map(|n| n.name.clone()).collect();

        // Build parallel groups based on dependency levels
        let parallel_groups = build_parallel_groups(&sorted_nodes);

        Ok(Self {
            tasks,
            execution_order,
            parallel_groups,
        })
    }
}

/// Build parallel execution groups from sorted tasks.
///
/// Tasks in the same group have all their dependencies satisfied by previous groups
/// and can therefore be executed in parallel.
fn build_parallel_groups(sorted_nodes: &[CoreTaskNode]) -> Vec<Vec<String>> {
    use std::collections::HashMap;

    if sorted_nodes.is_empty() {
        return Vec::new();
    }

    // Track which level each task is at
    let mut levels: HashMap<String, usize> = HashMap::new();

    for node in sorted_nodes {
        // A task's level is one more than the maximum level of its dependencies
        let max_dep_level = node
            .task
            .depends_on
            .iter()
            .filter_map(|dep| levels.get(dep).copied())
            .max()
            .unwrap_or(0);

        let level = if node.task.depends_on.is_empty() {
            0
        } else {
            max_dep_level + 1
        };

        levels.insert(node.name.clone(), level);
    }

    // Group tasks by level
    let max_level = levels.values().copied().max().unwrap_or(0);
    let mut groups: Vec<Vec<String>> = vec![Vec::new(); max_level + 1];

    for node in sorted_nodes {
        if let Some(&level) = levels.get(&node.name) {
            groups[level].push(node.name.clone());
        }
    }

    // Remove empty groups
    groups.into_iter().filter(|g| !g.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dag_export_serialization() {
        let export = DagExport {
            tasks: vec![
                TaskNode {
                    name: "build".to_string(),
                    dependencies: vec![],
                    command: Some("cargo build".to_string()),
                    description: Some("Build the project".to_string()),
                },
                TaskNode {
                    name: "test".to_string(),
                    dependencies: vec!["build".to_string()],
                    command: Some("cargo test".to_string()),
                    description: None,
                },
            ],
            execution_order: vec!["build".to_string(), "test".to_string()],
            parallel_groups: vec![vec!["build".to_string()], vec!["test".to_string()]],
        };

        let json = serde_json::to_string_pretty(&export).unwrap();
        assert!(json.contains("\"name\": \"build\""));
        assert!(json.contains("\"execution_order\""));
        assert!(json.contains("\"parallel_groups\""));
    }
}
