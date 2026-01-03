//! Task resolution abstractions for group expansion.
//!
//! This module provides the [`TaskResolution`] enum and [`TaskResolver`] trait
//! that enable unified handling of single tasks and task groups (sequential/parallel).

use crate::TaskNodeData;

/// How a task name resolves - single task or group.
///
/// This enum represents the three ways a task name can resolve:
/// - A single leaf task (no children)
/// - A sequential group (children run in order)
/// - A parallel group (children run concurrently)
#[derive(Debug, Clone)]
pub enum TaskResolution<T: TaskNodeData> {
    /// A single leaf task with its data.
    Single(T),

    /// Sequential group - tasks run in order (implicit deps between them).
    ///
    /// Children are named like `"build[0]"`, `"build[1]"` for a group named `"build"`.
    Sequential {
        /// Child task names in execution order.
        children: Vec<String>,
    },

    /// Parallel group - tasks can run concurrently.
    ///
    /// Children are named like `"build.frontend"`, `"build.backend"` for a group named `"build"`.
    Parallel {
        /// Child task names (no particular order).
        children: Vec<String>,
        /// Group-level dependencies applied to all children.
        depends_on: Vec<String>,
    },
}

/// Trait for resolving task names to their definitions.
///
/// Implement this trait to provide task lookup and group expansion
/// for use with [`TaskGraph::build_for_task_with_resolver`].
///
/// # Example
///
/// ```ignore
/// impl TaskResolver<Task> for Tasks {
///     fn resolve(&self, name: &str) -> Option<TaskResolution<Task>> {
///         let definition = self.get(name)?;
///         match definition {
///             TaskDefinition::Single(task) => Some(TaskResolution::Single(task.clone())),
///             TaskDefinition::Group(TaskGroup::Sequential(tasks)) => {
///                 let children = (0..tasks.len())
///                     .map(|i| format!("{}[{}]", name, i))
///                     .collect();
///                 Some(TaskResolution::Sequential { children })
///             }
///             // ... parallel handling
///         }
///     }
/// }
/// ```
pub trait TaskResolver<T: TaskNodeData> {
    /// Resolve a task name to its definition (single or group).
    ///
    /// Returns `None` if the task doesn't exist.
    fn resolve(&self, name: &str) -> Option<TaskResolution<T>>;
}
