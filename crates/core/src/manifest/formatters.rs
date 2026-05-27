use serde::{Deserialize, Serialize};

// ============================================================================
// Formatter Types
// ============================================================================

fn default_true() -> bool {
    true
}

/// Formatters configuration for code formatting tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Formatters {
    /// Rust formatter configuration (rustfmt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust: Option<RustFormatter>,

    /// Nix formatter configuration (nixfmt or alejandra)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nix: Option<NixFormatter>,

    /// Go formatter configuration (gofmt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub go: Option<GoFormatter>,

    /// CUE formatter configuration (cue fmt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cue: Option<CueFormatter>,
}

/// Rust formatter configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RustFormatter {
    /// Whether this formatter is enabled (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Glob patterns for files to format (default: ["*.rs"])
    #[serde(default = "default_rs_includes")]
    pub includes: Vec<String>,

    /// Rust edition for formatting rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
}

impl Default for RustFormatter {
    fn default() -> Self {
        Self {
            enabled: true,
            includes: default_rs_includes(),
            edition: None,
        }
    }
}

fn default_rs_includes() -> Vec<String> {
    vec!["*.rs".to_string()]
}

/// Nix formatter tool selection
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NixFormatterTool {
    /// Use nixfmt (default)
    #[default]
    Nixfmt,
    /// Use alejandra
    Alejandra,
}

impl NixFormatterTool {
    /// Get the command name for this tool
    #[must_use]
    pub fn command(&self) -> &'static str {
        match self {
            Self::Nixfmt => "nixfmt",
            Self::Alejandra => "alejandra",
        }
    }

    /// Get the check flag for this tool
    #[must_use]
    pub fn check_flag(&self) -> &'static str {
        match self {
            Self::Nixfmt => "--check",
            Self::Alejandra => "-c",
        }
    }
}

/// Nix formatter configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NixFormatter {
    /// Whether this formatter is enabled (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Glob patterns for files to format (default: ["*.nix"])
    #[serde(default = "default_nix_includes")]
    pub includes: Vec<String>,

    /// Which Nix formatter tool to use (nixfmt or alejandra)
    #[serde(default)]
    pub tool: NixFormatterTool,
}

impl Default for NixFormatter {
    fn default() -> Self {
        Self {
            enabled: true,
            includes: default_nix_includes(),
            tool: NixFormatterTool::default(),
        }
    }
}

fn default_nix_includes() -> Vec<String> {
    vec!["*.nix".to_string()]
}

/// Go formatter configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GoFormatter {
    /// Whether this formatter is enabled (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Glob patterns for files to format (default: ["*.go"])
    #[serde(default = "default_go_includes")]
    pub includes: Vec<String>,
}

impl Default for GoFormatter {
    fn default() -> Self {
        Self {
            enabled: true,
            includes: default_go_includes(),
        }
    }
}

fn default_go_includes() -> Vec<String> {
    vec!["*.go".to_string()]
}

/// CUE formatter configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CueFormatter {
    /// Whether this formatter is enabled (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Glob patterns for files to format (default: ["*.cue"])
    #[serde(default = "default_cue_includes")]
    pub includes: Vec<String>,
}

impl Default for CueFormatter {
    fn default() -> Self {
        Self {
            enabled: true,
            includes: default_cue_includes(),
        }
    }
}

fn default_cue_includes() -> Vec<String> {
    vec!["*.cue".to_string()]
}
