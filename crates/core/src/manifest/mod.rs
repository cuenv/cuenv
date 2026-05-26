//! Root Project configuration type
//!
//! Based on schema/core.cue

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::environment::EnvValue;
use crate::module::Instance;
use crate::secrets::Secret;
use crate::tasks::Task;
use crate::tasks::{
    Input, Mapping, ProjectReference, ScriptShell, ShellOptions, TaskDependency, TaskNode,
};
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

    /// Runtime configuration (devenv, nix, tools, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// Cuenv-managed VCS dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vcs: HashMap<String, VcsDependency>,
}

/// A cuenv-managed Git dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VcsDependency {
    /// Git repository URL.
    pub url: String,
    /// Branch, tag, or commit-ish to resolve.
    #[serde(default = "default_vcs_reference")]
    pub reference: String,
    /// Whether to materialize a tracked source snapshot.
    pub vendor: bool,
    /// Repository-relative materialization path.
    pub path: String,
    /// Subdirectory of the repo to materialize via sparse checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

fn default_vcs_reference() -> String {
    "HEAD".to_string()
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
    pub sections: std::collections::BTreeMap<String, EditorConfigSection>,
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
    Tools(Box<ToolsRuntime>),
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
        /// Legacy single-file selector inside archive/pkg payloads.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Optional typed extraction rules for archive/pkg assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<GitHubExtract>,
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
    /// Download from an arbitrary HTTP URL
    #[serde(rename = "url")]
    Url {
        /// URL with optional {version}, {os}, {arch} templates
        url: String,
        /// Legacy single-file selector inside archive payloads.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Optional typed extraction rules for archive assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<GitHubExtract>,
    },
}

fn default_rustup_profile() -> String {
    "default".to_string()
}

/// Typed extraction rule for GitHub release assets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GitHubExtract {
    /// Extract a binary and place it in `bin/`.
    Bin {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional binary rename in cache/bin.
        #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
        as_name: Option<String>,
    },
    /// Extract a dynamic library and place it in `lib/`.
    Lib {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional env var to export the absolute file path.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    /// Extract include/header material and place it in `include/`.
    Include {
        /// Path to file in the archive/pkg payload.
        path: String,
    },
    /// Extract pkg-config metadata and place it in `lib/pkgconfig/`.
    PkgConfig {
        /// Path to file in the archive/pkg payload.
        path: String,
    },
    /// Extract a generic file and place it in `files/`.
    File {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional env var to export the absolute file path.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
}

// ============================================================================
// Service Types
// ============================================================================

/// Structured command invocation: a program plus its arguments.
///
/// Shared base type for tasks and service entrypoints. Arguments may be
/// literal strings or runtime task output references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Command {
    /// Program to execute.
    pub command: String,

    /// Arguments (may contain output refs).
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Inline script invocation: a script body interpreted by a shell.
///
/// Shared base type for tasks and service entrypoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    /// Script body.
    pub script: String,

    /// Shell interpreter (defaults to bash on the CUE side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_shell: Option<ScriptShell>,

    /// Shell options (errexit, nounset, pipefail, xtrace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_options: Option<ShellOptions>,
}

/// How a [`Service`] is executed.
///
/// Either:
/// - a full [`Task`] (lets a service reuse an existing task definition),
/// - an inline [`Script`], or
/// - an inline [`Command`].
///
/// Deserialized as an untagged enum, with the most specific variant
/// (`Task`) attempted first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Entrypoint {
    /// Full task reference (or inline task) reused as a service entrypoint.
    Task(Box<Task>),
    /// Inline script.
    Script(Script),
    /// Inline command.
    Command(Command),
}

impl Default for Entrypoint {
    fn default() -> Self {
        Entrypoint::Command(Command::default())
    }
}

/// Long-running supervised process definition.
///
/// Services live alongside tasks on a project but execute under different
/// rules: they must reach a readiness state, are kept alive across the
/// session, restart according to policy, and tear down on `cuenv down`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Service {
    /// Type discriminator — always `"service"`.
    #[serde(rename = "type", default = "default_service_type")]
    pub service_type: String,

    /// How the service process is launched.
    #[serde(default)]
    pub entrypoint: Entrypoint,

    /// Environment variables (same shape as Task).
    #[serde(default)]
    pub env: HashMap<String, EnvValue>,

    /// Working directory override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,

    /// Dependencies — may reference tasks OR services.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Labels for discovery via ServiceMatcher.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Runtime override for this service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Readiness probe (single probe per service).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,

    /// Restart policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,

    /// File watcher for restart-on-change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<ServiceWatch>,

    /// Log handling configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<ServiceLogs>,

    /// Shutdown behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<Shutdown>,

    /// Hard kill if startup-to-ready exceeds this duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

impl Service {
    /// Return the primary program name for workspace-detection heuristics
    /// (bun, cargo, etc.). Scripts have no single program.
    #[must_use]
    pub fn primary_command(&self) -> Option<&str> {
        match &self.entrypoint {
            Entrypoint::Task(task) => {
                if task.command.is_empty() {
                    None
                } else {
                    Some(task.command.as_str())
                }
            }
            Entrypoint::Command(cmd) => Some(cmd.command.as_str()),
            Entrypoint::Script(_) => None,
        }
    }
}

fn default_service_type() -> String {
    "service".to_string()
}

// ============================================================================
// Container Image Types
// ============================================================================

/// Output reference for a container image (ref or digest).
///
/// Mirrors [`TaskOutputRef`] but for image build outputs. The executor
/// resolves these at runtime after the image is built.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageOutputRef {
    #[serde(rename = "cuenvOutputRef")]
    pub cuenv_output_ref: bool,
    #[serde(rename = "cuenvImage")]
    pub cuenv_image: String,
    #[serde(rename = "cuenvOutput")]
    pub cuenv_output: String,
}

/// Container image build definition.
///
/// Declares a container image as a first-class project artifact. Images
/// participate in the task DAG and produce output references (`.ref`,
/// `.digest`) that downstream tasks can consume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContainerImage {
    /// Type discriminator — always `"image"`.
    #[serde(rename = "type", default = "default_image_type")]
    pub image_type: String,

    /// Image reference output — resolved at runtime after build.
    #[serde(rename = "ref")]
    pub ref_output: ImageOutputRef,

    /// Image digest output — resolved at runtime after build.
    pub digest: ImageOutputRef,

    /// Build context directory (required).
    pub context: String,

    /// Dockerfile path relative to context.
    #[serde(default = "default_dockerfile")]
    pub dockerfile: String,

    /// Build arguments (values may be literal strings or image output refs).
    #[serde(
        default,
        rename = "buildArgs",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub build_args: HashMap<String, serde_json::Value>,

    /// Target stage for multi-stage builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Image tags (e.g., `["latest", "v1.0.0"]`).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Registry to push to (omit for local-only builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,

    /// Repository name (defaults to image name if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Target platforms for multi-arch builds.
    #[serde(default)]
    pub platform: Vec<String>,

    /// Dependencies on tasks or other images.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Labels for discovery.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Input files/patterns for cache key derivation.
    #[serde(default)]
    pub inputs: Vec<Input>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_image_type() -> String {
    "image".to_string()
}

fn default_dockerfile() -> String {
    "Dockerfile".to_string()
}

/// Readiness probe — discriminated by `kind` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum Readiness {
    /// TCP port connectivity check.
    #[serde(rename = "port")]
    Port(ReadinessPort),
    /// HTTP endpoint check.
    #[serde(rename = "http")]
    Http(ReadinessHttp),
    /// Regex match on service output.
    #[serde(rename = "log")]
    Log(ReadinessLog),
    /// External command check (exit 0 = ready).
    #[serde(rename = "command")]
    Command(ReadinessCommand),
    /// Simple delay before considering ready.
    #[serde(rename = "delay")]
    Delay(ReadinessDelay),
}

/// Common readiness probe fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReadinessCommon {
    /// Time between probe attempts (e.g., "500ms").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Max time to reach ready (e.g., "60s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Initial delay before first probe (e.g., "0s").
    #[serde(
        default,
        rename = "initialDelay",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_delay: Option<String>,
}

/// TCP port readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessPort {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// TCP port on localhost.
    pub port: u16,
    /// Host to connect to (default: 127.0.0.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// HTTP readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessHttp {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// URL to check.
    pub url: String,
    /// Expected status codes (default: 2xx).
    #[serde(
        default,
        rename = "expectStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub expect_status: Option<Vec<u16>>,
    /// HTTP method (default: GET).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// Log pattern readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessLog {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// Regex pattern — first match declares ready.
    pub pattern: String,
    /// Which stream to watch (default: "either").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// External command readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessCommand {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// Command to run (exit 0 = ready).
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Simple delay readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessDelay {
    /// Duration to wait before considering ready.
    pub delay: String,
}

impl Readiness {
    /// Access the common probe fields shared by all readiness types.
    ///
    /// Returns `None` for `Delay`, which has no common fields.
    #[must_use]
    pub fn common_fields(&self) -> Option<&ReadinessCommon> {
        match self {
            Self::Port(p) => Some(&p.common),
            Self::Http(h) => Some(&h.common),
            Self::Log(l) => Some(&l.common),
            Self::Command(c) => Some(&c.common),
            Self::Delay(_) => None,
        }
    }
}

/// Restart policy for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestartPolicy {
    /// Restart mode (default: "onFailure").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Exponential backoff between restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<BackoffConfig>,
    /// Max restarts within the sliding window (default: 5).
    #[serde(
        default,
        rename = "maxRestarts",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_restarts: Option<u32>,
    /// Sliding window for restart counting (default: "60s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

/// Exponential backoff configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackoffConfig {
    /// Initial delay (default: "1s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    /// Maximum delay (default: "30s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    /// Backoff multiplier (default: 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
}

/// File watcher configuration for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceWatch {
    /// Glob patterns relative to project root.
    pub paths: Vec<String>,
    /// Patterns to ignore (gitignore syntax).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    /// Debounce window (default: "200ms").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<String>,
    /// Action on change (default: "restart").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Tasks to re-run before restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<Vec<TaskDependency>>,
}

/// Service log configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceLogs {
    /// Stream prefix shown in multiplexed output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// ANSI color hint for renderers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Persist to file (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<bool>,
}

/// Shutdown behavior for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shutdown {
    /// Signal to send (default: "SIGTERM").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Grace period before SIGKILL (default: "10s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
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

    /// Cuenv-managed VCS dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vcs: HashMap<String, VcsDependency>,

    /// CI configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CI>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskNode>,

    /// Services configuration — long-running supervised processes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub services: HashMap<String, Service>,

    /// Container image build definitions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub images: HashMap<String, ContainerImage>,

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
            TaskNode::Sequence(steps) => {
                for sub_node in steps {
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
#[path = "manifest_tests.rs"]
mod tests;
