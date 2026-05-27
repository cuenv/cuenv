//! Resolver-backed graph construction.

use std::collections::{HashMap, HashSet};

use tracing::debug;

use super::TaskGraph;
use crate::{MutableTaskNodeData, Result, TaskResolution, TaskResolver};

impl<T: MutableTaskNodeData> TaskGraph<T> {
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
        ResolverGraphBuilder::new(self, resolver).build(task_name)
    }
}

struct ResolverGraphBuilder<'graph, 'resolver, T, R>
where
    T: MutableTaskNodeData,
    R: TaskResolver<T>,
{
    graph: &'graph mut TaskGraph<T>,
    resolver: &'resolver R,
    to_process: Vec<String>,
    processed: HashSet<String>,
    sequential_orderings: Vec<Vec<String>>,
    pending_group_deps: HashMap<String, Vec<String>>,
}

impl<'graph, 'resolver, T, R> ResolverGraphBuilder<'graph, 'resolver, T, R>
where
    T: MutableTaskNodeData,
    R: TaskResolver<T>,
{
    fn new(graph: &'graph mut TaskGraph<T>, resolver: &'resolver R) -> Self {
        Self {
            graph,
            resolver,
            to_process: Vec::new(),
            processed: HashSet::new(),
            sequential_orderings: Vec::new(),
            pending_group_deps: HashMap::new(),
        }
    }

    fn build(mut self, task_name: &str) -> Result<()> {
        self.to_process.push(task_name.to_string());
        debug!("Building graph with resolver for '{}'", task_name);

        self.collect_tasks()?;
        self.add_sequential_edges();
        self.graph.add_dependency_edges()
    }

    fn collect_tasks(&mut self) -> Result<()> {
        while let Some(current_name) = self.to_process.pop() {
            if !self.processed.insert(current_name.clone()) {
                continue;
            }

            match self.resolver.resolve(&current_name) {
                Some(TaskResolution::Single(task)) => self.add_single_task(&current_name, task)?,
                Some(TaskResolution::Sequential { children }) => {
                    self.add_sequential_group(&current_name, children);
                }
                Some(TaskResolution::Parallel {
                    children,
                    depends_on,
                }) => {
                    self.add_parallel_group(&current_name, children, depends_on);
                }
                None => {
                    debug!("Task '{}' not found while building graph", current_name);
                }
            }
        }

        Ok(())
    }

    fn add_single_task(&mut self, current_name: &str, mut task: T) -> Result<()> {
        self.apply_pending_group_dependencies(current_name, &mut task);
        let deps = task
            .dependency_names()
            .map(String::from)
            .collect::<Vec<_>>();

        self.graph.add_task(current_name, task)?;
        self.queue_unprocessed(deps);

        Ok(())
    }

    fn add_sequential_group(&mut self, current_name: &str, children: Vec<String>) {
        self.graph.register_group(current_name, children.clone());
        self.sequential_orderings.push(children.clone());
        self.queue_unprocessed(children);
    }

    fn add_parallel_group(
        &mut self,
        current_name: &str,
        children: Vec<String>,
        depends_on: Vec<String>,
    ) {
        self.graph.register_group(current_name, children.clone());

        if !depends_on.is_empty() {
            self.pending_group_deps
                .insert(current_name.to_string(), depends_on.clone());
            self.queue_unprocessed(depends_on);
        }

        self.queue_unprocessed(children);
    }

    fn apply_pending_group_dependencies(&self, task_name: &str, task: &mut T) {
        let path_parts = task_name.split('.').collect::<Vec<_>>();
        for i in 1..path_parts.len() {
            let parent_path = path_parts[..i].join(".");
            self.apply_parent_dependencies(&parent_path, task);
        }

        if let Some(bracket_idx) = task_name.find('[') {
            self.apply_parent_dependencies(&task_name[..bracket_idx], task);
        }
    }

    fn apply_parent_dependencies(&self, parent_path: &str, task: &mut T) {
        if let Some(deps) = self.pending_group_deps.get(parent_path) {
            for dep in deps {
                task.add_dependency(dep.clone());
            }
        }
    }

    fn add_sequential_edges(&mut self) {
        for ordering in &self.sequential_orderings {
            for window in ordering.windows(2) {
                if let [prev, next] = window
                    && let (Some(prev_idx), Some(next_idx)) = (
                        self.graph.get_node_index(prev),
                        self.graph.get_node_index(next),
                    )
                {
                    self.graph.add_edge(prev_idx, next_idx);
                }
            }
        }
    }

    fn queue_unprocessed(&mut self, names: impl IntoIterator<Item = String>) {
        self.to_process.extend(
            names
                .into_iter()
                .filter(|name| !self.processed.contains(name)),
        );
    }
}
