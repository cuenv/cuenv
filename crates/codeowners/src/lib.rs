//! Generate CODEOWNERS files for GitHub, GitLab, and Bitbucket.
//!
//! This crate provides a builder-based API for generating CODEOWNERS files
//! that define code ownership rules for your repository.
//!
//! # Example
//!
//! ```rust
//! use cuenv_codeowners::{CodeOwners, Platform, Rule};
//!
//! let codeowners = CodeOwners::builder()
//!     .platform(Platform::Github)
//!     .rule(Rule::new("*", ["@org/core-team"]))  // Catch-all rule
//!     .rule(Rule::new("*.rs", ["@rust-team"]))
//!     .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
//!     .build();
//!
//! let content = codeowners.generate();
//! // Write to .github/CODEOWNERS or wherever appropriate
//! ```
//!
//! # Platform Support
//!
//! - **GitHub**: Uses `.github/CODEOWNERS` path and `# Section` comment syntax
//! - **GitLab**: Uses `CODEOWNERS` path and `[Section]` syntax for sections
//! - **Bitbucket**: Uses `CODEOWNERS` path and `# Section` comment syntax
//!
//! # Provider Support
//!
//! The [`provider`] module provides a trait-based abstraction for syncing
//! CODEOWNERS files. Use [`provider::detect_provider`] to auto-detect the
//! platform and get the appropriate provider.
//!
//! # Features
//!
//! - `serde`: Enable serde serialization/deserialization for all types
//! - `schemars`: Enable JSON Schema generation (implies `serde`)

#![warn(missing_docs)]

pub mod provider;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

#[cfg(feature = "schemars")]
use schemars::JsonSchema;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Target platform for CODEOWNERS file generation.
///
/// Different platforms have different default paths and section syntax:
/// - GitHub: `.github/CODEOWNERS`, sections as `# Section Name`
/// - GitLab: `CODEOWNERS`, sections as `[Section Name]`
/// - Bitbucket: `CODEOWNERS`, sections as `# Section Name`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum Platform {
    /// GitHub - uses `.github/CODEOWNERS` and `# Section` comments
    #[default]
    Github,
    /// GitLab - uses `CODEOWNERS` and `[Section]` syntax
    Gitlab,
    /// Bitbucket - uses `CODEOWNERS` and `# Section` comments
    Bitbucket,
}

impl Platform {
    /// Get the default path for CODEOWNERS file on this platform.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cuenv_codeowners::Platform;
    ///
    /// assert_eq!(Platform::Github.default_path(), ".github/CODEOWNERS");
    /// assert_eq!(Platform::Gitlab.default_path(), "CODEOWNERS");
    /// ```
    #[must_use]
    pub fn default_path(&self) -> &'static str {
        match self {
            Platform::Github => ".github/CODEOWNERS",
            Platform::Gitlab | Platform::Bitbucket => "CODEOWNERS",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Github => write!(f, "github"),
            Platform::Gitlab => write!(f, "gitlab"),
            Platform::Bitbucket => write!(f, "bitbucket"),
        }
    }
}

/// Conversion from manifest Platform type.
///
/// This is gated behind the `manifest` feature flag.
#[cfg(feature = "manifest")]
impl From<cuenv_core::owners::Platform> for Platform {
    fn from(p: cuenv_core::owners::Platform) -> Self {
        match p {
            cuenv_core::owners::Platform::Github => Self::Github,
            cuenv_core::owners::Platform::Gitlab => Self::Gitlab,
            cuenv_core::owners::Platform::Bitbucket => Self::Bitbucket,
        }
    }
}

/// A single code ownership rule.
///
/// Each rule maps a file pattern to one or more owners.
///
/// # Example
///
/// ```rust
/// use cuenv_codeowners::Rule;
///
/// let rule = Rule::new("*.rs", ["@rust-team", "@backend"])
///     .description("Rust source files")
///     .section("Backend");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct Rule {
    /// File pattern (glob syntax) matching files this rule applies to.
    pub pattern: String,
    /// List of owners for files matching this pattern.
    pub owners: Vec<String>,
    /// Optional description added as a comment above the rule.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub description: Option<String>,
    /// Optional section name for grouping rules in the output.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub section: Option<String>,
}

impl Rule {
    /// Create a new rule with a pattern and owners.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cuenv_codeowners::Rule;
    ///
    /// let rule = Rule::new("*.rs", ["@rust-team"]);
    /// let rule = Rule::new("/docs/**", vec!["@docs-team", "@tech-writers"]);
    /// ```
    pub fn new(
        pattern: impl Into<String>,
        owners: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            pattern: pattern.into(),
            owners: owners.into_iter().map(Into::into).collect(),
            description: None,
            section: None,
        }
    }

    /// Add a description to this rule.
    ///
    /// The description will be added as a comment above the rule in the output.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Assign this rule to a section.
    ///
    /// Rules with the same section will be grouped together in the output.
    #[must_use]
    pub fn section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
        self
    }
}

/// CODEOWNERS file configuration and generator.
///
/// Use [`CodeOwners::builder()`] to create a new instance.
///
/// # Example
///
/// ```rust
/// use cuenv_codeowners::{CodeOwners, Platform, Rule};
///
/// let codeowners = CodeOwners::builder()
///     .platform(Platform::Github)
///     .header("Custom header comment")
///     .rule(Rule::new("*", ["@org/maintainers"]))  // Catch-all rule
///     .rule(Rule::new("*.rs", ["@rust-team"]))
///     .build();
///
/// println!("{}", codeowners.generate());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct CodeOwners {
    /// Target platform for the CODEOWNERS file.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub platform: Option<Platform>,
    /// Custom output path override.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub path: Option<String>,
    /// Custom header comment for the file.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub header: Option<String>,
    /// Ownership rules.
    #[cfg_attr(feature = "serde", serde(default))]
    pub rules: Vec<Rule>,
}

impl CodeOwners {
    /// Create a new builder for constructing a CodeOwners configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cuenv_codeowners::CodeOwners;
    ///
    /// let codeowners = CodeOwners::builder()
    ///     .rule(cuenv_codeowners::Rule::new("*", ["@fallback-team"]))
    ///     .build();
    /// ```
    #[must_use]
    pub fn builder() -> CodeOwnersBuilder {
        CodeOwnersBuilder::default()
    }

    /// Generate the CODEOWNERS file content.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cuenv_codeowners::{CodeOwners, Rule};
    ///
    /// let codeowners = CodeOwners::builder()
    ///     .rule(Rule::new("*.rs", ["@rust-team"]))
    ///     .build();
    ///
    /// let content = codeowners.generate();
    /// assert!(content.contains("*.rs @rust-team"));
    /// ```
    #[must_use]
    pub fn generate(&self) -> String {
        let mut output = String::new();
        let platform = self.platform.unwrap_or_default();

        // Add header
        if let Some(ref header) = self.header {
            for line in header.lines() {
                output.push_str("# ");
                output.push_str(line);
                output.push('\n');
            }
            output.push('\n');
        }

        // Group rules by section for contiguous output
        let mut rules_by_section: BTreeMap<Option<&str>, Vec<&Rule>> = BTreeMap::new();
        for rule in &self.rules {
            rules_by_section
                .entry(rule.section.as_deref())
                .or_default()
                .push(rule);
        }

        let mut first_section = true;
        for (section, rules) in rules_by_section {
            if !first_section {
                output.push('\n');
            }
            first_section = false;

            // Write section header if present
            if let Some(section_name) = section {
                match platform {
                    Platform::Gitlab => {
                        output.push('[');
                        output.push_str(section_name);
                        output.push_str("]\n");
                    }
                    Platform::Github | Platform::Bitbucket => {
                        output.push_str("# ");
                        output.push_str(section_name);
                        output.push('\n');
                    }
                }
            }

            // Write all rules in this section
            for rule in rules {
                if let Some(ref description) = rule.description {
                    output.push_str("# ");
                    output.push_str(description);
                    output.push('\n');
                }

                output.push_str(&rule.pattern);
                output.push(' ');
                output.push_str(&rule.owners.join(" "));
                output.push('\n');
            }
        }

        output
    }

    /// Get the output path for the CODEOWNERS file.
    ///
    /// Returns the custom path if set, otherwise the platform's default path.
    #[must_use]
    pub fn output_path(&self) -> &str {
        self.path
            .as_deref()
            .unwrap_or_else(|| self.platform.unwrap_or_default().default_path())
    }

    /// Detect the platform from repository structure.
    ///
    /// Checks for platform-specific files/directories:
    /// - `.github/` directory -> GitHub
    /// - `.gitlab-ci.yml` file -> GitLab
    /// - `bitbucket-pipelines.yml` file -> Bitbucket
    /// - Falls back to GitHub if none detected
    ///
    /// # Example
    ///
    /// ```rust
    /// use cuenv_codeowners::{CodeOwners, Platform};
    /// use std::path::Path;
    ///
    /// let platform = CodeOwners::detect_platform(Path::new("."));
    /// ```
    #[must_use]
    pub fn detect_platform(repo_root: &Path) -> Platform {
        if repo_root.join(".github").is_dir() {
            Platform::Github
        } else if repo_root.join(".gitlab-ci.yml").exists() {
            Platform::Gitlab
        } else if repo_root.join("bitbucket-pipelines.yml").exists() {
            Platform::Bitbucket
        } else {
            Platform::Github
        }
    }
}

/// Builder for [`CodeOwners`].
///
/// # Example
///
/// ```rust
/// use cuenv_codeowners::{CodeOwners, Platform, Rule};
///
/// let codeowners = CodeOwners::builder()
///     .platform(Platform::Github)
///     .header("Code ownership rules")
///     .rule(Rule::new("*", ["@org/maintainers"]))  // Catch-all rule
///     .rule(Rule::new("*.rs", ["@rust-team"]))
///     .rules([
///         Rule::new("/docs/**", ["@docs-team"]),
///         Rule::new("/ci/**", ["@devops"]),
///     ])
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct CodeOwnersBuilder {
    platform: Option<Platform>,
    path: Option<String>,
    header: Option<String>,
    rules: Vec<Rule>,
}

impl CodeOwnersBuilder {
    /// Set the target platform.
    #[must_use]
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    /// Set a custom output path.
    ///
    /// Overrides the platform's default path.
    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set a custom header comment.
    ///
    /// The header will be added at the top of the file with `#` prefixes.
    #[must_use]
    pub fn header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    /// Add a single rule.
    #[must_use]
    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Add multiple rules.
    #[must_use]
    pub fn rules(mut self, rules: impl IntoIterator<Item = Rule>) -> Self {
        self.rules.extend(rules);
        self
    }

    /// Build the [`CodeOwners`] configuration.
    #[must_use]
    pub fn build(self) -> CodeOwners {
        CodeOwners {
            platform: self.platform,
            path: self.path,
            header: self.header,
            rules: self.rules,
        }
    }
}

/// Default cuenv header for generated CODEOWNERS files.
pub const DEFAULT_CUENV_HEADER: &str = "CODEOWNERS file - Generated by cuenv\nDo not edit manually. Configure in env.cue and run `cuenv owners sync`";

/// Conversion from manifest Owners type.
///
/// This is gated behind the `manifest` feature flag.
#[cfg(feature = "manifest")]
impl From<&cuenv_core::owners::Owners> for CodeOwners {
    fn from(owners: &cuenv_core::owners::Owners) -> Self {
        let mut builder = CodeOwnersBuilder::default();

        // Set platform
        builder = builder.platform(owners.platform().into());

        // Set custom path if provided
        if let Some(ref output) = owners.output
            && let Some(ref path) = output.path
        {
            builder = builder.path(path.clone());
        }

        // Set header (use custom or default cuenv header)
        let header = owners
            .header()
            .map(String::from)
            .unwrap_or_else(|| DEFAULT_CUENV_HEADER.to_string());
        builder = builder.header(header);

        // Add rules in sorted order
        for (_key, rule) in owners.sorted_rules() {
            let mut lib_rule = Rule::new(&rule.pattern, rule.owners.clone());
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
    fn test_rule_builder() {
        let rule = Rule::new("*.rs", ["@rust-team"])
            .description("Rust files")
            .section("Backend");

        assert_eq!(rule.pattern, "*.rs");
        assert_eq!(rule.owners, vec!["@rust-team"]);
        assert_eq!(rule.description, Some("Rust files".to_string()));
        assert_eq!(rule.section, Some("Backend".to_string()));
    }

    #[test]
    fn test_codeowners_output_path() {
        // Default (no config)
        let codeowners = CodeOwners::builder().build();
        assert_eq!(codeowners.output_path(), ".github/CODEOWNERS");

        // With platform
        let codeowners = CodeOwners::builder().platform(Platform::Gitlab).build();
        assert_eq!(codeowners.output_path(), "CODEOWNERS");

        // With custom path
        let codeowners = CodeOwners::builder()
            .platform(Platform::Github)
            .path("docs/CODEOWNERS")
            .build();
        assert_eq!(codeowners.output_path(), "docs/CODEOWNERS");
    }

    #[test]
    fn test_generate_simple() {
        let codeowners = CodeOwners::builder()
            .rule(Rule::new("*.rs", ["@rust-team"]))
            .rule(Rule::new("/docs/**", ["@docs-team", "@tech-writers"]))
            .build();

        let content = codeowners.generate();
        assert!(content.contains("*.rs @rust-team"));
        assert!(content.contains("/docs/** @docs-team @tech-writers"));
    }

    #[test]
    fn test_generate_with_sections() {
        let codeowners = CodeOwners::builder()
            .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
            .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
            .build();

        let content = codeowners.generate();
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Frontend"));
    }

    #[test]
    fn test_generate_with_custom_header() {
        let codeowners = CodeOwners::builder()
            .header("Custom Header\nLine 2")
            .build();

        let content = codeowners.generate();
        assert!(content.contains("# Custom Header"));
        assert!(content.contains("# Line 2"));
    }

    #[test]
    fn test_generate_with_descriptions() {
        let codeowners = CodeOwners::builder()
            .rule(Rule::new("*.rs", ["@rust-team"]).description("Rust source files"))
            .build();

        let content = codeowners.generate();
        assert!(content.contains("# Rust source files"));
        assert!(content.contains("*.rs @rust-team"));
    }

    #[test]
    fn test_generate_gitlab_sections() {
        let codeowners = CodeOwners::builder()
            .platform(Platform::Gitlab)
            .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
            .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
            .build();

        let content = codeowners.generate();
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
        // Rules with same section should be grouped even if not contiguous in input
        let codeowners = CodeOwners::builder()
            .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
            .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
            .rule(Rule::new("*.go", ["@backend"]).section("Backend"))
            .build();

        let content = codeowners.generate();

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

        assert!(
            rs_idx > backend_idx && rs_idx < frontend_idx,
            "*.rs should be in Backend section"
        );
        assert!(
            go_idx > backend_idx && go_idx < frontend_idx,
            "*.go should be in Backend section"
        );
    }

    #[test]
    fn test_builder_chaining() {
        let codeowners = CodeOwners::builder()
            .platform(Platform::Github)
            .path(".github/CODEOWNERS")
            .header("Code ownership")
            .rule(Rule::new("*.rs", ["@rust"]))
            .rules([
                Rule::new("*.ts", ["@typescript"]),
                Rule::new("*.py", ["@python"]),
            ])
            .build();

        assert_eq!(codeowners.platform, Some(Platform::Github));
        assert_eq!(codeowners.path, Some(".github/CODEOWNERS".to_string()));
        assert_eq!(codeowners.header, Some("Code ownership".to_string()));
        assert_eq!(codeowners.rules.len(), 3);
    }
}
