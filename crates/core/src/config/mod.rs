//! Configuration types for cuenv
//!
//! Based on schema/config.cue

use serde::{Deserialize, Serialize};

/// Main configuration structure for cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Task output format (for task execution)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,

    /// Command-specific configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<CommandsConfig>,

    /// Cache configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_mode: Option<CacheMode>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,

    /// Security and debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_mode: Option<bool>,

    /// Chrome trace generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_output: Option<bool>,

    /// Default environment settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_capabilities: Option<Vec<String>>,

    /// Task backend configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendConfig>,

    /// CI-specific configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CIConfig>,
}

/// Command-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommandsConfig {
    /// Task command configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskCommandConfig>,
}

/// Task command configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandConfig {
    /// Task list configuration (for `cuenv task` without arguments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<TaskListConfig>,
}

/// Task list display configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskListConfig {
    /// Output format for task listing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TaskListFormat>,
}

impl Config {
    /// Get the configured task list format, if any.
    ///
    /// Accesses `config.commands.task.list.format` with safe navigation.
    #[must_use]
    pub fn task_list_format(&self) -> Option<TaskListFormat> {
        self.commands
            .as_ref()
            .and_then(|c| c.task.as_ref())
            .and_then(|t| t.list.as_ref())
            .and_then(|l| l.format)
    }
}

/// CI-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CIConfig {
    /// Cuenv installation configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuenv: Option<CuenvConfig>,
}

/// Configuration for cuenv installation in CI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CuenvConfig {
    /// Source for cuenv binary
    #[serde(default)]
    pub source: CuenvSource,

    /// Version to install ("self", "latest", or specific version like "0.17.0")
    #[serde(default = "default_cuenv_version")]
    pub version: String,
}

fn default_cuenv_version() -> String {
    "self".to_string()
}

impl Default for CuenvConfig {
    fn default() -> Self {
        Self {
            source: CuenvSource::Release,
            version: default_cuenv_version(),
        }
    }
}

/// Source for cuenv binary in CI environments
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CuenvSource {
    /// Build using native Rust/Go toolchains (no Nix)
    Native,
    /// Use pre-built artifact from earlier CI job
    Artifact,
    /// Build from git checkout (requires Nix)
    Git,
    /// Install via Nix flake (auto-configures Cachix)
    Nix,
    /// Install via Homebrew tap (no Nix required)
    Homebrew,
    /// Download from GitHub Releases (default)
    #[default]
    Release,
}

impl CuenvSource {
    /// Get the string representation for activation condition matching
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Artifact => "artifact",
            Self::Git => "git",
            Self::Nix => "nix",
            Self::Homebrew => "homebrew",
            Self::Release => "release",
        }
    }
}

/// Task output format options (for task execution)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Tui,
    Spinner,
    Simple,
    Tree,
    Json,
}

/// Task list format options (for `cuenv task` without arguments)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskListFormat {
    /// Plain tree structure (default for non-TTY)
    Text,
    /// Colored tree structure (default for TTY)
    Rich,
    /// Category-grouped bordered tables
    Tables,
    /// Status dashboard with cache indicators
    Dashboard,
    /// Emoji-prefixed semantic categories
    Emoji,
}

impl TaskListFormat {
    /// Convert to the string representation used by the task command
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Rich => "rich",
            Self::Tables => "tables",
            Self::Dashboard => "dashboard",
            Self::Emoji => "emoji",
        }
    }
}

/// Cache mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheMode {
    Off,
    Read,
    ReadWrite,
    Write,
}

fn default_backend_type() -> String {
    "host".to_string()
}

/// Backend configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackendConfig {
    /// Which backend to use for tasks
    #[serde(default = "default_backend_type", rename = "type")]
    pub backend_type: String,

    /// Backend-specific options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<BackendOptions>,
}

/// Backend-specific options supported by cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BackendOptions {
    /// Default container image for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Optional platform hint for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}
