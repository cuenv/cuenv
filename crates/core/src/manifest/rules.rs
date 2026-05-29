use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Ignore patterns for tool-specific ignore files.
/// Keys are tool names (e.g., "git", "docker", "prettier").
/// Values can be either:
/// - A list of patterns: `["node_modules/", ".env"]`
/// - An object with patterns and optional filename override
pub type Ignore = HashMap<String, IgnoreValue>;
/// Value for an ignore entry - either a simple list of patterns or an extended config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum IgnoreValue {
    /// Simple list of patterns
    Patterns(Vec<String>),
    /// Extended config with patterns and optional filename override
    Extended(IgnoreEntry),
}

/// Extended ignore configuration with patterns and optional filename override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IgnoreEntry {
    /// List of patterns to include in the ignore file
    pub patterns: Vec<String>,
    /// Optional filename override (defaults to `.{tool}ignore`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

impl IgnoreValue {
    /// Get the patterns from this ignore value.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        match self {
            Self::Patterns(patterns) => patterns,
            Self::Extended(entry) => &entry.patterns,
        }
    }

    /// Get the optional filename override.
    #[must_use]
    pub fn filename(&self) -> Option<&str> {
        match self {
            Self::Patterns(_) => None,
            Self::Extended(entry) => entry.filename.as_deref(),
        }
    }
}

// ============================================================================
// Directory Rules Types (for .rules.cue files)
// ============================================================================

/// Directory-scoped rules configuration from .rules.cue files.
///
/// Each .rules.cue file is evaluated independently (no CUE unification).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct DirectoryRules {
    /// Ignore patterns for tool-specific ignore files.
    /// Generates files in the same directory as .rules.cue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Ignore>,

    /// Code ownership rules.
    /// Aggregated across all .rules.cue files to generate
    /// a single CODEOWNERS file at the repository root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owners: Option<RulesOwners>,

    /// EditorConfig settings.
    /// Generates .editorconfig in the same directory as .rules.cue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editorconfig: Option<EditorConfig>,
}

/// Simplified owners for directory rules (no output config).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RulesOwners {
    /// Code ownership rules - maps rule names to rule definitions.
    #[serde(default)]
    pub rules: HashMap<String, crate::owners::OwnerRule>,
}

/// EditorConfig configuration.
///
/// Note: `root = true` is auto-injected for the .editorconfig at repo root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EditorConfig {
    /// File-pattern specific settings.
    ///
    /// Unlike the other rules types, this struct intentionally omits
    /// `deny_unknown_fields`: the flattened map keys are arbitrary section
    /// globs (e.g. `*`, `*.rs`), so every key is a valid section name. Unknown
    /// *field* rejection happens one level down on [`EditorConfigSection`].
    #[serde(flatten)]
    pub sections: std::collections::BTreeMap<String, EditorConfigSection>,
}

/// A section in an EditorConfig file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct EditorConfigSection {
    /// Indentation style: "tab" or "space"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_style: Option<String>,

    /// Number of columns for each indentation level, or "tab"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_size: Option<EditorConfigValue>,

    /// Number of columns for tab character display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_width: Option<u32>,

    /// Line ending style: "lf", "crlf", or "cr"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_of_line: Option<String>,

    /// Character encoding
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charset: Option<String>,

    /// Remove trailing whitespace on save
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trim_trailing_whitespace: Option<bool>,

    /// Ensure file ends with a newline
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_final_newline: Option<bool>,

    /// Maximum line length (soft limit), or "off"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_line_length: Option<EditorConfigValue>,
}

/// A value that can be either an integer or a special string value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EditorConfigValue {
    /// Integer value
    Int(u32),
    /// String value (e.g., "tab" for indent_size, "off" for max_line_length)
    String(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_rules_rejects_unknown_top_level_fields() {
        let err = serde_json::from_value::<DirectoryRules>(serde_json::json!({
            "ignore": {
                "git": ["target/"]
            },
            "unexpected": true
        }))
        .expect_err("unknown top-level rules field should fail");

        assert!(
            err.to_string().contains("unknown field `unexpected`"),
            "expected unknown field error, got: {err}"
        );
    }

    #[test]
    fn ignore_entry_rejects_unknown_fields() {
        let err = serde_json::from_value::<IgnoreValue>(serde_json::json!({
            "patterns": ["target/"],
            "path": ".gitignore"
        }))
        .expect_err("unknown ignore entry field should fail");

        assert!(
            err.to_string().contains("data did not match any variant"),
            "expected untagged enum rejection, got: {err}"
        );
    }

    #[test]
    fn editorconfig_section_rejects_unknown_fields() {
        let err = serde_json::from_value::<EditorConfig>(serde_json::json!({
            "*": {
                "indent_style": "space",
                "indent": 2
            }
        }))
        .expect_err("unknown editorconfig section field should fail");

        assert!(
            err.to_string().contains("unknown field `indent`"),
            "expected unknown field error, got: {err}"
        );
    }

    #[test]
    fn rules_owners_rejects_unknown_fields() {
        let err = serde_json::from_value::<RulesOwners>(serde_json::json!({
            "rules": {},
            "output": {}
        }))
        .expect_err("unknown rules owners field should fail");

        assert!(
            err.to_string().contains("unknown field `output`"),
            "expected unknown field error, got: {err}"
        );
    }
}
