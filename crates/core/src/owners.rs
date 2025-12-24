//! Code ownership configuration types for cuenv manifests.
//!
//! This module provides serde-compatible types for deserializing CODEOWNERS
//! configuration from CUE manifests.
//!
//! Based on schema/owners.cue

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Platform for CODEOWNERS file generation.
///
/// This is the manifest-compatible version with serde/schemars derives.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// GitHub - uses `.github/CODEOWNERS`
    #[default]
    Github,
    /// GitLab - uses `CODEOWNERS` with `[Section]` syntax
    Gitlab,
    /// Bitbucket - uses `CODEOWNERS`
    Bitbucket,
}

impl Platform {
    /// Get the default path for CODEOWNERS file on this platform.
    #[must_use]
    pub fn default_path(&self) -> &'static str {
        match self {
            Self::Github => ".github/CODEOWNERS",
            Self::Gitlab | Self::Bitbucket => "CODEOWNERS",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Github => write!(f, "github"),
            Self::Gitlab => write!(f, "gitlab"),
            Self::Bitbucket => write!(f, "bitbucket"),
        }
    }
}

/// Output configuration for CODEOWNERS file generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OwnersOutput {
    /// Platform to generate CODEOWNERS for.
    pub platform: Option<Platform>,

    /// Custom path for CODEOWNERS file (overrides platform default).
    pub path: Option<String>,

    /// Header comment to include at the top of the generated file.
    pub header: Option<String>,
}

impl OwnersOutput {
    /// Get the output path for the CODEOWNERS file.
    #[must_use]
    pub fn output_path(&self) -> &str {
        if let Some(ref path) = self.path {
            path
        } else {
            self.platform.unwrap_or_default().default_path()
        }
    }
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
    /// Get the output path for the CODEOWNERS file.
    #[must_use]
    pub fn output_path(&self) -> &str {
        self.output
            .as_ref()
            .map(OwnersOutput::output_path)
            .unwrap_or_else(|| Platform::default().default_path())
    }

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

    /// Get the platform, defaulting to GitHub if not specified.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.output
            .as_ref()
            .and_then(|o| o.platform)
            .unwrap_or_default()
    }

    /// Get the header, if any.
    #[must_use]
    pub fn header(&self) -> Option<&str> {
        self.output.as_ref().and_then(|o| o.header.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_default_paths() {
        assert_eq!(Platform::Github.default_path(), ".github/CODEOWNERS");
        assert_eq!(Platform::Gitlab.default_path(), "CODEOWNERS");
        assert_eq!(Platform::Bitbucket.default_path(), "CODEOWNERS");
    }

    #[test]
    fn test_platform_display() {
        assert_eq!(Platform::Github.to_string(), "github");
        assert_eq!(Platform::Gitlab.to_string(), "gitlab");
        assert_eq!(Platform::Bitbucket.to_string(), "bitbucket");
    }

    #[test]
    fn test_owners_output_path() {
        // Default (no output config)
        let owners = Owners::default();
        assert_eq!(owners.output_path(), ".github/CODEOWNERS");

        // With platform specified
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Gitlab),
                path: None,
                header: None,
            }),
            ..Default::default()
        };
        assert_eq!(owners.output_path(), "CODEOWNERS");

        // With custom path
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Github),
                path: Some("docs/CODEOWNERS".to_string()),
                header: None,
            }),
            ..Default::default()
        };
        assert_eq!(owners.output_path(), "docs/CODEOWNERS");
    }

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
    fn test_owners_platform() {
        // Default
        let owners = Owners::default();
        assert_eq!(owners.platform(), Platform::Github);

        // With platform
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Gitlab),
                path: None,
                header: None,
            }),
            ..Default::default()
        };
        assert_eq!(owners.platform(), Platform::Gitlab);
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
}
