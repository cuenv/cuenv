//! Code ownership rule type for cuenv `.rules.cue` manifests.
//!
//! Ownership is configured per directory through `schema/rules`
//! (`rules.#DirectoryRules.owners`). The rules sync provider aggregates the
//! rules and the codeowners crate renders them for the detected platform.
//!
//! Based on schema/rules/owners.cue

use serde::{Deserialize, Serialize};

/// A single code ownership rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerRule {
    /// File pattern (glob syntax) - same as CODEOWNERS format.
    pub pattern: String,

    /// Owners for this pattern.
    pub owners: Vec<String>,

    /// Optional description for this rule (added as comment above the rule).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Section name for grouping rules in the output file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,

    /// Optional order for deterministic output (lower values appear first).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_owner_rule_rejects_unknown_fields() {
        let err = serde_json::from_value::<OwnerRule>(serde_json::json!({
            "pattern": "*.rs",
            "owners": ["@rust-team"],
            "priority": 10
        }))
        .expect_err("unknown owner rule field should fail");

        assert!(
            err.to_string().contains("unknown field `priority`"),
            "expected unknown field error, got: {err}"
        );
    }
}
