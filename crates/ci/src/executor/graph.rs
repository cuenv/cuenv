//! CI Task Graph
//!
//! Builds a directed acyclic graph (DAG) from IR tasks for dependency-ordered
//! parallel execution.

use crate::compiler::digest::compute_task_digest;
use crate::ir::{IntermediateRepresentation, Task as IRTask};
use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::IntoNodeReferences;
use std::collections::HashMap;
use thiserror::Error;

/// Error types for task graph operations
#[derive(Debug, Error)]
pub enum GraphError {
    /// Task dependency cycle detected
    #[error("Task dependency graph contains cycle involving: {tasks}")]
    CyclicDependency { tasks: String },

    /// Missing dependency reference
    #[error("Task '{task}' depends on non-existent task '{dependency}'")]
    MissingDependency { task: String, dependency: String },

    /// Topological sort failed
    #[error("Failed to determine task execution order")]
    SortFailed,
}

/// A node in the CI task graph
#[derive(Debug, Clone)]
pub struct CITaskNode {
    /// Task ID from IR
    pub id: String,
    /// The IR task definition
    pub task: IRTask,
    /// Pre-computed digest for cache lookup (computed after secret resolution)
    pub digest: String,
}

/// CI Task graph for dependency resolution and parallel execution
pub struct CITaskGraph {
    /// The directed graph of tasks
    graph: DiGraph<CITaskNode, ()>,
    /// Map from task IDs to node indices
    id_to_index: HashMap<String, NodeIndex>,
}

impl CITaskGraph {
    /// Build a task graph from an IR document
    ///
    /// # Errors
    /// Returns error if dependencies reference non-existent tasks or if
    /// the graph contains cycles.
    pub fn from_ir(ir: &IntermediateRepresentation) -> Result<Self, GraphError> {
        let mut graph = DiGraph::new();
        let mut id_to_index = HashMap::new();

        // First pass: Add all tasks as nodes
        for task in &ir.tasks {
            let node = CITaskNode {
                id: task.id.clone(),
                task: task.clone(),
                digest: String::new(), // Computed later with secrets
            };
            let index = graph.add_node(node);
            id_to_index.insert(task.id.clone(), index);
        }

        // Second pass: Add dependency edges
        let mut edges_to_add = Vec::new();
        for task in &ir.tasks {
            let task_index = id_to_index[&task.id];
            for dep_id in &task.depends_on {
                let dep_index =
                    id_to_index
                        .get(dep_id)
                        .ok_or_else(|| GraphError::MissingDependency {
                            task: task.id.clone(),
                            dependency: dep_id.clone(),
                        })?;
                // Edge goes from dependency to dependent (dep -> task)
                edges_to_add.push((*dep_index, task_index));
            }
        }

        for (from, to) in edges_to_add {
            graph.add_edge(from, to, ());
        }

        let result = Self { graph, id_to_index };

        // Check for cycles
        if result.has_cycles() {
            // Find tasks involved in cycle for error message
            let task_ids: Vec<_> = result
                .graph
                .node_references()
                .map(|(_, n)| n.id.clone())
                .collect();
            return Err(GraphError::CyclicDependency {
                tasks: task_ids.join(", "),
            });
        }

        Ok(result)
    }

    /// Check if the graph contains cycles
    #[must_use]
    pub fn has_cycles(&self) -> bool {
        is_cyclic_directed(&self.graph)
    }

    /// Get the number of tasks in the graph
    #[must_use]
    pub fn task_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get tasks grouped by dependency level for parallel execution
    ///
    /// Returns groups where all tasks in a group can execute concurrently
    /// because they have no dependencies on each other.
    ///
    /// # Errors
    /// Returns error if topological sort fails (shouldn't happen if cycle
    /// check passed).
    pub fn get_parallel_groups(&self) -> Result<Vec<Vec<&CITaskNode>>, GraphError> {
        // Topological sort
        let sorted_indices = toposort(&self.graph, None).map_err(|_| GraphError::SortFailed)?;

        if sorted_indices.is_empty() {
            return Ok(vec![]);
        }

        // Group tasks by their dependency level
        let mut groups: Vec<Vec<&CITaskNode>> = vec![];
        let mut processed: HashMap<&str, usize> = HashMap::new();

        for node_index in sorted_indices {
            let node = &self.graph[node_index];

            // Find the maximum level of all dependencies
            let mut level = 0;
            for dep_id in &node.task.depends_on {
                if let Some(&dep_level) = processed.get(dep_id.as_str()) {
                    level = level.max(dep_level + 1);
                }
            }

            // Add to appropriate group
            if level >= groups.len() {
                groups.resize_with(level + 1, Vec::new);
            }
            groups[level].push(node);
            processed.insert(&node.id, level);
        }

        Ok(groups)
    }

    /// Compute digests for all tasks after secret resolution
    ///
    /// This must be called after secrets are resolved to include secret
    /// fingerprints in the digest computation.
    ///
    /// # Arguments
    /// * `ir` - The IR document (for runtime lookups)
    /// * `secret_fingerprints` - Map of `task_id` -> (`secret_name` -> fingerprint)
    /// * `system_salt` - Optional system salt for secret HMAC
    pub fn compute_digests(
        &mut self,
        ir: &IntermediateRepresentation,
        secret_fingerprints: &HashMap<String, HashMap<String, String>>,
        system_salt: Option<&str>,
    ) {
        for node_index in self.graph.node_indices() {
            let node = &self.graph[node_index];
            let task = &node.task;

            // Get runtime digest if task has runtime
            let runtime_digest = task
                .runtime
                .as_ref()
                .and_then(|rid| ir.runtimes.iter().find(|r| &r.id == rid))
                .map(|r| r.digest.as_str());

            // Get secret fingerprints for this task
            let task_fingerprints = secret_fingerprints.get(&task.id);

            let digest = compute_task_digest(
                &task.command,
                &task.env,
                &task.inputs,
                runtime_digest,
                task_fingerprints,
                system_salt,
            );

            // Update the node's digest
            self.graph[node_index].digest = digest;
        }
    }

    /// Get a task node by ID
    #[must_use]
    pub fn get_task(&self, id: &str) -> Option<&CITaskNode> {
        self.id_to_index.get(id).map(|&idx| &self.graph[idx])
    }

    /// Get all task IDs in the graph
    #[must_use]
    pub fn task_ids(&self) -> Vec<&str> {
        self.graph
            .node_references()
            .map(|(_, n)| n.id.as_str())
            .collect()
    }

    /// Propagate `cache_policy`: disabled to tasks that transitively depend on deployment tasks
    ///
    /// According to PRD v1.3, tasks depending on deployments should inherit
    /// `cache_policy`: disabled for that execution to ensure deployment ordering
    /// is always respected.
    ///
    /// # Returns
    /// List of task IDs that had their cache policy changed
    pub fn propagate_deployment_cache_policy(&mut self) -> Vec<String> {
        use crate::ir::CachePolicy;
        use petgraph::visit::Dfs;

        let mut changed = Vec::new();

        // Find all deployment task indices
        let deployment_indices: Vec<NodeIndex> = self
            .graph
            .node_indices()
            .filter(|&idx| self.graph[idx].task.deployment)
            .collect();

        // For each deployment task, traverse all descendants and mark them as disabled
        for deploy_idx in deployment_indices {
            // Use DFS to find all tasks that depend on this deployment
            // We need to traverse in reverse direction (dependents of deployment)
            let mut dfs = Dfs::new(&self.graph, deploy_idx);
            while let Some(node_idx) = dfs.next(&self.graph) {
                // Skip the deployment task itself (already marked disabled by validation)
                if node_idx == deploy_idx {
                    continue;
                }

                let node = &mut self.graph[node_idx];
                if node.task.cache_policy != CachePolicy::Disabled {
                    tracing::debug!(
                        task = %node.id,
                        reason = "depends on deployment task",
                        "Setting cache_policy to disabled"
                    );
                    node.task.cache_policy = CachePolicy::Disabled;
                    changed.push(node.id.clone());
                }
            }
        }

        // Deduplicate (task may depend on multiple deployment tasks)
        changed.sort();
        changed.dedup();

        if !changed.is_empty() {
            tracing::info!(
                count = changed.len(),
                tasks = ?changed,
                "Disabled caching for tasks depending on deployments"
            );
        }

        changed
    }

    /// Check if a task transitively depends on any deployment task
    #[must_use]
    pub fn depends_on_deployment(&self, task_id: &str) -> bool {
        use petgraph::algo::has_path_connecting;

        let Some(&task_idx) = self.id_to_index.get(task_id) else {
            return false;
        };

        // Check if there's a path from any deployment task to this task
        for node_idx in self.graph.node_indices() {
            if self.graph[node_idx].task.deployment
                && node_idx != task_idx
                && has_path_connecting(&self.graph, node_idx, task_idx, None)
            {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CachePolicy, StageConfiguration, Task};

    fn make_task(id: &str, deps: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string(), id.to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: deps.iter().map(|s| (*s).to_string()).collect(),
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        }
    }

    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: crate::ir::PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks,
        }
    }

    #[test]
    fn test_simple_graph() {
        let ir = make_ir(vec![make_task("build", &[]), make_task("test", &["build"])]);

        let graph = CITaskGraph::from_ir(&ir).unwrap();
        assert_eq!(graph.task_count(), 2);
        assert!(!graph.has_cycles());
    }

    #[test]
    fn test_parallel_groups_linear() {
        // build -> test -> deploy (all sequential)
        let ir = make_ir(vec![
            make_task("build", &[]),
            make_task("test", &["build"]),
            make_task("deploy", &["test"]),
        ]);

        let graph = CITaskGraph::from_ir(&ir).unwrap();
        let groups = graph.get_parallel_groups().unwrap();

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].len(), 1); // build
        assert_eq!(groups[1].len(), 1); // test
        assert_eq!(groups[2].len(), 1); // deploy
    }

    #[test]
    fn test_parallel_groups_diamond() {
        // build -> test1 -\
        //       -> test2 -/-> deploy
        let ir = make_ir(vec![
            make_task("build", &[]),
            make_task("test1", &["build"]),
            make_task("test2", &["build"]),
            make_task("deploy", &["test1", "test2"]),
        ]);

        let graph = CITaskGraph::from_ir(&ir).unwrap();
        let groups = graph.get_parallel_groups().unwrap();

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].len(), 1); // build
        assert_eq!(groups[1].len(), 2); // test1, test2 (parallel)
        assert_eq!(groups[2].len(), 1); // deploy
    }

    #[test]
    fn test_cycle_detection() {
        let ir = make_ir(vec![
            make_task("a", &["c"]),
            make_task("b", &["a"]),
            make_task("c", &["b"]),
        ]);

        let result = CITaskGraph::from_ir(&ir);
        assert!(matches!(result, Err(GraphError::CyclicDependency { .. })));
    }

    #[test]
    fn test_missing_dependency() {
        let ir = make_ir(vec![make_task("test", &["nonexistent"])]);

        let result = CITaskGraph::from_ir(&ir);
        assert!(matches!(result, Err(GraphError::MissingDependency { .. })));
    }

    #[test]
    fn test_digest_computation() {
        let ir = make_ir(vec![make_task("build", &[])]);

        let mut graph = CITaskGraph::from_ir(&ir).unwrap();
        graph.compute_digests(&ir, &HashMap::new(), None);

        let task = graph.get_task("build").unwrap();
        assert!(!task.digest.is_empty());
        assert!(task.digest.starts_with("sha256:"));
    }

    #[test]
    fn test_independent_tasks_same_group() {
        // Three independent tasks should all be in level 0
        let ir = make_ir(vec![
            make_task("task1", &[]),
            make_task("task2", &[]),
            make_task("task3", &[]),
        ]);

        let graph = CITaskGraph::from_ir(&ir).unwrap();
        let groups = graph.get_parallel_groups().unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }
}
