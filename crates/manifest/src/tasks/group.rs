//! A parallel [`TaskGroup`]: all children run concurrently.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::dependency::TaskDependency;
use super::node::TaskNode;

// =============================================================================
// Parallel Execution (Task Group)
// =============================================================================

/// A parallel task group - all children run concurrently
///
/// Discriminated by the required `type: "group"` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskGroup {
    /// Type discriminator - always "group"
    #[serde(rename = "type")]
    pub type_: String,

    /// Dependencies on other tasks
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Limit concurrent executions (0 = unlimited)
    #[serde(default, rename = "maxConcurrency")]
    pub max_concurrency: Option<u32>,

    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,

    /// Named children - all run concurrently (flattened from remaining fields)
    #[serde(flatten)]
    pub children: HashMap<String, TaskNode>,
}

impl TaskGroup {
    /// Get the number of tasks in this group
    #[must_use]
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Check if the group is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}
