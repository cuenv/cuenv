//! Task data used by the dependency graph.

/// Trait for task data that can be stored in the task graph.
///
/// Implement this trait for your task type to enable it to be stored
/// in a [`crate::TaskGraph`] and participate in dependency resolution.
pub trait TaskNodeData: Clone {
    /// Returns an iterator over the names of tasks this task depends on.
    fn dependency_names(&self) -> impl Iterator<Item = &str>;

    /// Returns true if this task has a dependency on the given task name.
    fn has_dependency(&self, name: &str) -> bool {
        self.dependency_names().any(|n| n == name)
    }
}

/// Trait for task data that supports dependency mutation during graph expansion.
///
/// Implement this for task types used with resolver-backed graph construction,
/// where group-level dependencies or output references need to be folded into
/// the task data before dependency edges are materialized.
pub trait MutableTaskNodeData: TaskNodeData {
    /// Adds a dependency to this task by name.
    fn add_dependency(&mut self, dep: String);
}
