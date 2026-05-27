use super::TaskGraph;
use crate::TaskNodeData;
use std::collections::HashSet;

/// Borrowed predicate used to resolve whether an external dependency is affected.
pub type ExternalAffectedResolver<'a> = &'a dyn Fn(&str) -> bool;

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
    /// ```ignore
    /// // Without external dependency checking
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.is_affected_by(&changed_files, &project_root),
    ///     None,
    /// );
    ///
    /// // With external dependency checking (for CI cross-project deps)
    /// let affected = graph.compute_affected(
    ///     &["build", "test", "deploy"],
    ///     |task| task.is_affected_by(&changed_files, &project_root),
    ///     Some(&|dep: &str| check_external_dependency(dep, &all_projects, &changed_files)),
    /// );
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
        let mut affected = HashSet::new();

        self.mark_directly_affected(pipeline_tasks, &is_directly_affected, &mut affected);
        self.propagate_affected(pipeline_tasks, is_external_affected, &mut affected);
        affected_in_pipeline_order(pipeline_tasks, &affected)
    }

    fn mark_directly_affected<F>(
        &self,
        pipeline_tasks: &[impl AsRef<str>],
        is_directly_affected: &F,
        affected: &mut HashSet<String>,
    ) where
        F: Fn(&T) -> bool,
    {
        for task_name in pipeline_tasks {
            let task_name = task_name.as_ref();
            if let Some(node) = self.get_node_by_name(task_name)
                && is_directly_affected(&node.task)
            {
                affected.insert(task_name.to_string());
            }
        }
    }

    fn propagate_affected(
        &self,
        pipeline_tasks: &[impl AsRef<str>],
        is_external_affected: Option<ExternalAffectedResolver<'_>>,
        affected: &mut HashSet<String>,
    ) {
        let mut changed = true;
        while changed {
            changed = false;
            for task_name in pipeline_tasks {
                changed |= self.propagate_task_affected(
                    task_name.as_ref(),
                    is_external_affected,
                    affected,
                );
            }
        }
    }

    fn propagate_task_affected(
        &self,
        task_name: &str,
        is_external_affected: Option<ExternalAffectedResolver<'_>>,
        affected: &mut HashSet<String>,
    ) -> bool {
        if affected.contains(task_name) {
            return false;
        }

        let Some(node) = self.get_node_by_name(task_name) else {
            return false;
        };

        for dep in node.task.dependency_names() {
            if self.dependency_is_affected(dep, is_external_affected, affected) {
                affected.insert(task_name.to_string());
                return true;
            }
        }

        false
    }

    fn dependency_is_affected(
        &self,
        dep: &str,
        is_external_affected: Option<ExternalAffectedResolver<'_>>,
        affected: &HashSet<String>,
    ) -> bool {
        if dep.starts_with('#') {
            return is_external_affected.is_some_and(|resolver| resolver(dep));
        }

        self.expand_dep_to_leaf_tasks(dep)
            .into_iter()
            .any(|leaf_dep| affected.contains(&leaf_dep))
    }
}

fn affected_in_pipeline_order(
    pipeline_tasks: &[impl AsRef<str>],
    affected: &HashSet<String>,
) -> Vec<String> {
    pipeline_tasks
        .iter()
        .map(|task| task.as_ref().to_string())
        .filter(|task| affected.contains(task))
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
