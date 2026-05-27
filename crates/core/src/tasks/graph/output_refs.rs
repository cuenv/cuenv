use super::TaskGraph;
use crate::Result;
use crate::tasks::Tasks;
use cuenv_task_graph::{MutableTaskNodeData, TaskNodeData};

impl TaskGraph {
    /// Add implicit dependency edges inferred from task output references.
    ///
    /// Each pair `(from_task, to_task)` means `from_task` references an output
    /// of `to_task` and therefore depends on it. This creates both:
    /// - A `dependsOn` entry on the task data (for consistency)
    /// - An actual petgraph edge (so topological sort / parallel groups work)
    ///
    /// When a referenced target task is not yet in the graph, it is added
    /// (along with its transitive dependencies) from `all_tasks`. This is
    /// necessary because `build_for_task` only follows explicit `dependsOn`
    /// edges and won't discover tasks that are only referenced via output refs.
    pub fn add_output_ref_deps(
        &mut self,
        deps: &[(String, String)],
        all_tasks: &Tasks,
    ) -> Result<()> {
        for (from, to) in deps {
            // Only process pairs where the source task is already in the graph.
            // This avoids pulling in unrelated tasks (e.g., pipeline[1] -> pipeline[0]
            // when the user only asked to run "work").
            if self.inner.get_node_index(from).is_none() {
                continue;
            }

            // Ensure the target task is in the graph (it may not be if the
            // only link to it is through an output reference).
            if self.inner.get_node_index(to).is_none() {
                self.inner
                    .build_for_task_with_resolver(to, all_tasks)
                    .map_err(|e| crate::Error::configuration(e.to_string()))?;
            }

            let from_idx = self.inner.get_node_index(from);
            let to_idx = self.inner.get_node_index(to);

            if let (Some(from_idx), Some(to_idx)) = (from_idx, to_idx) {
                // Skip if this dependency already exists (e.g., user also has explicit dependsOn)
                let already_exists = self
                    .inner
                    .get_task_mut(from)
                    .is_some_and(|d| d.has_dependency(to));

                if !already_exists {
                    // Add dependency to task data for consistency
                    if let Some(from_data) = self.inner.get_task_mut(from) {
                        from_data.add_dependency(to.clone());
                    }
                    // Create actual petgraph edge (to -> from means "to must run before from")
                    self.inner.add_edge(to_idx, from_idx);
                }
            }
        }
        Ok(())
    }
}
