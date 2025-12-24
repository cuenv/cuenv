//! Code ownership configuration types for cuenv manifests.
//!
//! This module provides serde-compatible types for deserializing CODEOWNERS
//! configuration from CUE manifests.
//!
//! Provider crates (cuenv-github, cuenv-gitlab, cuenv-bitbucket) handle the
//! platform-specific logic (file paths, section styles) based on repository
//! structure detection.
//!
//! Based on schema/owners.cue

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Output configuration for CODEOWNERS file generation.
///
/// Note: The `platform` field is kept for CUE schema compatibility but is not
/// used directly. The provider is detected at runtime based on repository
/// structure (`.github/` directory, `.gitlab-ci.yml`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OwnersOutput {
    /// Platform hint (e.g., "github", "gitlab", "bitbucket").
    /// Provider is detected automatically; this field is for schema compatibility.
    pub platform: Option<String>,

    /// Custom path for CODEOWNERS file (overrides platform default).
    pub path: Option<String>,

    /// Header comment to include at the top of the generated file.
    pub header: Option<String>,
}

/// A single code ownership rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Code ownership configuration for a project.
///
/// This type is designed for deserializing from CUE manifests.
/// Use `cuenv_codeowners` to convert to the library type for generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Owners {
    /// Output configuration for CODEOWNERS file generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OwnersOutput>,

    /// Code ownership rules - maps rule names to rule definitions.
    /// Using a map enables CUE unification/layering across configs.
    #[serde(default)]
    pub rules: HashMap<String, OwnerRule>,
}

impl Owners {
    /// Get rules sorted by order then by key for determinism.
    #[must_use]
    pub fn sorted_rules(&self) -> Vec<(&String, &OwnerRule)> {
        let mut rule_entries: Vec<_> = self.rules.iter().collect();
        rule_entries.sort_by(|a, b| {
            let order_a = a.1.order.unwrap_or(i32::MAX);
            let order_b = b.1.order.unwrap_or(i32::MAX);
            order_a.cmp(&order_b).then_with(|| a.0.cmp(b.0))
        });
        rule_entries
    }

    /// Get the header, if any.
    #[must_use]
    pub fn header(&self) -> Option<&str> {
        self.output.as_ref().and_then(|o| o.header.as_deref())
    }

    /// Get the custom path, if specified.
    #[must_use]
    pub fn custom_path(&self) -> Option<&str> {
        self.output.as_ref().and_then(|o| o.path.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sorted_rules() {
        let mut rules = HashMap::new();
        rules.insert(
            "z-last".to_string(),
            OwnerRule {
                pattern: "*.last".to_string(),
                owners: vec!["@team".to_string()],
                description: None,
                section: None,
                order: Some(3),
            },
        );
        rules.insert(
            "a-first".to_string(),
            OwnerRule {
                pattern: "*.first".to_string(),
                owners: vec!["@team".to_string()],
                description: None,
                section: None,
                order: Some(1),
            },
        );
        rules.insert(
            "m-middle".to_string(),
            OwnerRule {
                pattern: "*.middle".to_string(),
                owners: vec!["@team".to_string()],
                description: None,
                section: None,
                order: Some(2),
            },
        );

        let owners = Owners {
            rules,
            ..Default::default()
        };

        let sorted = owners.sorted_rules();
        assert_eq!(sorted.len(), 3);
        assert_eq!(sorted[0].0, "a-first");
        assert_eq!(sorted[1].0, "m-middle");
        assert_eq!(sorted[2].0, "z-last");
    }

    #[test]
    fn test_owners_header() {
        // No header
        let owners = Owners::default();
        assert!(owners.header().is_none());

        // With header
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: None,
                path: None,
                header: Some("Custom Header".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(owners.header(), Some("Custom Header"));
    }

    #[test]
    fn test_owners_custom_path() {
        // No custom path
        let owners = Owners::default();
        assert!(owners.custom_path().is_none());

        // With custom path
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: None,
                path: Some("docs/CODEOWNERS".to_string()),
                header: None,
            }),
            ..Default::default()
        };
        assert_eq!(owners.custom_path(), Some("docs/CODEOWNERS"));
    }

    #[test]
    fn test_owners_output_platform_string() {
        // Platform can be set as a string hint
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some("gitlab".to_string()),
                path: None,
                header: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            owners.output.as_ref().unwrap().platform,
            Some("gitlab".to_string())
        );
    }
}
