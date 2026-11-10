//! Named context DTOs.
//!
//! A context is a filter over one invocation of cuenv (see
//! `schema/context.cue`). A filter is data: a field left out matches anything,
//! and a string is a regular expression that must match the whole value.
//! Matching lives in `cuenv-core` (`context`).

use serde::{Deserialize, Serialize};

/// A named context: what an invocation must look like for it to be active.
///
/// The two filters share no field, so the fields of an entry decide which one
/// it is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ContextDefinition {
    /// Matches the tasks cuenv runs (`#TaskFilter`).
    Task(TaskFilter),
    /// Matches the facts of an invocation (`#When`).
    When(When),
}

/// A filter over the tasks cuenv runs, one task at a time (`#TaskFilter`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskFilter {
    /// The task's full name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The program the task runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Labels the task must carry: each entry must match one of its labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl TaskFilter {
    /// Whether the filter names nothing, and so says nothing about a task.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.command.is_none() && self.labels.is_empty()
    }
}

/// A filter over the facts of an invocation (`#When`), by fact name. A value
/// is a string, `null`, or the fields of a fact that has some.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct When(pub serde_json::Map<String, serde_json::Value>);
