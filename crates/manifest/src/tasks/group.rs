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
    #[serde(flatten, deserialize_with = "children_without_reserved_fields")]
    pub children: HashMap<String, TaskNode>,
}

/// Fields the schema writes on a group that are not children.
///
/// `cuenvRef` says what the node is and which project it belongs to. It is
/// read from the evaluated value where references are resolved; here it only
/// has to stay out of the children.
const RESERVED_FIELDS: &[&str] = &["cuenvRef"];

fn children_without_reserved_fields<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, TaskNode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let fields: HashMap<String, serde_json::Value> = HashMap::deserialize(deserializer)?;
    fields
        .into_iter()
        .filter(|(name, _)| !RESERVED_FIELDS.contains(&name.as_str()))
        .map(|(name, value)| {
            serde_json::from_value(value)
                .map(|node| (name.clone(), node))
                .map_err(|error| serde::de::Error::custom(format!("group child '{name}': {error}")))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_reference_of_a_group_is_not_a_child() {
        let group: TaskGroup = serde_json::from_value(serde_json::json!({
            "cuenvRef": {"kind": "group", "project": "cuenv", "name": "arm"},
            "type": "group",
            "build": {
                "cuenvRef": {"kind": "task", "project": "cuenv", "name": "arm.build"},
                "command": "nix",
                "args": ["build"]
            },
            "nested": {
                "cuenvRef": {"kind": "group", "project": "cuenv", "name": "arm.nested"},
                "type": "group",
                "inner": {"command": "true"}
            }
        }))
        .expect("a group with reserved fields");

        let mut children: Vec<&str> = group.children.keys().map(String::as_str).collect();
        children.sort_unstable();
        assert_eq!(children, ["build", "nested"]);
        let nested = group.children["nested"].as_group().expect("a nested group");
        assert_eq!(nested.children.keys().collect::<Vec<_>>(), ["inner"]);
    }

    #[test]
    fn a_child_that_is_not_a_task_is_reported_by_name() {
        let error = serde_json::from_value::<TaskGroup>(serde_json::json!({
            "type": "group",
            "broken": {"neither": "a task nor a group"}
        }))
        .expect_err("a child that is no task node");

        assert!(
            error.to_string().contains("group child 'broken'"),
            "{error}"
        );
    }
}
