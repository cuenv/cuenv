//! Generate CODEOWNERS files with configurable formatting.
//!
//! This crate provides a builder-based API for generating CODEOWNERS files
//! that define code ownership rules for your repository. It is provider-agnostic;
//! platform-specific logic (paths, section styles) belongs in provider crates
//! like `cuenv-github` or `cuenv-gitlab`.
//!
//! # Example
//!
//! ```rust
//! use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};
//!
//! let codeowners = CodeOwners::builder()
//!     .section_style(SectionStyle::Comment)  // GitHub/Bitbucket style
//!     .rule(Rule::new("*", ["@org/core-team"]))
//!     .rule(Rule::new("*.rs", ["@rust-team"]))
//!     .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
//!     .build();
//!
//! let content = codeowners.generate();
//! ```
//!
//! # Section Styles
//!
//! - `Comment`: `# Section Name` (used by GitHub, Bitbucket)
//! - `Bracket`: `[Section Name]` (used by GitLab)
//! - `None`: No section headers
//!
//! # Provider Support
//!
//! The [`provider`] module provides a trait-based abstraction for syncing
//! CODEOWNERS files. Provider implementations live in platform crates.
//!
//! # Features
//!
//! - `serde`: Enable serde serialization/deserialization for all types
//! - `schemars`: Enable JSON Schema generation (implies `serde`)

#![warn(missing_docs)]

pub mod provider;

use std::collections::BTreeMap;
use std::fmt;

#[cfg(feature = "schemars")]
use schemars::JsonSchema;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Section formatting style for CODEOWNERS files.
///
/// Different platforms use different syntax for section headers:
/// - GitHub/Bitbucket: `# Section Name` (comment style)
/// - GitLab: `[Section Name]` (bracket style)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum SectionStyle {
    /// `# Section Name` - comment-based sections (GitHub, Bitbucket)
    #[default]
    Comment,
    /// `[Section Name]` - bracket-based sections (GitLab)
    Bracket,
    /// No section headers in output
    None,
}

impl fmt::Display for SectionStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SectionStyle::Comment => write!(f, "comment"),
            SectionStyle::Bracket => write!(f, "bracket"),
            SectionStyle::None => write!(f, "none"),
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
/// use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};
///
/// let codeowners = CodeOwners::builder()
///     .section_style(SectionStyle::Comment)
///     .header("Custom header comment")
///     .rule(Rule::new("*", ["@org/maintainers"]))
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
    /// Section formatting style.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub section_style: Option<SectionStyle>,
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
        let section_style = self.section_style.unwrap_or_default();

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
                match section_style {
                    SectionStyle::Bracket => {
                        output.push('[');
                        output.push_str(section_name);
                        output.push_str("]\n");
                    }
                    SectionStyle::Comment => {
                        output.push_str("# ");
                        output.push_str(section_name);
                        output.push('\n');
                    }
                    SectionStyle::None => {
                        // No section header
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
}

/// Builder for [`CodeOwners`].
///
/// # Example
///
/// ```rust
/// use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};
///
/// let codeowners = CodeOwners::builder()
///     .section_style(SectionStyle::Comment)
///     .header("Code ownership rules")
///     .rule(Rule::new("*", ["@org/maintainers"]))
///     .rule(Rule::new("*.rs", ["@rust-team"]))
///     .rules([
///         Rule::new("/docs/**", ["@docs-team"]),
///         Rule::new("/ci/**", ["@devops"]),
///     ])
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct CodeOwnersBuilder {
    section_style: Option<SectionStyle>,
    header: Option<String>,
    rules: Vec<Rule>,
}

impl CodeOwnersBuilder {
    /// Set the section formatting style.
    #[must_use]
    pub fn section_style(mut self, style: SectionStyle) -> Self {
        self.section_style = Some(style);
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
            section_style: self.section_style,
            header: self.header,
            rules: self.rules,
        }
    }
}

/// Default cuenv header for generated CODEOWNERS files.
pub const DEFAULT_CUENV_HEADER: &str = "CODEOWNERS file - Generated by cuenv\nDo not edit manually. Configure in env.cue and run `cuenv owners sync`";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_section_style_display() {
        assert_eq!(SectionStyle::Comment.to_string(), "comment");
        assert_eq!(SectionStyle::Bracket.to_string(), "bracket");
        assert_eq!(SectionStyle::None.to_string(), "none");
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
    fn test_generate_with_sections_comment_style() {
        let codeowners = CodeOwners::builder()
            .section_style(SectionStyle::Comment)
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
    fn test_generate_bracket_sections() {
        let codeowners = CodeOwners::builder()
            .section_style(SectionStyle::Bracket)
            .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
            .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
            .build();

        let content = codeowners.generate();
        // Bracket style uses [Section] syntax
        assert!(
            content.contains("[Backend]"),
            "Bracket style should use [Section] syntax, got: {content}"
        );
        assert!(
            content.contains("[Frontend]"),
            "Bracket style should use [Section] syntax, got: {content}"
        );
        // Should NOT use comment-style sections
        assert!(
            !content.contains("# Backend"),
            "Bracket style should NOT use # Section"
        );
        assert!(
            !content.contains("# Frontend"),
            "Bracket style should NOT use # Section"
        );
    }

    #[test]
    fn test_generate_no_section_headers() {
        let codeowners = CodeOwners::builder()
            .section_style(SectionStyle::None)
            .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
            .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
            .build();

        let content = codeowners.generate();
        // No section headers should appear
        assert!(
            !content.contains("Backend"),
            "SectionStyle::None should not include section headers"
        );
        assert!(
            !content.contains("Frontend"),
            "SectionStyle::None should not include section headers"
        );
        // Rules should still be present
        assert!(content.contains("*.rs @backend"));
        assert!(content.contains("*.ts @frontend"));
    }

    #[test]
    fn test_generate_groups_rules_by_section() {
        // Rules with same section should be grouped even if not contiguous in input
        let codeowners = CodeOwners::builder()
            .section_style(SectionStyle::Comment)
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
            .section_style(SectionStyle::Comment)
            .header("Code ownership")
            .rule(Rule::new("*.rs", ["@rust"]))
            .rules([
                Rule::new("*.ts", ["@typescript"]),
                Rule::new("*.py", ["@python"]),
            ])
            .build();

        assert_eq!(codeowners.section_style, Some(SectionStyle::Comment));
        assert_eq!(codeowners.header, Some("Code ownership".to_string()));
        assert_eq!(codeowners.rules.len(), 3);
    }
}
