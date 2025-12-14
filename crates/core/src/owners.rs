//! Code ownership configuration types for cuenv manifests.
//!
//! This module provides serde-compatible types for deserializing CODEOWNERS
//! configuration from CUE manifests. It wraps the `cuenv_codeowners` library
//! which provides the actual generation logic.
//!
//! Based on schema/owners.cue

use cuenv_codeowners::{Codeowners, CodeownersBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

// Re-export the library's Platform type for convenience, but also define our own
// for serde/schemars compatibility in manifests.
pub use cuenv_codeowners::Platform as LibPlatform;

/// Platform for CODEOWNERS file generation.
///
/// This is the manifest-compatible version with serde/schemars derives.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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
        self.to_lib().default_path()
    }

    /// Convert to the library's Platform type.
    #[must_use]
    pub fn to_lib(self) -> LibPlatform {
        match self {
            Self::Github => LibPlatform::Github,
            Self::Gitlab => LibPlatform::Gitlab,
            Self::Bitbucket => LibPlatform::Bitbucket,
        }
    }

    /// Convert from the library's Platform type.
    #[must_use]
    pub fn from_lib(platform: LibPlatform) -> Self {
        match platform {
            LibPlatform::Github => Self::Github,
            LibPlatform::Gitlab => Self::Gitlab,
            LibPlatform::Bitbucket => Self::Bitbucket,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_lib())
    }
}

/// Output configuration for CODEOWNERS file generation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
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
}

/// Code ownership configuration for a project.
///
/// This type is designed for deserializing from CUE manifests. Use
/// [`to_codeowners()`](Self::to_codeowners) to convert to the library type
/// for generation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Owners {
    /// Output configuration for CODEOWNERS file generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OwnersOutput>,

    /// Global default owners applied to all patterns without explicit owners.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_owners: Option<Vec<String>>,

    /// Code ownership rules - maps patterns to owners.
    #[serde(default)]
    pub rules: Vec<OwnerRule>,
}

impl Owners {
    /// Convert to the library's [`Codeowners`] type for generation.
    ///
    /// This method converts the manifest configuration to the library type,
    /// adding a default cuenv header if none is specified.
    #[must_use]
    pub fn to_codeowners(&self) -> Codeowners {
        let mut builder = CodeownersBuilder::default();

        // Set platform and path from output config
        if let Some(ref output) = self.output {
            if let Some(platform) = output.platform {
                builder = builder.platform(platform.to_lib());
            }
            if let Some(ref path) = output.path {
                builder = builder.path(path.clone());
            }
            if let Some(ref header) = output.header {
                builder = builder.header(header.clone());
            } else {
                // Default cuenv header
                builder = builder.header(
                    "CODEOWNERS file - Generated by cuenv\n\
                     Do not edit manually. Configure in env.cue and run `cuenv owners sync`",
                );
            }
        } else {
            // Default cuenv header when no output config
            builder = builder.header(
                "CODEOWNERS file - Generated by cuenv\n\
                 Do not edit manually. Configure in env.cue and run `cuenv owners sync`",
            );
        }

        // Set default owners
        if let Some(ref default_owners) = self.default_owners {
            builder = builder.default_owners(default_owners.clone());
        }

        // Add rules
        for rule in &self.rules {
            let mut lib_rule = cuenv_codeowners::Rule::new(&rule.pattern, rule.owners.clone());
            if let Some(ref description) = rule.description {
                lib_rule = lib_rule.description(description.clone());
            }
            if let Some(ref section) = rule.section {
                lib_rule = lib_rule.section(section.clone());
            }
            builder = builder.rule(lib_rule);
        }

        builder.build()
    }

    /// Generate the CODEOWNERS file content.
    ///
    /// This is a convenience method that converts to [`Codeowners`] and
    /// calls `generate()`.
    #[must_use]
    pub fn generate(&self) -> String {
        self.to_codeowners().generate()
    }

    /// Get the output path for the CODEOWNERS file.
    #[must_use]
    pub fn output_path(&self) -> &str {
        self.output
            .as_ref()
            .map(|o| o.output_path())
            .unwrap_or_else(|| Platform::default().default_path())
    }

    /// Detect platform from repository structure.
    ///
    /// Delegates to the library's detection logic.
    #[must_use]
    pub fn detect_platform(repo_root: &Path) -> Platform {
        Platform::from_lib(Codeowners::detect_platform(repo_root))
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
    fn test_generate_simple() {
        let owners = Owners {
            rules: vec![
                OwnerRule {
                    pattern: "*.rs".to_string(),
                    owners: vec!["@rust-team".to_string()],
                    description: None,
                    section: None,
                },
                OwnerRule {
                    pattern: "/docs/**".to_string(),
                    owners: vec!["@docs-team".to_string(), "@tech-writers".to_string()],
                    description: None,
                    section: None,
                },
            ],
            ..Default::default()
        };

        let content = owners.generate();
        assert!(content.contains("*.rs @rust-team"));
        assert!(content.contains("/docs/** @docs-team @tech-writers"));
    }

    #[test]
    fn test_generate_with_sections() {
        let owners = Owners {
            rules: vec![
                OwnerRule {
                    pattern: "*.rs".to_string(),
                    owners: vec!["@backend".to_string()],
                    description: Some("Rust source files".to_string()),
                    section: Some("Backend".to_string()),
                },
                OwnerRule {
                    pattern: "*.ts".to_string(),
                    owners: vec!["@frontend".to_string()],
                    description: None,
                    section: Some("Frontend".to_string()),
                },
            ],
            ..Default::default()
        };

        let content = owners.generate();
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Rust source files"));
        assert!(content.contains("# Frontend"));
    }

    #[test]
    fn test_generate_with_default_owners() {
        let owners = Owners {
            default_owners: Some(vec!["@core-team".to_string()]),
            rules: vec![OwnerRule {
                pattern: "/security/**".to_string(),
                owners: vec!["@security-team".to_string()],
                description: None,
                section: None,
            }],
            ..Default::default()
        };

        let content = owners.generate();
        assert!(content.contains("* @core-team"));
        assert!(content.contains("/security/** @security-team"));
    }

    #[test]
    fn test_generate_with_custom_header() {
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: None,
                path: None,
                header: Some("Custom Header\nLine 2".to_string()),
            }),
            rules: vec![],
            ..Default::default()
        };

        let content = owners.generate();
        assert!(content.contains("# Custom Header"));
        assert!(content.contains("# Line 2"));
    }

    #[test]
    fn test_generate_gitlab_sections() {
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Gitlab),
                path: None,
                header: None,
            }),
            rules: vec![
                OwnerRule {
                    pattern: "*.rs".to_string(),
                    owners: vec!["@backend".to_string()],
                    section: Some("Backend".to_string()),
                    description: None,
                },
                OwnerRule {
                    pattern: "*.ts".to_string(),
                    owners: vec!["@frontend".to_string()],
                    section: Some("Frontend".to_string()),
                    description: None,
                },
            ],
            ..Default::default()
        };

        let content = owners.generate();
        // GitLab uses [Section] syntax
        assert!(
            content.contains("[Backend]"),
            "GitLab should use [Section] syntax, got: {content}"
        );
        assert!(
            content.contains("[Frontend]"),
            "GitLab should use [Section] syntax, got: {content}"
        );
        // Should NOT use comment-style sections
        assert!(
            !content.contains("# Backend"),
            "GitLab should NOT use # Section"
        );
        assert!(
            !content.contains("# Frontend"),
            "GitLab should NOT use # Section"
        );
    }

    #[test]
    fn test_generate_groups_rules_by_section() {
        // Test that rules with same section are grouped together even if not contiguous in input
        let owners = Owners {
            rules: vec![
                OwnerRule {
                    pattern: "*.rs".to_string(),
                    owners: vec!["@backend".to_string()],
                    section: Some("Backend".to_string()),
                    description: None,
                },
                OwnerRule {
                    pattern: "*.ts".to_string(),
                    owners: vec!["@frontend".to_string()],
                    section: Some("Frontend".to_string()),
                    description: None,
                },
                OwnerRule {
                    pattern: "*.go".to_string(),
                    owners: vec!["@backend".to_string()],
                    section: Some("Backend".to_string()),
                    description: None,
                },
            ],
            ..Default::default()
        };

        let content = owners.generate();
        // Backend section should only appear once
        let backend_count = content.matches("# Backend").count();
        assert_eq!(
            backend_count, 1,
            "Backend section should appear exactly once, found {backend_count} times"
        );
        // Both backend rules should be together
        let backend_idx = content.find("# Backend").unwrap();
        let rs_idx = content.find("*.rs").unwrap();
        let go_idx = content.find("*.go").unwrap();
        let frontend_idx = content.find("# Frontend").unwrap();
        // Both .rs and .go should come after Backend header and before Frontend header
        assert!(
            rs_idx > backend_idx && rs_idx < frontend_idx,
            "*.rs should be in Backend section"
        );
        assert!(
            go_idx > backend_idx && go_idx < frontend_idx,
            "*.go should be in Backend section"
        );
    }
}
