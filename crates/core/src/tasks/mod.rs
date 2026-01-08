//! Task execution and management module
//!
//! This module provides the core types for task execution, matching the CUE schema.
//!
//! # Task API v2
//!
//! Users annotate tasks with their type to unlock specific semantics:
//! - [`Task`]: Single command or script
//! - [`TaskGroup`]: Parallel execution (all children run concurrently)
//! - [`TaskList`]: Sequential execution (steps run in order)

pub mod backend;
pub mod executor;
pub mod graph;
pub mod index;
pub mod io;

// Re-export executor and graph modules
pub use backend::{
    BackendFactory, HostBackend, TaskBackend, create_backend, create_backend_with_factory,
    should_use_dagger,
};
pub use executor::*;
pub use graph::*;
pub use index::{IndexedTask, TaskIndex, TaskPath, WorkspaceTask};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

fn default_hermetic() -> bool {
    true
}

// =============================================================================
// Script Shell Configuration
// =============================================================================

/// Shell interpreter for script-based tasks
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScriptShell {
    #[default]
    Bash,
    Sh,
    Zsh,
    Fish,
    Powershell,
    Pwsh,
    Python,
    Node,
    Ruby,
    Perl,
}

impl ScriptShell {
    /// Get the command and flag for this shell
    #[must_use]
    pub fn command_and_flag(&self) -> (&'static str, &'static str) {
        match self {
            ScriptShell::Bash => ("bash", "-c"),
            ScriptShell::Sh => ("sh", "-c"),
            ScriptShell::Zsh => ("zsh", "-c"),
            ScriptShell::Fish => ("fish", "-c"),
            ScriptShell::Powershell => ("powershell", "-Command"),
            ScriptShell::Pwsh => ("pwsh", "-Command"),
            ScriptShell::Python => ("python", "-c"),
            ScriptShell::Node => ("node", "-e"),
            ScriptShell::Ruby => ("ruby", "-e"),
            ScriptShell::Perl => ("perl", "-e"),
        }
    }

    /// Returns true if this shell supports POSIX-style options (errexit, pipefail, etc.)
    #[must_use]
    pub fn supports_shell_options(&self) -> bool {
        matches!(self, ScriptShell::Bash | ScriptShell::Sh | ScriptShell::Zsh)
    }
}

/// Shell options for bash-like shells
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellOptions {
    /// -e: exit on error (default: true)
    #[serde(default = "default_true")]
    pub errexit: bool,
    /// -u: error on undefined vars (default: true)
    #[serde(default = "default_true")]
    pub nounset: bool,
    /// -o pipefail: fail on pipe errors (default: true)
    #[serde(default = "default_true")]
    pub pipefail: bool,
    /// -x: debug/trace mode (default: false)
    #[serde(default)]
    pub xtrace: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            errexit: true,
            nounset: true,
            pipefail: true,
            xtrace: false,
        }
    }
}

impl ShellOptions {
    /// Generate the shell options prefix for a script
    #[must_use]
    pub fn to_set_commands(&self) -> String {
        let mut opts = Vec::new();
        if self.errexit {
            opts.push("-e");
        }
        if self.nounset {
            opts.push("-u");
        }
        if self.pipefail {
            opts.push("-o pipefail");
        }
        if self.xtrace {
            opts.push("-x");
        }
        if opts.is_empty() {
            String::new()
        } else {
            format!("set {}\n", opts.join(" "))
        }
    }
}

/// Shell configuration for task execution (legacy, for backwards compatibility)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Shell {
    /// Shell executable name (e.g., "bash", "fish", "zsh")
    pub command: Option<String>,
    /// Flag for command execution (e.g., "-c", "--command")
    pub flag: Option<String>,
}

/// Mapping of external output to local workspace path
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    /// Path relative to external project root of a declared output from the external task
    pub from: String,
    /// Path inside the dependent task’s hermetic workspace where this file/dir will be materialized
    pub to: String,
}

/// A single task input definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Input {
    /// Local path/glob input
    Path(String),
    /// Cross-project reference
    Project(ProjectReference),
    /// Same-project task output reference
    Task(TaskOutput),
}

impl Input {
    pub fn as_path(&self) -> Option<&String> {
        match self {
            Input::Path(path) => Some(path),
            Input::Project(_) | Input::Task(_) => None,
        }
    }

    pub fn as_project(&self) -> Option<&ProjectReference> {
        match self {
            Input::Project(reference) => Some(reference),
            Input::Path(_) | Input::Task(_) => None,
        }
    }

    pub fn as_task_output(&self) -> Option<&TaskOutput> {
        match self {
            Input::Task(output) => Some(output),
            Input::Path(_) | Input::Project(_) => None,
        }
    }
}

/// Cross-project input declaration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectReference {
    /// Path to project root relative to env.cue or absolute-from-repo-root
    pub project: String,
    /// Name of the external task in that project
    pub task: String,
    /// Required explicit mappings
    pub map: Vec<Mapping>,
}

/// Reference to another task's outputs within the same project
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskOutput {
    /// Name of the task whose cached outputs to consume (e.g. "docs.build")
    pub task: String,
    /// Optional explicit mapping of outputs. If omitted, all outputs are
    /// materialized at their original paths in the hermetic workspace.
    #[serde(default)]
    pub map: Option<Vec<Mapping>>,
}

/// Source location metadata from CUE evaluation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SourceLocation {
    /// Path to source file, relative to cue.mod root
    pub file: String,
    /// Line number where the task was defined
    pub line: u32,
    /// Column number where the task was defined
    pub column: u32,
}

impl SourceLocation {
    /// Get the directory containing this source file
    pub fn directory(&self) -> Option<&str> {
        std::path::Path::new(&self.file)
            .parent()
            .and_then(|p| p.to_str())
            .filter(|s| !s.is_empty())
    }
}

/// A task dependency - an embedded task reference with _name field
/// When tasks reference other tasks directly in CUE (e.g., `dependsOn: [build]`),
/// the Go bridge injects the `_name` field to identify the dependency.
///
/// Supports deserialization from:
/// - A string: `"taskName"` -> `TaskDependency { name: "taskName" }`
/// - An object with `_name`: `{ "_name": "taskName", ... }` -> `TaskDependency { name: "taskName" }`
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TaskDependency {
    /// The task name (injected by Go bridge based on task path)
    /// e.g., "build", "test.unit", "deploy.staging"
    #[serde(rename = "_name")]
    pub name: String,

    // Other fields are captured but not used - we only need the name
    #[serde(flatten)]
    _rest: serde_json::Value,
}

impl<'de> serde::Deserialize<'de> for TaskDependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct TaskDependencyVisitor;

        impl<'de> Visitor<'de> for TaskDependencyVisitor {
            type Value = TaskDependency;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or an object with _name field")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(TaskDependency::from_name(value))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(TaskDependency::from_name(value))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                // Deserialize as a JSON object and extract _name
                let value: serde_json::Value =
                    serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                let name = value
                    .get("_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| de::Error::missing_field("_name"))?
                    .to_string();

                Ok(TaskDependency { name, _rest: value })
            }
        }

        deserializer.deserialize_any(TaskDependencyVisitor)
    }
}

impl TaskDependency {
    /// Create a new TaskDependency from a task name
    #[must_use]
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            _rest: serde_json::Value::Null,
        }
    }

    /// Get the task name
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }

    /// Check if this dependency matches a given task name
    pub fn matches(&self, name: &str) -> bool {
        self.name == name
    }
}

// =============================================================================
// Single Executable Task
// =============================================================================

/// A single executable task
///
/// Note: Custom deserialization is used to ensure that a Task can only be
/// deserialized when it has a `command` or `script` field. This is necessary
/// because TaskNode uses untagged enum, and we need to distinguish
/// between Task, TaskGroup, and TaskList during deserialization.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Task {
    /// Shell configuration for command execution (legacy, for backwards compatibility)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Shell>,

    /// Command to execute. Required unless 'script' is provided.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,

    /// Inline script to execute (alternative to command).
    /// When script is provided, shell defaults to bash if not specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Shell interpreter for script-based tasks (e.g., bash, python, node)
    /// Only used when `script` is provided.
    #[serde(
        default,
        rename = "scriptShell",
        skip_serializing_if = "Option::is_none"
    )]
    pub script_shell: Option<ScriptShell>,

    /// Shell options for bash-like shells (errexit, nounset, pipefail, xtrace)
    /// Only used when `script` is provided with a POSIX-compatible shell.
    #[serde(
        default,
        rename = "shellOptions",
        skip_serializing_if = "Option::is_none"
    )]
    pub shell_options: Option<ShellOptions>,

    /// Arguments for the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables for this task
    #[serde(default)]
    pub env: HashMap<String, serde_json::Value>,

    /// Dagger-specific configuration for running this task in a container
    /// DEPRECATED: Use runtime field with Dagger variant instead
    #[serde(default)]
    pub dagger: Option<DaggerTaskConfig>,

    /// Runtime override for this task (inherits from project if not set)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::manifest::Runtime>,

    /// When true (default), task runs in isolated hermetic directory.
    /// When false, task runs directly in workspace/project root.
    #[serde(default = "default_hermetic")]
    pub hermetic: bool,

    /// Task dependencies - embedded task references with _name field
    /// In CUE, users write `dependsOn: [build, test]` with direct references.
    /// The Go bridge injects _name into each embedded task for identification.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Input files/resources
    #[serde(default)]
    pub inputs: Vec<Input>,

    /// Output files/resources
    #[serde(default)]
    pub outputs: Vec<String>,

    /// Description of the task
    #[serde(default)]
    pub description: Option<String>,

    /// Task parameter definitions for CLI arguments
    #[serde(default)]
    pub params: Option<TaskParams>,

    /// Labels for task discovery via TaskMatcher
    /// Example: labels: ["projen", "codegen"]
    #[serde(default)]
    pub labels: Vec<String>,

    /// Execution timeout (e.g., "30m", "1h")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Retry configuration for failed tasks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    /// Continue execution even if this task fails (default: false)
    #[serde(default, rename = "continueOnError")]
    pub continue_on_error: bool,

    /// If set, this task is a reference to another project's task
    /// that should be resolved at runtime using TaskDiscovery.
    /// Format: "#project-name:task-name"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<String>,

    /// If set, specifies the project root where this task should execute.
    /// Used for TaskRef resolution to run tasks in their original project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<std::path::PathBuf>,

    /// Source file location where this task was defined (from CUE metadata).
    /// Used to determine default execution directory and for task listing grouping.
    #[serde(default, rename = "_source", skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,

    /// Working directory override (relative to cue.mod root).
    /// Defaults to the directory of the source file if not set.
    #[serde(default, rename = "dir", skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

/// Retry configuration for failed tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryConfig {
    /// Number of retry attempts (default: 3)
    #[serde(default = "default_retry_attempts")]
    pub attempts: u32,
    /// Delay between retries (e.g., "5s")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
}

fn default_retry_attempts() -> u32 {
    3
}

// Custom deserialization for Task to ensure either command or script is present.
// This is necessary for untagged enum deserialization in TaskNode to work correctly.
impl<'de> serde::Deserialize<'de> for Task {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper struct that mirrors Task but with all optional fields
        #[derive(serde::Deserialize)]
        struct TaskHelper {
            #[serde(default)]
            shell: Option<Shell>,
            #[serde(default)]
            command: Option<String>,
            #[serde(default)]
            script: Option<String>,
            #[serde(default, rename = "scriptShell")]
            script_shell: Option<ScriptShell>,
            #[serde(default, rename = "shellOptions")]
            shell_options: Option<ShellOptions>,
            #[serde(default)]
            args: Vec<String>,
            #[serde(default)]
            env: HashMap<String, serde_json::Value>,
            #[serde(default)]
            dagger: Option<DaggerTaskConfig>,
            #[serde(default)]
            runtime: Option<crate::manifest::Runtime>,
            #[serde(default = "default_hermetic")]
            hermetic: bool,
            #[serde(default, rename = "dependsOn")]
            depends_on: Vec<TaskDependency>,
            #[serde(default)]
            inputs: Vec<Input>,
            #[serde(default)]
            outputs: Vec<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            params: Option<TaskParams>,
            #[serde(default)]
            labels: Vec<String>,
            #[serde(default)]
            timeout: Option<String>,
            #[serde(default)]
            retry: Option<RetryConfig>,
            #[serde(default, rename = "continueOnError")]
            continue_on_error: bool,
            #[serde(default)]
            task_ref: Option<String>,
            #[serde(default)]
            project_root: Option<std::path::PathBuf>,
            #[serde(default, rename = "_source")]
            source: Option<SourceLocation>,
            #[serde(default, rename = "dir")]
            directory: Option<String>,
        }

        let helper = TaskHelper::deserialize(deserializer)?;

        // Validate: either command, script, or task_ref must be present
        let has_command = helper.command.as_ref().is_some_and(|c| !c.is_empty());
        let has_script = helper.script.is_some();
        let has_task_ref = helper.task_ref.is_some();

        if !has_command && !has_script && !has_task_ref {
            return Err(serde::de::Error::custom(
                "Task must have either 'command', 'script', or 'task_ref' field",
            ));
        }

        Ok(Task {
            shell: helper.shell,
            command: helper.command.unwrap_or_default(),
            script: helper.script,
            script_shell: helper.script_shell,
            shell_options: helper.shell_options,
            args: helper.args,
            env: helper.env,
            dagger: helper.dagger,
            runtime: helper.runtime,
            hermetic: helper.hermetic,
            depends_on: helper.depends_on,
            inputs: helper.inputs,
            outputs: helper.outputs,
            description: helper.description,
            params: helper.params,
            labels: helper.labels,
            timeout: helper.timeout,
            retry: helper.retry,
            continue_on_error: helper.continue_on_error,
            task_ref: helper.task_ref,
            project_root: helper.project_root,
            source: helper.source,
            directory: helper.directory,
        })
    }
}

/// Dagger-specific task configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DaggerTaskConfig {
    /// Base container image for running the task (e.g., "ubuntu:22.04")
    /// Overrides the global backend.options.image if set.
    #[serde(default)]
    pub image: Option<String>,

    /// Use container from a previous task as base instead of an image.
    /// The referenced task must have run first (use dependsOn to ensure ordering).
    #[serde(default)]
    pub from: Option<String>,

    /// Secrets to mount or expose as environment variables.
    /// Secrets are resolved using cuenv's secret resolvers and securely passed to Dagger.
    #[serde(default)]
    pub secrets: Option<Vec<DaggerSecret>>,

    /// Cache volumes to mount for persistent build caching.
    #[serde(default)]
    pub cache: Option<Vec<DaggerCacheMount>>,
}

/// Secret configuration for Dagger containers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaggerSecret {
    /// Name identifier for the secret in Dagger
    pub name: String,

    /// Mount secret as a file at this path (e.g., "/root/.npmrc")
    #[serde(default)]
    pub path: Option<String>,

    /// Expose secret as an environment variable with this name
    #[serde(default, rename = "envVar")]
    pub env_var: Option<String>,

    /// Secret resolver configuration
    pub resolver: crate::secrets::Secret,
}

/// Cache volume mount configuration for Dagger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerCacheMount {
    /// Path inside the container to mount the cache (e.g., "/root/.npm")
    pub path: String,

    /// Unique name for the cache volume
    pub name: String,
}

/// Task parameter definitions for CLI arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskParams {
    /// Positional arguments (order matters, consumed left-to-right)
    /// Referenced in args as {{0}}, {{1}}, etc.
    #[serde(default)]
    pub positional: Vec<ParamDef>,

    /// Named arguments (--flag style) as direct fields
    /// Referenced in args as {{name}} where name matches the field name
    #[serde(flatten, default)]
    pub named: HashMap<String, ParamDef>,
}

/// Parameter type for validation
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    #[default]
    String,
    Bool,
    Int,
}

/// Parameter definition for task arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ParamDef {
    /// Human-readable description shown in --help
    #[serde(default)]
    pub description: Option<String>,

    /// Whether the argument must be provided (default: false)
    #[serde(default)]
    pub required: bool,

    /// Default value if not provided
    #[serde(default)]
    pub default: Option<String>,

    /// Type hint for documentation (default: "string", not enforced at runtime)
    #[serde(default, rename = "type")]
    pub param_type: ParamType,

    /// Short flag (single character, e.g., "t" for -t)
    #[serde(default)]
    pub short: Option<String>,
}

/// Resolved task arguments ready for interpolation
#[derive(Debug, Clone, Default)]
pub struct ResolvedArgs {
    /// Positional argument values by index
    pub positional: Vec<String>,
    /// Named argument values by name
    pub named: HashMap<String, String>,
}

impl ResolvedArgs {
    /// Create empty resolved args
    pub fn new() -> Self {
        Self::default()
    }

    /// Interpolate placeholders in a string
    /// Supports {{0}}, {{1}} for positional and {{name}} for named args
    pub fn interpolate(&self, template: &str) -> String {
        let mut result = template.to_string();

        // Replace positional placeholders {{0}}, {{1}}, etc.
        for (i, value) in self.positional.iter().enumerate() {
            let placeholder = format!("{{{{{}}}}}", i);
            result = result.replace(&placeholder, value);
        }

        // Replace named placeholders {{name}}
        for (name, value) in &self.named {
            let placeholder = format!("{{{{{}}}}}", name);
            result = result.replace(&placeholder, value);
        }

        result
    }

    /// Interpolate all args in a list
    pub fn interpolate_args(&self, args: &[String]) -> Vec<String> {
        args.iter().map(|arg| self.interpolate(arg)).collect()
    }
}

impl Default for Task {
    fn default() -> Self {
        Self {
            shell: None,
            command: String::new(),
            script: None,
            script_shell: None,
            shell_options: None,
            args: vec![],
            env: HashMap::new(),
            dagger: None,
            runtime: None,
            hermetic: true, // Default to hermetic execution
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            description: None,
            params: None,
            labels: vec![],
            timeout: None,
            retry: None,
            continue_on_error: false,
            task_ref: None,
            project_root: None,
            source: None,
            directory: None,
        }
    }
}

impl Task {
    /// Creates a new TaskRef placeholder task.
    /// This task will be resolved at runtime using TaskDiscovery.
    pub fn from_task_ref(ref_str: &str) -> Self {
        Self {
            task_ref: Some(ref_str.to_string()),
            description: Some(format!("Reference to {}", ref_str)),
            ..Default::default()
        }
    }

    /// Returns true if this task is a TaskRef placeholder that needs resolution.
    pub fn is_task_ref(&self) -> bool {
        self.task_ref.is_some()
    }

    /// Returns an iterator over dependency task names.
    pub fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.depends_on.iter().map(|d| d.task_name())
    }

    /// Returns the description, or a default if not set.
    pub fn description(&self) -> &str {
        self.description
            .as_deref()
            .unwrap_or("No description provided")
    }

    /// Returns an iterator over local path/glob inputs.
    pub fn iter_path_inputs(&self) -> impl Iterator<Item = &String> {
        self.inputs.iter().filter_map(Input::as_path)
    }

    /// Returns an iterator over project references.
    pub fn iter_project_refs(&self) -> impl Iterator<Item = &ProjectReference> {
        self.inputs.iter().filter_map(Input::as_project)
    }

    /// Returns an iterator over same-project task output references.
    pub fn iter_task_outputs(&self) -> impl Iterator<Item = &TaskOutput> {
        self.inputs.iter().filter_map(Input::as_task_output)
    }

    /// Collects path/glob inputs applying an optional prefix (for workspace roots).
    pub fn collect_path_inputs_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        self.iter_path_inputs()
            .map(|path| apply_prefix(prefix, path))
            .collect()
    }

    /// Collects mapped destinations from project references, applying an optional prefix.
    pub fn collect_project_destinations_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        self.iter_project_refs()
            .flat_map(|reference| reference.map.iter().map(|m| apply_prefix(prefix, &m.to)))
            .collect()
    }

    /// Collects all input patterns (local + project destinations) with an optional prefix.
    pub fn collect_all_inputs_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        let mut inputs = self.collect_path_inputs_with_prefix(prefix);
        inputs.extend(self.collect_project_destinations_with_prefix(prefix));
        inputs
    }
}

impl crate::AffectedBy for Task {
    /// Returns true if this task is affected by the given file changes.
    ///
    /// # Behavior
    ///
    /// - Tasks with NO inputs are always considered affected (we can't determine what affects them)
    /// - Tasks with inputs are affected if any input pattern matches changed files
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        let inputs: Vec<_> = self.iter_path_inputs().collect();

        // No inputs = always affected (we can't determine what affects it)
        if inputs.is_empty() {
            return true;
        }

        // Check if any input pattern matches any changed file
        inputs
            .iter()
            .any(|pattern| crate::matches_pattern(changed_files, project_root, pattern))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.iter_path_inputs().map(String::as_str).collect()
    }
}

fn apply_prefix(prefix: Option<&Path>, value: &str) -> String {
    if let Some(prefix) = prefix {
        prefix.join(value).to_string_lossy().to_string()
    } else {
        value.to_string()
    }
}

// =============================================================================
// Parallel Execution (Task Group)
// =============================================================================

/// A parallel task group - all children run concurrently
///
/// Discriminated by the required `type: "group"` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskGroup {
    /// Type discriminator - always "group"
    #[serde(rename = "type")]
    pub type_: String,

    /// Dependencies on other tasks
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Limit concurrent executions (0 = unlimited)
    #[serde(default, rename = "maxConcurrency")]
    pub max_concurrency: Option<u32>,

    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,

    /// Named children - all run concurrently (flattened from remaining fields)
    #[serde(flatten)]
    pub children: HashMap<String, TaskNode>,
}

// =============================================================================
// Sequential Execution (Task Sequence)
// =============================================================================

// TaskSequence is simply Vec<TaskNode> - no wrapper struct needed.
// The sequence is discriminated by being a JSON array.

// =============================================================================
// Task Node (Union Type)
// =============================================================================

/// Union of all task types - explicit typing required in CUE
///
/// This is the recursive type that represents any task node in the tree.
/// Discriminated by:
/// - [`Task`]: Has `command` or `script` field
/// - [`TaskGroup`]: Has `type: "group"` field
/// - Sequence: Is a JSON array `[...]`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskNode {
    /// A single executable task
    Task(Box<Task>),
    /// A parallel task group
    Group(TaskGroup),
    /// A sequential list of tasks (just an array)
    Sequence(Vec<TaskNode>),
}

// =============================================================================
// Legacy Type Aliases (for backwards compatibility)
// =============================================================================

/// Legacy alias for TaskNode
#[deprecated(since = "0.26.0", note = "Use TaskNode instead")]
pub type TaskDefinition = TaskNode;

/// Legacy alias for TaskList (now just Vec<TaskNode>)
#[deprecated(since = "0.26.0", note = "Use Vec<TaskNode> directly")]
pub type TaskList = Vec<TaskNode>;

/// Root tasks structure from CUE
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tasks {
    /// Map of task names to their definitions
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskNode>,
}

impl Tasks {
    /// Create a new empty tasks collection
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a task node by name
    pub fn get(&self, name: &str) -> Option<&TaskNode> {
        self.tasks.get(name)
    }

    /// List all task names
    pub fn list_tasks(&self) -> Vec<&str> {
        self.tasks.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a task exists
    pub fn contains(&self, name: &str) -> bool {
        self.tasks.contains_key(name)
    }
}

impl TaskNode {
    /// Check if this is a single task
    pub fn is_task(&self) -> bool {
        matches!(self, TaskNode::Task(_))
    }

    /// Check if this is a task group (parallel)
    pub fn is_group(&self) -> bool {
        matches!(self, TaskNode::Group(_))
    }

    /// Check if this is a sequence (sequential)
    pub fn is_sequence(&self) -> bool {
        matches!(self, TaskNode::Sequence(_))
    }

    /// Get as single task if it is one
    pub fn as_task(&self) -> Option<&Task> {
        match self {
            TaskNode::Task(task) => Some(task.as_ref()),
            _ => None,
        }
    }

    /// Get as task group if it is one
    pub fn as_group(&self) -> Option<&TaskGroup> {
        match self {
            TaskNode::Group(group) => Some(group),
            _ => None,
        }
    }

    /// Get as sequence if it is one
    pub fn as_sequence(&self) -> Option<&Vec<TaskNode>> {
        match self {
            TaskNode::Sequence(seq) => Some(seq),
            _ => None,
        }
    }

    /// Get dependencies for this node
    pub fn depends_on(&self) -> &[TaskDependency] {
        match self {
            TaskNode::Task(task) => &task.depends_on,
            TaskNode::Group(group) => &group.depends_on,
            TaskNode::Sequence(_) => &[], // Sequences don't have top-level deps
        }
    }

    /// Get description for this node
    pub fn description(&self) -> Option<&str> {
        match self {
            TaskNode::Task(task) => task.description.as_deref(),
            TaskNode::Group(group) => group.description.as_deref(),
            TaskNode::Sequence(_) => None, // Sequences don't have descriptions
        }
    }

    // Legacy compatibility methods
    #[deprecated(since = "0.26.0", note = "Use is_task() instead")]
    pub fn is_single(&self) -> bool {
        self.is_task()
    }

    #[deprecated(since = "0.26.0", note = "Use as_task() instead")]
    pub fn as_single(&self) -> Option<&Task> {
        self.as_task()
    }

    #[deprecated(since = "0.26.0", note = "Use is_sequence() instead")]
    pub fn is_list(&self) -> bool {
        self.is_sequence()
    }

    #[deprecated(since = "0.26.0", note = "Use as_sequence() instead")]
    pub fn as_list(&self) -> Option<&Vec<TaskNode>> {
        self.as_sequence()
    }
}

impl TaskGroup {
    /// Get the number of tasks in this group
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Check if the group is empty
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl crate::AffectedBy for TaskGroup {
    /// A group is affected if ANY of its subtasks are affected.
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        self.children
            .values()
            .any(|node| node.is_affected_by(changed_files, project_root))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.children
            .values()
            .flat_map(|node| node.input_patterns())
            .collect()
    }
}

impl crate::AffectedBy for TaskNode {
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        match self {
            TaskNode::Task(task) => task.is_affected_by(changed_files, project_root),
            TaskNode::Group(group) => group.is_affected_by(changed_files, project_root),
            TaskNode::Sequence(seq) => seq
                .iter()
                .any(|node| node.is_affected_by(changed_files, project_root)),
        }
    }

    fn input_patterns(&self) -> Vec<&str> {
        match self {
            TaskNode::Task(task) => task.input_patterns(),
            TaskNode::Group(group) => group.input_patterns(),
            TaskNode::Sequence(seq) => seq.iter().flat_map(|node| node.input_patterns()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_default_values() {
        let task = Task {
            command: "echo".to_string(),
            ..Default::default()
        };

        assert!(task.shell.is_none());
        assert_eq!(task.command, "echo");
        assert_eq!(task.description(), "No description provided");
        assert!(task.args.is_empty());
        assert!(task.hermetic); // default is true
    }

    #[test]
    fn test_task_deserialization() {
        let json = r#"{
            "command": "echo",
            "args": ["Hello", "World"]
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.command, "echo");
        assert_eq!(task.args, vec!["Hello", "World"]);
        assert!(task.shell.is_none()); // default value
    }

    #[test]
    fn test_task_script_deserialization() {
        // Test that script-only tasks (no command) deserialize correctly
        let json = r#"{
            "script": "echo hello",
            "inputs": ["src/main.rs"]
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.command.is_empty()); // No command
        assert_eq!(task.script, Some("echo hello".to_string()));
        assert_eq!(task.inputs.len(), 1);
    }

    #[test]
    fn test_task_node_script_variant() {
        // Test that TaskNode::Task correctly deserializes script-only tasks
        let json = r#"{
            "script": "echo hello"
        }"#;

        let node: TaskNode = serde_json::from_str(json).unwrap();
        assert!(node.is_task());
    }

    #[test]
    fn test_task_group_with_script_task() {
        // Test parallel task group containing a script task (mimics cross.linux)
        let json = r#"{
            "parallel": {
                "linux": {
                    "script": "echo building",
                    "inputs": ["src/main.rs"]
                }
            }
        }"#;

        let group: TaskGroup = serde_json::from_str(json).unwrap();
        assert_eq!(group.len(), 1);
    }

    #[test]
    fn test_full_tasks_map_with_script() {
        // Test deserializing a full tasks map like in Project.tasks
        // This mimics the new structure: tasks: { cross: { parallel: { linux: { script: ... } } } }
        let json = r#"{
            "pwd": { "command": "pwd" },
            "cross": {
                "parallel": {
                    "linux": {
                        "script": "echo building",
                        "inputs": ["src/main.rs"]
                    }
                }
            }
        }"#;

        let tasks: HashMap<String, TaskNode> = serde_json::from_str(json).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.contains_key("pwd"));
        assert!(tasks.contains_key("cross"));

        // pwd should be Task
        assert!(tasks.get("pwd").unwrap().is_task());

        // cross should be Group
        assert!(tasks.get("cross").unwrap().is_group());
    }

    #[test]
    fn test_complex_nested_tasks_like_cuenv() {
        // Test a more complex structure mimicking cuenv's actual env.cue tasks
        let json = r#"{
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
                        "script": "echo building",
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
        }"#;

        let result: Result<HashMap<String, TaskNode>, _> = serde_json::from_str(json);
        match result {
            Ok(tasks) => {
                assert_eq!(tasks.len(), 5);
                assert!(tasks.get("pwd").unwrap().is_task());
                assert!(tasks.get("check").unwrap().is_task());
                assert!(tasks.get("fmt").unwrap().is_group());
                assert!(tasks.get("cross").unwrap().is_group());
                assert!(tasks.get("docs").unwrap().is_group());
            }
            Err(e) => {
                panic!("Failed to deserialize complex tasks: {}", e);
            }
        }
    }

    #[test]
    fn test_task_list_sequential() {
        let task1 = Task {
            command: "echo".to_string(),
            args: vec!["first".to_string()],
            description: Some("First task".to_string()),
            ..Default::default()
        };

        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            description: Some("Second task".to_string()),
            ..Default::default()
        };

        let list = TaskList {
            steps: vec![
                TaskNode::Task(Box::new(task1)),
                TaskNode::Task(Box::new(task2)),
            ],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        };

        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());
    }

    #[test]
    fn test_task_group_parallel() {
        let task1 = Task {
            command: "echo".to_string(),
            args: vec!["task1".to_string()],
            description: Some("Task 1".to_string()),
            ..Default::default()
        };

        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["task2".to_string()],
            description: Some("Task 2".to_string()),
            ..Default::default()
        };

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("task1".to_string(), TaskNode::Task(Box::new(task1)));
        parallel_tasks.insert("task2".to_string(), TaskNode::Task(Box::new(task2)));

        let group = TaskGroup {
            parallel: parallel_tasks,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        };

        assert_eq!(group.len(), 2);
        assert!(!group.is_empty());
    }

    #[test]
    fn test_tasks_collection() {
        let mut tasks = Tasks::new();
        assert!(tasks.list_tasks().is_empty());

        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            description: Some("Hello task".to_string()),
            ..Default::default()
        };

        tasks
            .tasks
            .insert("greet".to_string(), TaskNode::Task(Box::new(task)));

        assert!(tasks.contains("greet"));
        assert!(!tasks.contains("nonexistent"));
        assert_eq!(tasks.list_tasks(), vec!["greet"]);

        let retrieved = tasks.get("greet").unwrap();
        assert!(retrieved.is_task());
    }

    #[test]
    fn test_task_node_helpers() {
        let task = Task {
            command: "test".to_string(),
            description: Some("Test task".to_string()),
            ..Default::default()
        };

        let task_node = TaskNode::Task(Box::new(task.clone()));
        assert!(task_node.is_task());
        assert!(!task_node.is_group());
        assert!(!task_node.is_list());
        assert_eq!(task_node.as_task().unwrap().command, "test");
        assert!(task_node.as_group().is_none());
        assert!(task_node.as_list().is_none());

        let group = TaskNode::Group(TaskGroup {
            parallel: HashMap::new(),
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        });
        assert!(!group.is_task());
        assert!(group.is_group());
        assert!(!group.is_list());
        assert!(group.as_task().is_none());
        assert!(group.as_group().is_some());

        let list = TaskNode::List(TaskList {
            steps: vec![],
            depends_on: vec![],
            stop_on_first_error: true,
            description: None,
        });
        assert!(!list.is_task());
        assert!(!list.is_group());
        assert!(list.is_list());
        assert!(list.as_list().is_some());
    }

    #[test]
    fn test_script_shell_command_and_flag() {
        assert_eq!(ScriptShell::Bash.command_and_flag(), ("bash", "-c"));
        assert_eq!(ScriptShell::Python.command_and_flag(), ("python", "-c"));
        assert_eq!(ScriptShell::Node.command_and_flag(), ("node", "-e"));
        assert_eq!(
            ScriptShell::Powershell.command_and_flag(),
            ("powershell", "-Command")
        );
    }

    #[test]
    fn test_shell_options_default() {
        let opts = ShellOptions::default();
        assert!(opts.errexit);
        assert!(opts.nounset);
        assert!(opts.pipefail);
        assert!(!opts.xtrace);
    }

    #[test]
    fn test_shell_options_to_set_commands() {
        let opts = ShellOptions::default();
        assert_eq!(opts.to_set_commands(), "set -e -u -o pipefail\n");

        let debug_opts = ShellOptions {
            errexit: true,
            nounset: false,
            pipefail: true,
            xtrace: true,
        };
        assert_eq!(debug_opts.to_set_commands(), "set -e -o pipefail -x\n");

        let no_opts = ShellOptions {
            errexit: false,
            nounset: false,
            pipefail: false,
            xtrace: false,
        };
        assert_eq!(no_opts.to_set_commands(), "");
    }

    #[test]
    fn test_input_deserialization_variants() {
        let path_json = r#""src/**/*.rs""#;
        let path_input: Input = serde_json::from_str(path_json).unwrap();
        assert_eq!(path_input, Input::Path("src/**/*.rs".to_string()));

        let project_json = r#"{
            "project": "../projB",
            "task": "build",
            "map": [{"from": "dist/app.txt", "to": "vendor/app.txt"}]
        }"#;
        let project_input: Input = serde_json::from_str(project_json).unwrap();
        match project_input {
            Input::Project(reference) => {
                assert_eq!(reference.project, "../projB");
                assert_eq!(reference.task, "build");
                assert_eq!(reference.map.len(), 1);
                assert_eq!(reference.map[0].from, "dist/app.txt");
                assert_eq!(reference.map[0].to, "vendor/app.txt");
            }
            other => panic!("Expected project reference, got {:?}", other),
        }

        // Test TaskOutput variant (same-project task reference)
        let task_json = r#"{"task": "build.deps"}"#;
        let task_input: Input = serde_json::from_str(task_json).unwrap();
        match task_input {
            Input::Task(output) => {
                assert_eq!(output.task, "build.deps");
                assert!(output.map.is_none());
            }
            other => panic!("Expected task output reference, got {:?}", other),
        }
    }

    #[test]
    fn test_task_input_helpers_collect() {
        use std::collections::HashSet;
        use std::path::Path;

        let task = Task {
            inputs: vec![
                Input::Path("src".into()),
                Input::Project(ProjectReference {
                    project: "../projB".into(),
                    task: "build".into(),
                    map: vec![Mapping {
                        from: "dist/app.txt".into(),
                        to: "vendor/app.txt".into(),
                    }],
                }),
            ],
            ..Default::default()
        };

        let path_inputs: Vec<String> = task.iter_path_inputs().cloned().collect();
        assert_eq!(path_inputs, vec!["src".to_string()]);

        let project_refs: Vec<&ProjectReference> = task.iter_project_refs().collect();
        assert_eq!(project_refs.len(), 1);
        assert_eq!(project_refs[0].project, "../projB");

        let prefix = Path::new("prefix");
        let collected = task.collect_all_inputs_with_prefix(Some(prefix));
        let collected: HashSet<_> = collected
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        let expected: HashSet<_> = ["src", "vendor/app.txt"]
            .into_iter()
            .map(|p| prefix.join(p))
            .collect();
        assert_eq!(collected, expected);
    }

    #[test]
    fn test_resolved_args_interpolate_positional() {
        let args = ResolvedArgs {
            positional: vec!["video123".into(), "1080p".into()],
            named: HashMap::new(),
        };
        assert_eq!(args.interpolate("{{0}}"), "video123");
        assert_eq!(args.interpolate("{{1}}"), "1080p");
        assert_eq!(args.interpolate("--id={{0}}"), "--id=video123");
        assert_eq!(args.interpolate("{{0}}-{{1}}"), "video123-1080p");
    }

    #[test]
    fn test_resolved_args_interpolate_named() {
        let mut named = HashMap::new();
        named.insert("url".into(), "https://example.com".into());
        named.insert("quality".into(), "720p".into());
        let args = ResolvedArgs {
            positional: vec![],
            named,
        };
        assert_eq!(args.interpolate("{{url}}"), "https://example.com");
        assert_eq!(args.interpolate("--quality={{quality}}"), "--quality=720p");
    }

    #[test]
    fn test_resolved_args_interpolate_mixed() {
        let mut named = HashMap::new();
        named.insert("format".into(), "mp4".into());
        let args = ResolvedArgs {
            positional: vec!["VIDEO_ID".into()],
            named,
        };
        assert_eq!(
            args.interpolate("download {{0}} --format={{format}}"),
            "download VIDEO_ID --format=mp4"
        );
    }

    #[test]
    fn test_resolved_args_no_placeholder_unchanged() {
        let args = ResolvedArgs::new();
        assert_eq!(
            args.interpolate("no placeholders here"),
            "no placeholders here"
        );
        assert_eq!(args.interpolate(""), "");
    }

    #[test]
    fn test_resolved_args_interpolate_args_list() {
        let args = ResolvedArgs {
            positional: vec!["id123".into()],
            named: HashMap::new(),
        };
        let input = vec!["--id".into(), "{{0}}".into(), "--verbose".into()];
        let result = args.interpolate_args(&input);
        assert_eq!(result, vec!["--id", "id123", "--verbose"]);
    }

    #[test]
    fn test_task_params_deserialization_with_flatten() {
        // Test that named params are flattened (not nested under "named")
        let json = r#"{
            "positional": [{"description": "Video ID", "required": true}],
            "quality": {"description": "Quality", "default": "1080p", "short": "q"},
            "verbose": {"description": "Verbose output", "type": "bool"}
        }"#;
        let params: TaskParams = serde_json::from_str(json).unwrap();

        assert_eq!(params.positional.len(), 1);
        assert_eq!(
            params.positional[0].description,
            Some("Video ID".to_string())
        );
        assert!(params.positional[0].required);

        assert_eq!(params.named.len(), 2);
        assert!(params.named.contains_key("quality"));
        assert!(params.named.contains_key("verbose"));

        let quality = &params.named["quality"];
        assert_eq!(quality.default, Some("1080p".to_string()));
        assert_eq!(quality.short, Some("q".to_string()));

        let verbose = &params.named["verbose"];
        assert_eq!(verbose.param_type, ParamType::Bool);
    }

    #[test]
    fn test_task_params_empty() {
        let json = r#"{}"#;
        let params: TaskParams = serde_json::from_str(json).unwrap();
        assert!(params.positional.is_empty());
        assert!(params.named.is_empty());
    }

    #[test]
    fn test_param_def_defaults() {
        let def = ParamDef::default();
        assert!(def.description.is_none());
        assert!(!def.required);
        assert!(def.default.is_none());
        assert_eq!(def.param_type, ParamType::String);
        assert!(def.short.is_none());
    }

    // ==========================================================================
    // AffectedBy trait tests
    // ==========================================================================

    mod affected_tests {
        use super::*;
        use crate::AffectedBy;
        use std::path::PathBuf;

        fn make_task(inputs: Vec<&str>) -> Task {
            Task {
                inputs: inputs
                    .into_iter()
                    .map(|s| Input::Path(s.to_string()))
                    .collect(),
                command: "echo test".to_string(),
                ..Default::default()
            }
        }

        #[test]
        fn test_task_no_inputs_always_affected() {
            let task = make_task(vec![]);
            let changed_files: Vec<PathBuf> = vec![];
            let root = Path::new(".");

            // Task with no inputs should always be affected
            assert!(task.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_with_inputs_matching() {
            let task = make_task(vec!["src/**"]);
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(task.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_with_inputs_not_matching() {
            let task = make_task(vec!["src/**"]);
            let changed_files = vec![PathBuf::from("docs/readme.md")];
            let root = Path::new(".");

            assert!(!task.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_with_project_root_path_normalization() {
            let task = make_task(vec!["src/**"]);
            // File is repo-relative, but matches project-relative pattern
            let changed_files = vec![PathBuf::from("projects/website/src/app.rs")];
            let root = Path::new("projects/website");

            assert!(task.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_node_delegates_to_task() {
            let task = make_task(vec!["src/**"]);
            let node = TaskNode::Task(Box::new(task));
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(node.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_group_any_affected() {
            let lint_task = make_task(vec!["src/**"]);
            let test_task = make_task(vec!["tests/**"]);

            let mut parallel_tasks = HashMap::new();
            parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
            parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

            let group = TaskGroup {
                parallel: parallel_tasks,
                depends_on: vec![],
                max_concurrency: None,
                description: None,
            };

            // Change in src/ should affect the group (because lint is affected)
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(group.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_group_none_affected() {
            let lint_task = make_task(vec!["src/**"]);
            let test_task = make_task(vec!["tests/**"]);

            let mut parallel_tasks = HashMap::new();
            parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
            parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

            let group = TaskGroup {
                parallel: parallel_tasks,
                depends_on: vec![],
                max_concurrency: None,
                description: None,
            };

            // Change in docs/ should not affect the group
            let changed_files = vec![PathBuf::from("docs/readme.md")];
            let root = Path::new(".");

            assert!(!group.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_task_list_any_affected() {
            let build_task = make_task(vec!["src/**"]);
            let deploy_task = make_task(vec!["deploy/**"]);

            let list = TaskList {
                steps: vec![
                    TaskNode::Task(Box::new(build_task)),
                    TaskNode::Task(Box::new(deploy_task)),
                ],
                depends_on: vec![],
                stop_on_first_error: true,
                description: None,
            };

            // Change in src/ should affect the list (because build is affected)
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(list.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_input_patterns_returns_patterns() {
            let task = make_task(vec!["src/**", "Cargo.toml"]);
            let patterns = task.input_patterns();

            assert_eq!(patterns.len(), 2);
            assert!(patterns.contains(&"src/**"));
            assert!(patterns.contains(&"Cargo.toml"));
        }
    }
}
