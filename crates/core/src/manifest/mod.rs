//! Root Project configuration type
//!
//! Based on schema/core.cue

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::module::Instance;
use crate::secrets::Secret;
use crate::tasks::Task;
use crate::tasks::{Input, Mapping, ProjectReference, TaskNode};
use cuenv_hooks::{Hook, Hooks};

/// A hook step to run as part of task dependencies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum HookItem {
    /// Reference to a task in another project
    TaskRef(TaskRef),
    /// Discovery-based hook step that expands a TaskMatcher into concrete tasks
    Match(MatchHook),
    /// Inline task definition
    Task(Box<Task>),
}

/// Hook step that expands to tasks discovered via TaskMatcher.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHook {
    /// Optional stable name used for task naming/logging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Task matcher to select tasks across the workspace
    #[serde(rename = "match")]
    pub matcher: TaskMatcher,
}

/// Reference to a task in another env.cue project by its name property
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRef {
    /// Format: "#project-name:task-name" where project-name is the `name` field in env.cue
    /// Example: "#projen-generator:bun.install"
    #[serde(rename = "ref")]
    pub ref_: String,
}

impl TaskRef {
    /// Parse the TaskRef into project name and task name
    /// Returns None if the format is invalid or if project/task names are empty
    pub fn parse(&self) -> Option<(String, String)> {
        let ref_str = self.ref_.strip_prefix('#')?;
        let parts: Vec<&str> = ref_str.splitn(2, ':').collect();
        if parts.len() == 2 {
            let project = parts[0];
            let task = parts[1];
            if !project.is_empty() && !task.is_empty() {
                Some((project.to_string(), task.to_string()))
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// Match tasks across projects by metadata for discovery-based execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskMatcher {
    /// Match tasks with these labels (all must match)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,

    /// Match tasks whose command matches this value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Match tasks whose args contain specific patterns
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<ArgMatcher>>,

    /// Run matched tasks in parallel (default: true)
    #[serde(default = "default_true")]
    pub parallel: bool,
}

/// Pattern matcher for task arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArgMatcher {
    /// Match if any arg contains this substring
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,

    /// Match if any arg matches this regex pattern
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Base configuration structure (composable across directories)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Base {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Formatters configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatters: Option<Formatters>,
}

// ============================================================================
// Formatter Types
// ============================================================================

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

/// Ignore patterns for tool-specific ignore files.
/// Keys are tool names (e.g., "git", "docker", "prettier").
/// Values can be either:
/// - A list of patterns: `["node_modules/", ".env"]`
/// - An object with patterns and optional filename override
pub type Ignore = HashMap<String, IgnoreValue>;

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
    #[serde(flatten)]
    pub sections: HashMap<String, EditorConfigSection>,
}

/// A section in an EditorConfig file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
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

// ============================================================================
// Runtime Types
// ============================================================================

/// Runtime declares where/how a task executes.
/// Set at project level as the default, override per-task as needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Runtime {
    /// Activate Nix devShell before execution
    Nix(NixRuntime),
    /// Activate devenv shell before execution
    Devenv(DevenvRuntime),
    /// Simple container execution
    Container(ContainerRuntime),
    /// Advanced container with caching, secrets, chaining
    Dagger(DaggerRuntime),
    /// OCI-based binary fetching from container images
    Oci(OciRuntime),
    /// Multi-source tool management (GitHub, OCI, Nix)
    Tools(ToolsRuntime),
}

/// Nix runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NixRuntime {
    /// Flake reference (default: "." for local flake.nix)
    #[serde(default = "default_flake")]
    pub flake: String,
    /// Output attribute path (default: devShells.${system}.default)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

impl Default for NixRuntime {
    fn default() -> Self {
        Self {
            flake: default_flake(),
            output: None,
        }
    }
}

fn default_flake() -> String {
    ".".to_string()
}

/// Devenv runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DevenvRuntime {
    /// Path to devenv config directory (default: ".")
    #[serde(default = "default_flake")]
    pub path: String,
}

/// Simple container runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerRuntime {
    /// Container image (e.g., "node:20-alpine", "rust:1.75-slim")
    pub image: String,
}

/// Dagger runtime configuration (advanced container orchestration)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DaggerRuntime {
    /// Base container image (required unless 'from' is specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Use container from a previous task as base
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Secrets to mount or expose as environment variables
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<DaggerSecret>,
    /// Cache volumes for persistent build caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache: Vec<DaggerCacheMount>,
}

/// Secret configuration for Dagger containers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerSecret {
    /// Name identifier for the secret
    pub name: String,
    /// Mount secret as a file at this path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Expose secret as an environment variable with this name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// Secret resolver configuration
    pub resolver: serde_json::Value,
}

/// Cache volume mount configuration for Dagger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerCacheMount {
    /// Path inside the container to mount the cache
    pub path: String,
    /// Unique name for the cache volume
    pub name: String,
}

/// OCI-based binary runtime configuration.
///
/// Fetches binaries from OCI images for hermetic, content-addressed binary management.
/// Images require explicit `extract` paths to specify which binaries to extract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciRuntime {
    /// Platforms to resolve and lock (e.g., "darwin-arm64", "linux-x86_64")
    #[serde(default)]
    pub platforms: Vec<String>,
    /// OCI images to fetch binaries from
    #[serde(default)]
    pub images: Vec<OciImage>,
    /// Cache directory (defaults to ~/.cache/cuenv/oci)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
}

/// An OCI image to extract binaries from.
///
/// Images require explicit `extract` paths to specify which binaries to extract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OciImage {
    /// Full image reference (e.g., "nginx:1.25-alpine", "gcr.io/distroless/static:latest")
    pub image: String,
    /// Rename the extracted binary (when package name differs from binary name)
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Extraction paths specifying which binaries to extract from the image
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extract: Vec<OciExtract>,
}

/// A binary to extract from a container image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OciExtract {
    /// Path to the binary inside the container (e.g., "/usr/sbin/nginx")
    pub path: String,
    /// Name to expose the binary as in PATH (defaults to filename from path)
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
}

/// GitHub provider configuration for runtime-level authentication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GitHubProviderConfig {
    /// Authentication token (must use secret resolver like 1Password or exec)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
}

/// Multi-source tool runtime configuration.
///
/// Provides ergonomic tool management with platform-specific overrides.
/// Simple case: `jq: "1.7.1"` requires a source to be defined.
/// Complex case: Platform-specific sources with overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsRuntime {
    /// Platforms to resolve and lock (e.g., "darwin-arm64", "linux-x86_64")
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Named Nix flake references for pinning
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub flakes: HashMap<String, String>,
    /// GitHub provider configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubProviderConfig>,
    /// Tool specifications (version string or full Tool config)
    #[serde(default)]
    pub tools: HashMap<String, ToolSpec>,
    /// Cache directory (defaults to ~/.cache/cuenv/tools)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
}

/// Tool specification - either a simple version or full config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolSpec {
    /// Simple version string (requires explicit source configuration)
    Version(String),
    /// Full tool configuration with source and overrides
    Full(ToolConfig),
}

impl ToolSpec {
    /// Get the version string.
    #[must_use]
    pub fn version(&self) -> &str {
        match self {
            Self::Version(v) => v,
            Self::Full(c) => &c.version,
        }
    }
}

/// Full tool configuration with source and platform overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    /// Version string (e.g., "1.7.1", "latest")
    pub version: String,
    /// Rename the binary in PATH
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Default source for all platforms
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceConfig>,
    /// Platform-specific source overrides
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<SourceOverride>,
}

/// Platform-specific source override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceOverride {
    /// Match by OS (darwin, linux)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Match by architecture (arm64, x86_64)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Source for matching platforms
    pub source: SourceConfig,
}

/// Source configuration for fetching a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    /// Extract from OCI container image
    Oci {
        /// Image reference with optional {version}, {os}, {arch} templates
        image: String,
        /// Path to binary inside the container
        path: String,
    },
    /// Download from GitHub Releases
    #[serde(rename = "github")]
    GitHub {
        /// Repository (owner/repo)
        repo: String,
        /// Tag prefix (prepended to version, defaults to "")
        #[serde(default, rename = "tagPrefix")]
        tag_prefix: String,
        /// Release tag override (if set, ignores tagPrefix)
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        /// Asset name with optional {version}, {os}, {arch} templates
        asset: String,
        /// Path to binary within archive (if archived)
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Build from Nix flake
    Nix {
        /// Named flake reference (key in runtime.flakes)
        flake: String,
        /// Package attribute (e.g., "jq", "python3")
        package: String,
        /// Output path if binary can't be auto-detected
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Install via rustup
    Rustup {
        /// Toolchain identifier (e.g., "stable", "1.83.0", "nightly-2024-01-01")
        toolchain: String,
        /// Installation profile: minimal, default, complete
        #[serde(default = "default_rustup_profile")]
        profile: String,
        /// Additional components to install (e.g., "clippy", "rustfmt", "rust-src")
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        components: Vec<String>,
        /// Additional targets to install (e.g., "x86_64-unknown-linux-gnu")
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<String>,
    },
}

fn default_rustup_profile() -> String {
    "default".to_string()
}

// ============================================================================
// Project Type
// ============================================================================

/// Root Project configuration structure (leaf node - cannot unify with other projects)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Project {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Project name (unique identifier, required by the CUE schema)
    pub name: String,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// CI configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CI>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskNode>,

    /// Codegen configuration for code generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codegen: Option<CodegenConfig>,

    /// Runtime configuration (project-level default for all tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Formatters configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatters: Option<Formatters>,
}

impl Project {
    /// Create a new Project configuration with a required name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Get hooks to execute when entering environment as a map (name -> hook)
    pub fn on_enter_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_enter.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when entering environment, sorted by (order, name)
    pub fn on_enter_hooks(&self) -> Vec<Hook> {
        let map = self.on_enter_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute when exiting environment as a map (name -> hook)
    pub fn on_exit_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_exit.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when exiting environment, sorted by (order, name)
    pub fn on_exit_hooks(&self) -> Vec<Hook> {
        let map = self.on_exit_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute before git push as a map (name -> hook)
    pub fn pre_push_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.pre_push.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute before git push, sorted by (order, name)
    pub fn pre_push_hooks(&self) -> Vec<Hook> {
        let map = self.pre_push_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Returns self unchanged.
    ///
    /// Workspace detection and task injection now happens via auto-detection
    /// from lockfiles in the task executor. This method is kept for API compatibility.
    #[must_use]
    pub fn with_implicit_tasks(self) -> Self {
        self
    }

    /// Expand shorthand cross-project references in inputs and implicit dependencies.
    ///
    /// Handles inputs in the format: "#project:task:path/to/file"
    /// Converts them to explicit ProjectReference inputs.
    /// Also adds implicit dependsOn entries for all project references.
    pub fn expand_cross_project_references(&mut self) {
        for (_, task_node) in self.tasks.iter_mut() {
            Self::expand_task_node(task_node);
        }
    }

    fn expand_task_node(node: &mut TaskNode) {
        match node {
            TaskNode::Task(task) => Self::expand_task(task),
            TaskNode::Group(group) => {
                for sub_node in group.children.values_mut() {
                    Self::expand_task_node(sub_node);
                }
            }
            TaskNode::Sequence(sequence) => {
                for sub_node in sequence {
                    Self::expand_task_node(sub_node);
                }
            }
        }
    }

    fn expand_task(task: &mut Task) {
        let mut new_inputs = Vec::new();
        let mut implicit_deps = Vec::new();

        // Process existing inputs
        for input in &task.inputs {
            match input {
                Input::Path(path) if path.starts_with('#') => {
                    // Parse "#project:task:path"
                    // Remove leading #
                    let parts: Vec<&str> = path[1..].split(':').collect();
                    if parts.len() >= 3 {
                        let project = parts[0].to_string();
                        let task_name = parts[1].to_string();
                        // Rejoin the rest as the path (it might contain colons)
                        let file_path = parts[2..].join(":");

                        new_inputs.push(Input::Project(ProjectReference {
                            project: project.clone(),
                            task: task_name.clone(),
                            map: vec![Mapping {
                                from: file_path.clone(),
                                to: file_path,
                            }],
                        }));

                        // Add implicit dependency
                        implicit_deps.push(format!("#{}:{}", project, task_name));
                    } else if parts.len() == 2 {
                        // Handle "#project:task" as pure dependency?
                        // The prompt says: `["#projectName:taskName"]` for dependsOn
                        // For inputs, it likely expects a file mapping.
                        // If user puts `["#p:t"]` in inputs, it's invalid as an input unless it maps something.
                        // Assuming `#p:t:f` is the requirement for inputs.
                        // Keeping original if not matching pattern (or maybe warning?)
                        new_inputs.push(input.clone());
                    } else {
                        new_inputs.push(input.clone());
                    }
                }
                Input::Project(proj_ref) => {
                    // Add implicit dependency for explicit project references too
                    implicit_deps.push(format!("#{}:{}", proj_ref.project, proj_ref.task));
                    new_inputs.push(input.clone());
                }
                _ => new_inputs.push(input.clone()),
            }
        }

        task.inputs = new_inputs;

        // Add unique implicit dependencies
        for dep in implicit_deps {
            if !task.depends_on.iter().any(|d| d.task_name() == dep) {
                task.depends_on
                    .push(crate::tasks::TaskDependency::from_name(dep));
            }
        }
    }
}

impl TryFrom<&Instance> for Project {
    type Error = crate::Error;

    fn try_from(instance: &Instance) -> Result<Self, Self::Error> {
        let mut project: Project = instance.deserialize()?;
        project.expand_cross_project_references();
        Ok(project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskDependency, TaskGroup, TaskList, TaskNode};
    use crate::test_utils::create_test_hook;

    #[test]
    fn test_expand_cross_project_references() {
        let task = Task {
            inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
            ..Default::default()
        };

        let mut cuenv = Project::new("test");
        cuenv
            .tasks
            .insert("deploy".into(), TaskNode::Task(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("deploy").unwrap();
        let task = task_def.as_task().unwrap();

        // Check inputs expansion
        assert_eq!(task.inputs.len(), 1);
        match &task.inputs[0] {
            Input::Project(proj_ref) => {
                assert_eq!(proj_ref.project, "myproj");
                assert_eq!(proj_ref.task, "build");
                assert_eq!(proj_ref.map.len(), 1);
                assert_eq!(proj_ref.map[0].from, "dist/app.js");
                assert_eq!(proj_ref.map[0].to, "dist/app.js");
            }
            _ => panic!("Expected ProjectReference"),
        }

        // Check implicit dependency
        assert_eq!(task.depends_on.len(), 1);
        assert_eq!(task.depends_on[0].task_name(), "#myproj:build");
    }

    // ============================================================================
    // HookItem and TaskRef Tests
    // ============================================================================

    #[test]
    fn test_task_ref_parse_valid() {
        let task_ref = TaskRef {
            ref_: "#projen-generator:types".to_string(),
        };

        let parsed = task_ref.parse();
        assert!(parsed.is_some());

        let (project, task) = parsed.unwrap();
        assert_eq!(project, "projen-generator");
        assert_eq!(task, "types");
    }

    #[test]
    fn test_task_ref_parse_with_dots() {
        let task_ref = TaskRef {
            ref_: "#my-project:bun.install".to_string(),
        };

        let parsed = task_ref.parse();
        assert!(parsed.is_some());

        let (project, task) = parsed.unwrap();
        assert_eq!(project, "my-project");
        assert_eq!(task, "bun.install");
    }

    #[test]
    fn test_task_ref_parse_no_hash() {
        let task_ref = TaskRef {
            ref_: "project:task".to_string(),
        };

        // Without leading #, parse should fail
        let parsed = task_ref.parse();
        assert!(parsed.is_none());
    }

    #[test]
    fn test_task_ref_parse_no_colon() {
        let task_ref = TaskRef {
            ref_: "#project-only".to_string(),
        };

        // Without colon separator, parse should fail
        let parsed = task_ref.parse();
        assert!(parsed.is_none());
    }

    #[test]
    fn test_task_ref_parse_empty_project() {
        let task_ref = TaskRef {
            ref_: "#:task".to_string(),
        };

        // Empty project name should be rejected
        assert!(task_ref.parse().is_none());
    }

    #[test]
    fn test_task_ref_parse_empty_task() {
        let task_ref = TaskRef {
            ref_: "#project:".to_string(),
        };

        // Empty task name should be rejected
        assert!(task_ref.parse().is_none());
    }

    #[test]
    fn test_task_ref_parse_both_empty() {
        let task_ref = TaskRef {
            ref_: "#:".to_string(),
        };

        // Both empty should be rejected
        assert!(task_ref.parse().is_none());
    }

    #[test]
    fn test_task_ref_parse_multiple_colons() {
        let task_ref = TaskRef {
            ref_: "#project:task:extra".to_string(),
        };

        // Multiple colons - first split wins
        let parsed = task_ref.parse();
        assert!(parsed.is_some());
        let (project, task) = parsed.unwrap();
        assert_eq!(project, "project");
        assert_eq!(task, "task:extra");
    }

    #[test]
    fn test_task_ref_parse_unicode() {
        let task_ref = TaskRef {
            ref_: "#项目名:任务名".to_string(),
        };

        let parsed = task_ref.parse();
        assert!(parsed.is_some());
        let (project, task) = parsed.unwrap();
        assert_eq!(project, "项目名");
        assert_eq!(task, "任务名");
    }

    #[test]
    fn test_task_ref_parse_special_characters() {
        let task_ref = TaskRef {
            ref_: "#my-project_v2:build.ci-test".to_string(),
        };

        let parsed = task_ref.parse();
        assert!(parsed.is_some());
        let (project, task) = parsed.unwrap();
        assert_eq!(project, "my-project_v2");
        assert_eq!(task, "build.ci-test");
    }

    #[test]
    fn test_hook_item_task_ref_deserialization() {
        let json = "{\"ref\": \"#other-project:build\"}";
        let hook_item: HookItem = serde_json::from_str(json).unwrap();

        match hook_item {
            HookItem::TaskRef(task_ref) => {
                assert_eq!(task_ref.ref_, "#other-project:build");
                let (project, task) = task_ref.parse().unwrap();
                assert_eq!(project, "other-project");
                assert_eq!(task, "build");
            }
            _ => panic!("Expected HookItem::TaskRef"),
        }
    }

    #[test]
    fn test_hook_item_match_deserialization() {
        let json = r#"{
            "name": "projen",
            "match": {
                "labels": ["codegen", "projen"]
            }
        }"#;
        let hook_item: HookItem = serde_json::from_str(json).unwrap();

        match hook_item {
            HookItem::Match(match_hook) => {
                assert_eq!(match_hook.name, Some("projen".to_string()));
                assert_eq!(
                    match_hook.matcher.labels,
                    Some(vec!["codegen".to_string(), "projen".to_string()])
                );
            }
            _ => panic!("Expected HookItem::Match"),
        }
    }

    #[test]
    fn test_hook_item_match_with_parallel_false() {
        let json = r#"{
            "match": {
                "labels": ["build"],
                "parallel": false
            }
        }"#;
        let hook_item: HookItem = serde_json::from_str(json).unwrap();

        match hook_item {
            HookItem::Match(match_hook) => {
                assert!(match_hook.name.is_none());
                assert!(!match_hook.matcher.parallel);
            }
            _ => panic!("Expected HookItem::Match"),
        }
    }

    #[test]
    fn test_hook_item_inline_task_deserialization() {
        let json = r#"{
            "command": "echo",
            "args": ["hello"]
        }"#;
        let hook_item: HookItem = serde_json::from_str(json).unwrap();

        match hook_item {
            HookItem::Task(task) => {
                assert_eq!(task.command, "echo");
                assert_eq!(task.args, vec!["hello"]);
            }
            _ => panic!("Expected HookItem::Task"),
        }
    }

    #[test]
    fn test_task_matcher_deserialization() {
        let json = r#"{
            "labels": ["projen", "codegen"],
            "parallel": true
        }"#;
        let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

        assert_eq!(
            matcher.labels,
            Some(vec!["projen".to_string(), "codegen".to_string()])
        );
        assert!(matcher.parallel);
    }

    #[test]
    fn test_task_matcher_defaults() {
        let json = r#"{}"#;
        let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

        assert!(matcher.labels.is_none());
        assert!(matcher.command.is_none());
        assert!(matcher.args.is_none());
        assert!(matcher.parallel); // default true
    }

    #[test]
    fn test_task_matcher_with_command() {
        let json = r#"{
            "command": "prisma",
            "args": [{"contains": "generate"}]
        }"#;
        let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

        assert_eq!(matcher.command, Some("prisma".to_string()));
        let args = matcher.args.unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].contains, Some("generate".to_string()));
    }

    // ============================================================================
    // Cross-Project Reference Expansion Tests
    // ============================================================================

    #[test]
    fn test_expand_multiple_cross_project_references() {
        let task = Task {
            inputs: vec![
                Input::Path("#projA:build:dist/lib.js".to_string()),
                Input::Path("#projB:compile:out/types.d.ts".to_string()),
                Input::Path("src/**/*.ts".to_string()), // Local path
            ],
            ..Default::default()
        };

        let mut cuenv = Project::new("test");
        cuenv
            .tasks
            .insert("bundle".into(), TaskNode::Task(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("bundle").unwrap();
        let task = task_def.as_task().unwrap();

        // Should have 3 inputs (2 project refs + 1 local)
        assert_eq!(task.inputs.len(), 3);

        // Should have 2 implicit dependencies
        assert_eq!(task.depends_on.len(), 2);
        assert!(
            task.depends_on
                .iter()
                .any(|d| d.task_name() == "#projA:build")
        );
        assert!(
            task.depends_on
                .iter()
                .any(|d| d.task_name() == "#projB:compile")
        );
    }

    #[test]
    fn test_expand_cross_project_in_task_group() {
        let task1 = Task {
            command: "step1".to_string(),
            inputs: vec![Input::Path("#projA:build:dist/lib.js".to_string())],
            ..Default::default()
        };

        let task2 = Task {
            command: "step2".to_string(),
            inputs: vec![Input::Path("#projB:compile:out/types.d.ts".to_string())],
            ..Default::default()
        };

        let mut cuenv = Project::new("test");
        cuenv.tasks.insert(
            "pipeline".into(),
            TaskNode::List(TaskList {
                steps: vec![
                    TaskNode::Task(Box::new(task1)),
                    TaskNode::Task(Box::new(task2)),
                ],
                depends_on: vec![],
                stop_on_first_error: true,
                description: None,
            }),
        );

        cuenv.expand_cross_project_references();

        // Verify expansion happened in both tasks
        match cuenv.tasks.get("pipeline").unwrap() {
            TaskNode::List(list) => {
                match &list.steps[0] {
                    TaskNode::Task(task) => {
                        assert!(
                            task.depends_on
                                .iter()
                                .any(|d| d.task_name() == "#projA:build")
                        );
                    }
                    _ => panic!("Expected single task"),
                }
                match &list.steps[1] {
                    TaskNode::Task(task) => {
                        assert!(
                            task.depends_on
                                .iter()
                                .any(|d| d.task_name() == "#projB:compile")
                        );
                    }
                    _ => panic!("Expected single task"),
                }
            }
            _ => panic!("Expected task list"),
        }
    }

    #[test]
    fn test_expand_cross_project_in_parallel_group() {
        let task1 = Task {
            command: "taskA".to_string(),
            inputs: vec![Input::Path("#projA:build:lib.js".to_string())],
            ..Default::default()
        };

        let task2 = Task {
            command: "taskB".to_string(),
            inputs: vec![Input::Path("#projB:build:types.d.ts".to_string())],
            ..Default::default()
        };

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("a".to_string(), TaskNode::Task(Box::new(task1)));
        parallel_tasks.insert("b".to_string(), TaskNode::Task(Box::new(task2)));

        let mut cuenv = Project::new("test");
        cuenv.tasks.insert(
            "parallel".into(),
            TaskNode::Group(TaskGroup {
                parallel: parallel_tasks,
                depends_on: vec![],
                description: None,
                max_concurrency: None,
            }),
        );

        cuenv.expand_cross_project_references();

        // Verify expansion happened in both parallel tasks
        match cuenv.tasks.get("parallel").unwrap() {
            TaskNode::Group(group) => {
                match group.parallel.get("a").unwrap() {
                    TaskNode::Task(task) => {
                        assert!(
                            task.depends_on
                                .iter()
                                .any(|d| d.task_name() == "#projA:build")
                        );
                    }
                    _ => panic!("Expected single task"),
                }
                match group.parallel.get("b").unwrap() {
                    TaskNode::Task(task) => {
                        assert!(
                            task.depends_on
                                .iter()
                                .any(|d| d.task_name() == "#projB:build")
                        );
                    }
                    _ => panic!("Expected single task"),
                }
            }
            _ => panic!("Expected parallel group"),
        }
    }

    #[test]
    fn test_no_duplicate_implicit_dependencies() {
        // Task already has the dependency explicitly
        let task = Task {
            depends_on: vec![TaskDependency::from_name("#myproj:build")],
            inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
            ..Default::default()
        };

        let mut cuenv = Project::new("test");
        cuenv
            .tasks
            .insert("deploy".into(), TaskNode::Task(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("deploy").unwrap();
        let task = task_def.as_task().unwrap();

        // Should not duplicate the dependency
        assert_eq!(task.depends_on.len(), 1);
        assert_eq!(task.depends_on[0].task_name(), "#myproj:build");
    }

    // ============================================================================
    // Project Hooks (onEnter, onExit) Tests
    // ============================================================================

    #[test]
    fn test_on_enter_hooks_ordering() {
        let mut on_enter = HashMap::new();
        on_enter.insert("hook_c".to_string(), create_test_hook(300, "echo c"));
        on_enter.insert("hook_a".to_string(), create_test_hook(100, "echo a"));
        on_enter.insert("hook_b".to_string(), create_test_hook(200, "echo b"));

        let mut cuenv = Project::new("test");
        cuenv.hooks = Some(Hooks {
            on_enter: Some(on_enter),
            on_exit: None,
            pre_push: None,
        });

        let hooks = cuenv.on_enter_hooks();
        assert_eq!(hooks.len(), 3);

        // Should be sorted by order
        assert_eq!(hooks[0].order, 100);
        assert_eq!(hooks[1].order, 200);
        assert_eq!(hooks[2].order, 300);
    }

    #[test]
    fn test_on_enter_hooks_same_order_sort_by_name() {
        let mut on_enter = HashMap::new();
        on_enter.insert("z_hook".to_string(), create_test_hook(100, "echo z"));
        on_enter.insert("a_hook".to_string(), create_test_hook(100, "echo a"));

        let cuenv = Project {
            name: "test".to_string(),
            hooks: Some(Hooks {
                on_enter: Some(on_enter),
                on_exit: None,
                pre_push: None,
            }),
            ..Default::default()
        };

        let hooks = cuenv.on_enter_hooks();
        assert_eq!(hooks.len(), 2);

        // Same order, should be sorted by name
        assert_eq!(hooks[0].command, "echo a");
        assert_eq!(hooks[1].command, "echo z");
    }

    #[test]
    fn test_empty_hooks() {
        let cuenv = Project::new("test");

        let on_enter = cuenv.on_enter_hooks();
        let on_exit = cuenv.on_exit_hooks();

        assert!(on_enter.is_empty());
        assert!(on_exit.is_empty());
    }

    #[test]
    fn test_project_deserialization_with_script_tasks() {
        // This test uses the new explicit API with parallel groups
        let json = r#"{
            "name": "cuenv",
            "hooks": {
                "onEnter": {
                    "nix": {
                        "order": 10,
                        "propagate": false,
                        "command": "nix",
                        "args": ["print-dev-env"],
                        "inputs": ["flake.nix", "flake.lock"],
                        "source": true
                    }
                }
            },
            "tasks": {
                "pwd": { "command": "pwd" },
                "check": {
                    "command": "nix",
                    "args": ["flake", "check"],
                    "inputs": ["flake.nix"]
                },
                "fmt": {
                    "parallel": {
                        "fix": {
                            "command": "treefmt",
                            "inputs": [".config"]
                        },
                        "check": {
                            "command": "treefmt",
                            "args": ["--fail-on-change"],
                            "inputs": [".config"]
                        }
                    }
                },
                "cross": {
                    "parallel": {
                        "linux": {
                            "script": "echo building for linux",
                            "inputs": ["Cargo.toml"]
                        }
                    }
                },
                "docs": {
                    "parallel": {
                        "build": {
                            "command": "bash",
                            "args": ["-c", "bun install"],
                            "inputs": ["docs"],
                            "outputs": ["docs/dist"]
                        },
                        "deploy": {
                            "command": "bash",
                            "args": ["-c", "wrangler deploy"],
                            "dependsOn": ["docs.build"],
                            "inputs": [{"task": "docs.build"}]
                        }
                    }
                }
            }
        }"#;

        let result: Result<Project, _> = serde_json::from_str(json);
        match result {
            Ok(project) => {
                assert_eq!(project.name, "cuenv");
                assert_eq!(project.tasks.len(), 5);
                assert!(project.tasks.contains_key("pwd"));
                assert!(project.tasks.contains_key("cross"));
                // Verify cross is a group with parallel subtasks
                let cross = project.tasks.get("cross").unwrap();
                assert!(cross.is_group());
            }
            Err(e) => {
                panic!("Failed to deserialize Project with script tasks: {}", e);
            }
        }
    }

    #[test]
    fn test_deserialize_actual_cuenv_project() {
        // Read actual CUE output from /tmp/project.json (created by cue eval)
        let json = match std::fs::read_to_string("/tmp/project.json") {
            Ok(content) => content,
            Err(_) => return, // Skip if file doesn't exist
        };
        let result: Result<Project, _> = serde_json::from_str(&json);
        match result {
            Ok(project) => {
                eprintln!("Project name: {}", project.name);
                eprintln!("Tasks: {:?}", project.tasks.keys().collect::<Vec<_>>());
            }
            Err(e) => {
                eprintln!("Failed: {}", e);
                eprintln!("Line: {}, Col: {}", e.line(), e.column());
                // Read the JSON around the error line
                let lines: Vec<&str> = json.lines().collect();
                let line_num = e.line();
                let start = if line_num > 3 { line_num - 3 } else { 1 };
                let end = std::cmp::min(line_num + 3, lines.len());
                for i in start..=end {
                    if i <= lines.len() {
                        eprintln!("{}: {}", i, lines[i - 1]);
                    }
                }
                panic!("Deserialization failed");
            }
        }
    }
}
