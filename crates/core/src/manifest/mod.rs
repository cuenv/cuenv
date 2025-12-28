//! Root Project configuration type
//!
//! Based on schema/core.cue

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::hooks::Hook;
use crate::tasks::{Input, Mapping, ProjectReference, TaskGroup};
use crate::tasks::{Task, TaskDefinition};

/// Workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    /// Enable or disable the workspace
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional: manually specify the root of the workspace relative to env.cue
    pub root: Option<String>,

    /// Optional: manually specify the package manager
    pub package_manager: Option<String>,

    /// Workspace lifecycle hooks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<WorkspaceHooks>,

    /// Commands that trigger auto-association to this workspace.
    /// Any task with a matching command will automatically use this workspace.
    #[serde(default)]
    pub commands: Vec<String>,

    /// Tasks to inject automatically when this workspace is enabled.
    /// Keys become task names prefixed with workspace name (e.g., "bun.install").
    #[serde(default)]
    pub inject: HashMap<String, Task>,
}

/// Workspace lifecycle hooks for pre/post install
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHooks {
    /// Tasks or references to run before workspace install
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_install: Option<Vec<HookItem>>,

    /// Tasks or references to run after workspace install
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_install: Option<Vec<HookItem>>,
}

/// A hook step to run as part of workspace lifecycle hooks.
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

/// Match tasks across workspace by metadata for discovery-based execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskMatcher {
    /// Limit to specific workspaces (by name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<String>>,

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

/// Collection of hooks that can be executed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Hooks {
    /// Named hooks to execute when entering an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onEnter")]
    pub on_enter: Option<HashMap<String, Hook>>,

    /// Named hooks to execute when exiting an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onExit")]
    pub on_exit: Option<HashMap<String, Hook>>,
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

    /// Workspaces configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,
}

/// Ignore patterns for tool-specific ignore files.
/// Keys are tool names (e.g., "git", "docker", "prettier").
/// Values can be either:
/// - A list of patterns: `["node_modules/", ".env"]`
/// - An object with patterns and optional filename override
pub type Ignore = HashMap<String, IgnoreValue>;

// ============================================================================
// Cube Types (for code generation)
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

/// A file definition from the cube
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

/// A CUE Cube containing file definitions for code generation
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CubeConfig {
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
    /// OCI-based binary fetching (e.g., Homebrew bottles)
    Oci(OciRuntime),
    /// Multi-source tool management (Homebrew, GitHub, OCI, Nix)
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
/// Homebrew bottles (ghcr.io/homebrew/*) are auto-detected and extracted.
/// Other images require explicit `extract` paths.
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
/// Homebrew bottles (ghcr.io/homebrew/*) are auto-detected and extracted.
/// Other images require explicit `extract` paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OciImage {
    /// Full image reference (e.g., "ghcr.io/homebrew/core/jq:1.7.1", "nginx:1.25-alpine")
    pub image: String,
    /// Rename the extracted binary (for Homebrew bottles where package != binary name)
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Explicit extraction paths (required for non-Homebrew images)
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

/// Multi-source tool runtime configuration.
///
/// Provides ergonomic tool management with platform-specific overrides.
/// Simple case: `jq: "1.7.1"` uses Homebrew.
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
    /// Simple version string (uses Homebrew by default)
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
    /// Fetch from Homebrew bottles (ghcr.io/homebrew)
    Homebrew {
        /// Formula name (defaults to tool name)
        #[serde(skip_serializing_if = "Option::is_none")]
        formula: Option<String>,
    },
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
        /// Release tag (defaults to "v{version}")
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

    /// Workspaces configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,

    /// CI configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CI>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskDefinition>,

    /// Cube configuration for code generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cube: Option<CubeConfig>,

    /// Runtime configuration (project-level default for all tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,
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

    /// Inject implicit tasks and dependencies based on workspace declarations.
    ///
    /// When a workspace is declared (e.g., `workspaces: bun: #BunWorkspace`), this method:
    /// 1. Auto-associates tasks to workspaces based on their command matching workspace's `commands`
    /// 2. Injects tasks from the workspace's `inject` field
    ///
    /// This ensures users don't need to manually define common tasks like
    /// `bun.install` or manually wire up dependencies.
    pub fn with_implicit_tasks(mut self) -> Self {
        let Some(workspaces) = &self.workspaces else {
            return self;
        };

        // Clone workspaces to avoid borrow issues
        let workspaces = workspaces.clone();

        // Build command -> workspace mapping from config
        let mut command_to_workspace: HashMap<String, String> = HashMap::new();
        for (ws_name, config) in &workspaces {
            if !config.enabled {
                continue;
            }
            for cmd in &config.commands {
                command_to_workspace.insert(cmd.clone(), ws_name.clone());
            }
        }

        // Auto-associate tasks based on command
        for task_def in self.tasks.values_mut() {
            Self::auto_associate_by_command(task_def, &command_to_workspace);
        }

        // Inject tasks from workspace definitions
        for (ws_name, config) in &workspaces {
            if !config.enabled {
                continue;
            }

            for (task_name, inject_task) in &config.inject {
                let full_name = format!("{}.{}", ws_name, task_name);

                // Don't override user-defined tasks
                if self.tasks.contains_key(&full_name) {
                    continue;
                }

                // Clone and set workspace on injected task
                let mut task = inject_task.clone();
                task.workspaces = Some(vec![ws_name.clone()]);

                self.tasks
                    .insert(full_name, TaskDefinition::Single(Box::new(task)));
            }
        }

        self
    }

    /// Recursively auto-associate workspaces to tasks based on command matching.
    fn auto_associate_by_command(
        task_def: &mut TaskDefinition,
        command_to_workspace: &HashMap<String, String>,
    ) {
        match task_def {
            TaskDefinition::Single(task) => {
                // Only auto-associate if workspaces is None (not specified)
                // If Some([]) or Some([...]), user explicitly set it - don't modify
                if task.workspaces.is_some() {
                    return;
                }

                // Check if task's command matches any workspace
                if let Some(ws_name) = command_to_workspace.get(&task.command) {
                    task.workspaces = Some(vec![ws_name.clone()]);
                }
            }
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for sub in tasks {
                        Self::auto_associate_by_command(sub, command_to_workspace);
                    }
                }
                TaskGroup::Parallel(parallel) => {
                    for sub in parallel.tasks.values_mut() {
                        Self::auto_associate_by_command(sub, command_to_workspace);
                    }
                }
            },
        }
    }

    /// Expand shorthand cross-project references in inputs and implicit dependencies.
    ///
    /// Handles inputs in the format: "#project:task:path/to/file"
    /// Converts them to explicit ProjectReference inputs.
    /// Also adds implicit dependsOn entries for all project references.
    pub fn expand_cross_project_references(&mut self) {
        for (_, task_def) in self.tasks.iter_mut() {
            Self::expand_task_definition(task_def);
        }
    }

    fn expand_task_definition(task_def: &mut TaskDefinition) {
        match task_def {
            TaskDefinition::Single(task) => Self::expand_task(task),
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for sub_task in tasks {
                        Self::expand_task_definition(sub_task);
                    }
                }
                TaskGroup::Parallel(group) => {
                    for sub_task in group.tasks.values_mut() {
                        Self::expand_task_definition(sub_task);
                    }
                }
            },
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
            if !task.depends_on.contains(&dep) {
                task.depends_on.push(dep);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::ParallelGroup;
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
            .insert("deploy".into(), TaskDefinition::Single(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("deploy").unwrap();
        let task = task_def.as_single().unwrap();

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
        assert_eq!(task.depends_on[0], "#myproj:build");
    }

    // ============================================================================
    // Auto-association and Inject Tests
    // ============================================================================

    #[test]
    fn test_auto_associate_by_command() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string(), "bunx".to_string()],
                inject: HashMap::new(),
            },
        )]));

        // Add a task with command "bun" and no explicit workspaces
        cuenv.tasks.insert(
            "dev".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string(), "dev".to_string()],
                workspaces: None, // Not specified - should auto-associate
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Task should now have bun workspace associated
        let task_def = cuenv.tasks.get("dev").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.workspaces, Some(vec!["bun".to_string()]));
    }

    #[test]
    fn test_auto_associate_bunx_command() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string(), "bunx".to_string()],
                inject: HashMap::new(),
            },
        )]));

        // Add a task with command "bunx"
        cuenv.tasks.insert(
            "codegen".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bunx".to_string(),
                args: vec!["prisma".to_string(), "generate".to_string()],
                workspaces: None,
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Task should have bun workspace associated via bunx command
        let task_def = cuenv.tasks.get("codegen").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.workspaces, Some(vec!["bun".to_string()]));
    }

    #[test]
    fn test_auto_associate_opt_out_with_empty_array() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::new(),
            },
        )]));

        // Add a task with explicit empty workspaces (opt-out)
        cuenv.tasks.insert(
            "standalone".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string(), "standalone".to_string()],
                workspaces: Some(vec![]), // Explicit empty = opt-out
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Task should still have empty workspaces (not auto-associated)
        let task_def = cuenv.tasks.get("standalone").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.workspaces, Some(vec![]));
    }

    #[test]
    fn test_auto_associate_explicit_workspace_unchanged() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::new(),
            },
        )]));

        // Add a task with explicit different workspace
        cuenv.tasks.insert(
            "task".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string()],
                workspaces: Some(vec!["other".to_string()]), // Explicit
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Task should keep its explicit workspace
        let task_def = cuenv.tasks.get("task").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.workspaces, Some(vec!["other".to_string()]));
    }

    #[test]
    fn test_inject_creates_task() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::from([(
                    "install".to_string(),
                    Task {
                        command: "bun".to_string(),
                        args: vec!["install".to_string()],
                        hermetic: false,
                        ..Default::default()
                    },
                )]),
            },
        )]));

        let cuenv = cuenv.with_implicit_tasks();

        // Injected task should exist
        assert!(cuenv.tasks.contains_key("bun.install"));

        let task_def = cuenv.tasks.get("bun.install").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.command, "bun");
        assert_eq!(task.args, vec!["install"]);
        assert_eq!(task.workspaces, Some(vec!["bun".to_string()]));
    }

    #[test]
    fn test_inject_does_not_override_user_task() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::from([(
                    "install".to_string(),
                    Task {
                        command: "bun".to_string(),
                        args: vec!["install".to_string()],
                        ..Default::default()
                    },
                )]),
            },
        )]));

        // User defines their own bun.install task
        cuenv.tasks.insert(
            "bun.install".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "custom-bun".to_string(),
                args: vec!["custom-install".to_string()],
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // User's task should not be overridden
        let task_def = cuenv.tasks.get("bun.install").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.command, "custom-bun");
    }

    #[test]
    fn test_disabled_workspace_no_inject() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: false, // Disabled
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::from([(
                    "install".to_string(),
                    Task {
                        command: "bun".to_string(),
                        args: vec!["install".to_string()],
                        ..Default::default()
                    },
                )]),
            },
        )]));

        let cuenv = cuenv.with_implicit_tasks();
        assert!(!cuenv.tasks.contains_key("bun.install"));
    }

    #[test]
    fn test_disabled_workspace_no_auto_associate() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: false, // Disabled
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::new(),
            },
        )]));

        cuenv.tasks.insert(
            "dev".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string(), "dev".to_string()],
                workspaces: None,
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Task should NOT be auto-associated (workspace disabled)
        let task_def = cuenv.tasks.get("dev").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.workspaces, None);
    }

    #[test]
    fn test_no_workspaces_unchanged() {
        let cuenv = Project::new("test");
        let cuenv = cuenv.with_implicit_tasks();
        assert!(cuenv.tasks.is_empty());
    }

    #[test]
    fn test_nested_task_groups_auto_associate() {
        let mut cuenv = Project::new("test");
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: vec!["bun".to_string()],
                inject: HashMap::new(),
            },
        )]));

        // Create a parallel group with bun tasks
        cuenv.tasks.insert(
            "build".into(),
            TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                tasks: HashMap::from([
                    (
                        "frontend".into(),
                        TaskDefinition::Single(Box::new(Task {
                            command: "bun".to_string(),
                            args: vec!["run".to_string(), "build:frontend".to_string()],
                            workspaces: None,
                            ..Default::default()
                        })),
                    ),
                    (
                        "backend".into(),
                        TaskDefinition::Single(Box::new(Task {
                            command: "cargo".to_string(), // Different command
                            args: vec!["build".to_string()],
                            workspaces: None,
                            ..Default::default()
                        })),
                    ),
                ]),
                depends_on: vec![],
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Check nested task got auto-associated
        let build_def = cuenv.tasks.get("build").unwrap();
        if let TaskDefinition::Group(TaskGroup::Parallel(group)) = build_def {
            let frontend = group.tasks.get("frontend").unwrap();
            if let TaskDefinition::Single(task) = frontend {
                assert_eq!(task.workspaces, Some(vec!["bun".to_string()]));
            } else {
                panic!("Expected Single task");
            }

            let backend = group.tasks.get("backend").unwrap();
            if let TaskDefinition::Single(task) = backend {
                // cargo command not in bun workspace commands, should remain None
                assert_eq!(task.workspaces, None);
            } else {
                panic!("Expected Single task");
            }
        } else {
            panic!("Expected Parallel group");
        }
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
    fn test_workspace_hooks_before_install() {
        let json = format!(
            r#"{{
            "beforeInstall": [
                {{"ref": "{}"}},
                {{"name": "codegen", "match": {{"labels": ["codegen"]}}}},
                {{"command": "echo", "args": ["ready"]}}
            ]
        }}"#,
            "#projen:types"
        );
        let hooks: WorkspaceHooks = serde_json::from_str(&json).unwrap();

        let before_install = hooks.before_install.unwrap();
        assert_eq!(before_install.len(), 3);

        // First item: TaskRef
        match &before_install[0] {
            HookItem::TaskRef(task_ref) => {
                assert_eq!(task_ref.ref_, "#projen:types");
            }
            _ => panic!("Expected TaskRef"),
        }

        // Second item: Match
        match &before_install[1] {
            HookItem::Match(match_hook) => {
                assert_eq!(match_hook.name, Some("codegen".to_string()));
            }
            _ => panic!("Expected Match"),
        }

        // Third item: Inline Task
        match &before_install[2] {
            HookItem::Task(task) => {
                assert_eq!(task.command, "echo");
            }
            _ => panic!("Expected Task"),
        }
    }

    #[test]
    fn test_workspace_hooks_after_install() {
        let json = r#"{
            "afterInstall": [
                {"command": "prisma", "args": ["generate"]}
            ]
        }"#;
        let hooks: WorkspaceHooks = serde_json::from_str(json).unwrap();

        assert!(hooks.before_install.is_none());
        let after_install = hooks.after_install.unwrap();
        assert_eq!(after_install.len(), 1);

        match &after_install[0] {
            HookItem::Task(task) => {
                assert_eq!(task.command, "prisma");
                assert_eq!(task.args, vec!["generate"]);
            }
            _ => panic!("Expected Task"),
        }
    }

    #[test]
    fn test_workspace_config_with_hooks() {
        let json = format!(
            r#"{{
            "enabled": true,
            "hooks": {{
                "beforeInstall": [
                    {{"ref": "{}"}}
                ]
            }}
        }}"#,
            "#generator:types"
        );
        let config: WorkspaceConfig = serde_json::from_str(&json).unwrap();

        assert!(config.enabled);
        assert!(config.hooks.is_some());

        let hooks = config.hooks.unwrap();
        let before_install = hooks.before_install.unwrap();
        assert_eq!(before_install.len(), 1);
    }

    #[test]
    fn test_task_matcher_deserialization() {
        let json = r#"{
            "workspaces": ["packages/lib"],
            "labels": ["projen", "codegen"],
            "parallel": true
        }"#;
        let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

        assert_eq!(matcher.workspaces, Some(vec!["packages/lib".to_string()]));
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

        assert!(matcher.workspaces.is_none());
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
    // WorkspaceHooks with Project Integration Tests
    // ============================================================================

    #[test]
    fn test_cuenv_workspace_with_before_install_hooks() {
        let json = format!(
            r#"{{
            "name": "test-project",
            "workspaces": {{
                "bun": {{
                    "enabled": true,
                    "hooks": {{
                        "beforeInstall": [
                            {{"ref": "{}"}},
                            {{"command": "sh", "args": ["-c", "echo setup"]}}
                        ]
                    }}
                }}
            }},
            "tasks": {{
                "dev": {{
                    "command": "bun",
                    "args": ["run", "dev"],
                    "workspaces": ["bun"]
                }}
            }}
        }}"#,
            "#generator:types"
        );
        let cuenv: Project = serde_json::from_str(&json).unwrap();

        assert_eq!(cuenv.name, "test-project");
        let workspaces = cuenv.workspaces.unwrap();
        let bun_config = workspaces.get("bun").unwrap();

        assert!(bun_config.enabled);
        let hooks = bun_config.hooks.as_ref().unwrap();
        let before_install = hooks.before_install.as_ref().unwrap();
        assert_eq!(before_install.len(), 2);
    }

    #[test]
    fn test_cuenv_multiple_workspaces_with_hooks() {
        let json = format!(
            r#"{{
            "name": "multi-workspace",
            "workspaces": {{
                "bun": {{
                    "enabled": true,
                    "hooks": {{
                        "beforeInstall": [{{"ref": "{}"}}]
                    }}
                }},
                "cargo": {{
                    "enabled": true,
                    "hooks": {{
                        "beforeInstall": [{{"command": "cargo", "args": ["generate"]}}]
                    }}
                }}
            }},
            "tasks": {{}}
        }}"#,
            "#projen:types"
        );
        let cuenv: Project = serde_json::from_str(&json).unwrap();

        let workspaces = cuenv.workspaces.unwrap();
        assert!(workspaces.contains_key("bun"));
        assert!(workspaces.contains_key("cargo"));

        // Verify bun hooks
        let bun_hooks = workspaces["bun"].hooks.as_ref().unwrap();
        assert!(bun_hooks.before_install.is_some());

        // Verify cargo hooks
        let cargo_hooks = workspaces["cargo"].hooks.as_ref().unwrap();
        assert!(cargo_hooks.before_install.is_some());
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
            .insert("bundle".into(), TaskDefinition::Single(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("bundle").unwrap();
        let task = task_def.as_single().unwrap();

        // Should have 3 inputs (2 project refs + 1 local)
        assert_eq!(task.inputs.len(), 3);

        // Should have 2 implicit dependencies
        assert_eq!(task.depends_on.len(), 2);
        assert!(task.depends_on.contains(&"#projA:build".to_string()));
        assert!(task.depends_on.contains(&"#projB:compile".to_string()));
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
            TaskDefinition::Group(TaskGroup::Sequential(vec![
                TaskDefinition::Single(Box::new(task1)),
                TaskDefinition::Single(Box::new(task2)),
            ])),
        );

        cuenv.expand_cross_project_references();

        // Verify expansion happened in both tasks
        match cuenv.tasks.get("pipeline").unwrap() {
            TaskDefinition::Group(TaskGroup::Sequential(tasks)) => {
                match &tasks[0] {
                    TaskDefinition::Single(task) => {
                        assert!(task.depends_on.contains(&"#projA:build".to_string()));
                    }
                    _ => panic!("Expected single task"),
                }
                match &tasks[1] {
                    TaskDefinition::Single(task) => {
                        assert!(task.depends_on.contains(&"#projB:compile".to_string()));
                    }
                    _ => panic!("Expected single task"),
                }
            }
            _ => panic!("Expected sequential group"),
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
        parallel_tasks.insert("a".to_string(), TaskDefinition::Single(Box::new(task1)));
        parallel_tasks.insert("b".to_string(), TaskDefinition::Single(Box::new(task2)));

        let mut cuenv = Project::new("test");
        cuenv.tasks.insert(
            "parallel".into(),
            TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                tasks: parallel_tasks,
                depends_on: vec![],
            })),
        );

        cuenv.expand_cross_project_references();

        // Verify expansion happened in both parallel tasks
        match cuenv.tasks.get("parallel").unwrap() {
            TaskDefinition::Group(TaskGroup::Parallel(group)) => {
                match group.tasks.get("a").unwrap() {
                    TaskDefinition::Single(task) => {
                        assert!(task.depends_on.contains(&"#projA:build".to_string()));
                    }
                    _ => panic!("Expected single task"),
                }
                match group.tasks.get("b").unwrap() {
                    TaskDefinition::Single(task) => {
                        assert!(task.depends_on.contains(&"#projB:build".to_string()));
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
            depends_on: vec!["#myproj:build".to_string()],
            inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
            ..Default::default()
        };

        let mut cuenv = Project::new("test");
        cuenv
            .tasks
            .insert("deploy".into(), TaskDefinition::Single(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("deploy").unwrap();
        let task = task_def.as_single().unwrap();

        // Should not duplicate the dependency
        assert_eq!(task.depends_on.len(), 1);
        assert_eq!(task.depends_on[0], "#myproj:build");
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
        // This test mimics the structure of cuenv's actual env.cue
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
                    "fix": {
                        "command": "treefmt",
                        "inputs": [".config"]
                    },
                    "check": {
                        "command": "treefmt",
                        "args": ["--fail-on-change"],
                        "inputs": [".config"]
                    }
                },
                "cross": {
                    "linux": {
                        "script": "echo building for linux",
                        "inputs": ["Cargo.toml"]
                    }
                },
                "docs": {
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
        }"#;

        let result: Result<Project, _> = serde_json::from_str(json);
        match result {
            Ok(project) => {
                assert_eq!(project.name, "cuenv");
                assert_eq!(project.tasks.len(), 5);
                assert!(project.tasks.contains_key("pwd"));
                assert!(project.tasks.contains_key("cross"));
                // Verify cross.linux is a script task
                let cross = project.tasks.get("cross").unwrap();
                assert!(cross.is_group());
            }
            Err(e) => {
                panic!("Failed to deserialize Project with script tasks: {}", e);
            }
        }
    }
}
