use super::TaskGraph;
use crate::TaskNodeData;
use petgraph::Direction::Outgoing;
use std::collections::{HashSet, VecDeque};

/// Borrowed predicate used to resolve whether an external dependency is affected.
pub type ExternalAffectedResolver<'resolver> = &'resolver dyn Fn(&str) -> bool;

impl<T: TaskNodeData> TaskGraph<T> {
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
    /// ```
    /// use cuenv_task_graph::{TaskGraph, TaskNodeData};
    /// #[derive(Clone)]
    /// struct Task { dependencies: Vec<String>, changed: bool }
    /// impl TaskNodeData for Task {
    ///     fn dependency_names(&self) -> impl Iterator<Item = &str> {
    ///         self.dependencies.iter().map(String::as_str)
    ///     }
    /// }
    /// let mut graph = TaskGraph::new();
    /// let build = graph.add_task("build", Task { dependencies: vec![], changed: true })?;
    /// let test = graph.add_task("test", Task { dependencies: vec!["build".into()], changed: false })?;
    /// graph.add_task("deploy", Task { dependencies: vec!["#other:build".into()], changed: false })?;
    /// graph.add_edge(build, test);
    /// // Without external dependency checking.
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.changed,
    ///     None,
    /// );
    ///
    /// assert_eq!(affected, ["build", "test"]);
    /// // With external dependency checking (for CI cross-project deps).
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.changed,
    ///     Some(&|dep: &str| dep == "#other:build"),
    /// );
    /// assert_eq!(affected, ["build", "test", "deploy"]);
    /// # Ok::<(), cuenv_task_graph::Error>(())
    /// ```
    pub fn compute_affected<F>(
        &self,
        pipeline_tasks: &[impl AsRef<str>],
        is_directly_affected: F,
        is_external_affected: Option<ExternalAffectedResolver<'_>>,
    ) -> Vec<String>
    where
        F: Fn(&T) -> bool,
    {
        let pipeline_names: Vec<&str> = pipeline_tasks.iter().map(AsRef::as_ref).collect();
        let pipeline_set: HashSet<&str> = pipeline_names.iter().copied().collect();
        let mut affected = HashSet::new();
        let mut queue = VecDeque::new();

        for task_name in &pipeline_names {
            let Some(&node_index) = self.name_to_node.get(*task_name) else {
                continue;
            };
            let node = &self.graph[node_index];
            if (is_directly_affected(&node.task)
                || Self::has_affected_external_dependency(&node.task, is_external_affected))
                && affected.insert((*task_name).to_string())
            {
                queue.push_back(node_index);
            }
        }

        while let Some(node_index) = queue.pop_front() {
            for dependent_index in self.graph.neighbors_directed(node_index, Outgoing) {
                let dependent_name = self.graph[dependent_index].name.as_str();
                if !pipeline_set.contains(dependent_name) || affected.contains(dependent_name) {
                    continue;
                }
                affected.insert(dependent_name.to_string());
                queue.push_back(dependent_index);
            }
        }

        affected_in_pipeline_order(&pipeline_names, &affected)
    }

    fn has_affected_external_dependency(
        task: &T,
        is_external_affected: Option<ExternalAffectedResolver<'_>>,
    ) -> bool {
        let Some(resolver) = is_external_affected else {
            return false;
        };

        task.dependency_names()
            .any(|dep| dep.starts_with('#') && resolver(dep))
    }
}

fn affected_in_pipeline_order(
    pipeline_tasks: &[impl AsRef<str>],
    affected: &HashSet<String>,
) -> Vec<String> {
    pipeline_tasks
        .iter()
        .filter_map(|task| {
            let task = task.as_ref();
            affected.contains(task).then(|| task.to_string())
        })
        .collect()
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
/// ```
/// use std::collections::HashMap;
///
/// let deps: HashMap<&str, Vec<String>> = [
///     ("build", vec![]),
///     ("test", vec!["build".to_string()]),
///     ("deploy", vec!["test".to_string()]),
/// ].into_iter().collect();
///
/// let closure = cuenv_task_graph::compute_transitive_closure(
///     ["deploy"],
///     |name| deps.get(name).map(|v| v.as_slice()),
/// );
/// assert_eq!(closure.len(), 3);
/// assert!(closure.contains("build"));
/// ```
#[must_use]
pub fn compute_transitive_closure<'a>(
    initial: impl IntoIterator<Item = &'a str>,
    get_deps: impl Fn(&str) -> Option<&'a [String]>,
) -> HashSet<String> {
    let mut all = HashSet::new();
    let mut frontier: Vec<&str> = Vec::new();

    for name in initial {
        if all.insert(name.to_string()) {
            frontier.push(name);
        }
    }

    while let Some(task_id) = frontier.pop() {
        if let Some(deps) = get_deps(task_id) {
            for dep in deps {
                if all.insert(dep.clone()) {
                    frontier.push(dep.as_str());
                }
            }
        }
    }

    all
}
