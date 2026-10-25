//! The [`TaskNode`] union of task, group and sequence, and the root [`Tasks`]
//! map.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::dependency::TaskDependency;
use super::group::TaskGroup;
use super::task::Task;

// =============================================================================
// Sequential Execution (Task Sequence)
// =============================================================================

// TaskSequence is simply Vec<TaskNode> - no wrapper struct needed.
// The sequence is discriminated by being a JSON array.

// =============================================================================
// Task Node (Union Type)
// =============================================================================

/// Union of all task types - explicit typing required in CUE
///
/// This is the recursive type that represents any task node in the tree.
/// Discriminated by:
/// - [`Task`]: Has `command` or `script` field
/// - [`TaskGroup`]: Has `type: "group"` field
/// - Sequence: Is a JSON array `[...]`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskNode {
    /// A single executable task
    Task(Box<Task>),
    /// A parallel task group
    Group(TaskGroup),
    /// A sequential list of tasks (just an array)
    Sequence(Vec<Self>),
}

/// Root tasks structure from CUE
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tasks {
    /// Map of task names to their definitions
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskNode>,
}

impl Tasks {
    /// Create a new empty tasks collection
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a task node by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TaskNode> {
        self.tasks.get(name)
    }

    /// List all task names
    #[must_use]
    pub fn list_tasks(&self) -> Vec<&str> {
        self.tasks.keys().map(String::as_str).collect()
    }

    /// Check if a task exists
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.tasks.contains_key(name)
    }
}

impl TaskNode {
    /// Check if this is a single task
    #[must_use]
    pub fn is_task(&self) -> bool {
        matches!(self, Self::Task(_))
    }

    /// Check if this is a task group (parallel)
    #[must_use]
    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }

    /// Check if this is a sequence (sequential)
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        matches!(self, Self::Sequence(_))
    }

    /// Get as single task if it is one
    #[must_use]
    pub fn as_task(&self) -> Option<&Task> {
        match self {
            Self::Task(task) => Some(task.as_ref()),
            _ => None,
        }
    }

    /// Get as task group if it is one
    #[must_use]
    pub fn as_group(&self) -> Option<&TaskGroup> {
        match self {
            Self::Group(group) => Some(group),
            _ => None,
        }
    }

    /// Get as sequence if it is one
    #[must_use]
    pub fn as_sequence(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Sequence(seq) => Some(seq),
            _ => None,
        }
    }

    /// Get dependencies for this node
    #[must_use]
    pub fn depends_on(&self) -> &[TaskDependency] {
        match self {
            Self::Task(task) => &task.depends_on,
            Self::Group(group) => &group.depends_on,
            Self::Sequence(_) => &[], // Sequences don't have top-level deps
        }
    }

    /// Get description for this node
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Task(task) => task.description.as_deref(),
            Self::Group(group) => group.description.as_deref(),
            Self::Sequence(_) => None, // Sequences don't have descriptions
        }
    }
}
