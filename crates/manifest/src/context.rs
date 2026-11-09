//! Named context DTOs.
//!
//! A context is a constraint over the facts of an invocation (see `#Context`
//! in `schema/context.cue`). The CUE bridge exports each constraint as a
//! [`ContextMatcher`], because a constraint is not data and would otherwise
//! export as `null`. Matching lives in `cuenv-core` (`context`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A named context: when it is active, and which contexts can be active with it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContextDefinition {
    /// The constraint an invocation's facts must satisfy.
    pub when: ContextMatcher,

    /// The other contexts whose constraint can hold at the same time.
    #[serde(default)]
    pub overlaps: Vec<String>,
}

/// The kinds a constraint can require without naming a value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ContextKind {
    /// Any string.
    String,
    /// Any boolean.
    Bool,
    /// Any number.
    Number,
    /// Any value.
    Any,
}

/// A compiled `when` constraint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ContextMatcher {
    /// The fact is exactly this value (`null` included).
    Equals(serde_json::Value),
    /// The fact has this value's type and is a different value (`!=`).
    NotEquals(serde_json::Value),
    /// The fact has this kind.
    Kind(ContextKind),
    /// The fact is a string the regular expression matches (`=~`).
    Matches(String),
    /// The fact is a string the regular expression does not match (`!~`).
    NotMatches(String),
    /// Some matcher holds (`|`).
    AnyOf(Vec<Self>),
    /// Every matcher holds (`&`).
    AllOf(Vec<Self>),
    /// The fact is a struct whose named fields match.
    Fields(BTreeMap<String, Self>),
}
