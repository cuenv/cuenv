use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Codegen Types (for code generation)
// ============================================================================

/// File generation mode
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// Always regenerate this file (managed by codegen)
    #[default]
    Managed,
    /// Generate only if file doesn't exist (user owns this file)
    Scaffold,
}

/// Format configuration for a generated file
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormatConfig {
    /// Indent style: "space" or "tab"
    #[serde(default = "default_indent")]
    pub indent: String,
    /// Indent size (number of spaces or tab width)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_size: Option<usize>,
    /// Maximum line width
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_width: Option<usize>,
    /// Trailing comma style
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailing_comma: Option<String>,
    /// Use semicolons
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semicolons: Option<bool>,
    /// Quote style: "single" or "double"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quotes: Option<String>,
}

/// Lint configuration for a generated file
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LintConfig {
    /// Whether linting is enabled for this generated file.
    #[serde(default)]
    pub enabled: bool,
}

fn default_indent() -> String {
    "space".to_string()
}

/// A file definition from the codegen configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectFile {
    /// Content of the file
    pub content: String,
    /// Programming language of the file
    pub language: String,
    /// Generation mode (managed or scaffold)
    #[serde(default)]
    pub mode: FileMode,
    /// Formatting configuration
    #[serde(default)]
    pub format: FormatConfig,
    /// Whether to add this file path to .gitignore.
    /// Defaults based on mode (set in CUE schema):
    ///   - managed: true (generated files should be ignored)
    ///   - scaffold: false (user-owned files should be committed)
    #[serde(default)]
    pub gitignore: bool,
    /// Optional validation/linting configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<LintConfig>,
}

/// Codegen configuration containing file definitions for code generation
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodegenConfig {
    /// Map of file paths to their definitions
    #[serde(default)]
    pub files: HashMap<String, ProjectFile>,
    /// Optional context data for templating
    #[serde(default)]
    pub context: serde_json::Value,
}
