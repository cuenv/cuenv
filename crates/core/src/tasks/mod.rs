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
pub mod cache;
mod cache_policy;
mod capture_types;
pub mod captures;
mod dependency;
pub(crate) mod env;
pub mod executor;
pub mod graph;
pub mod graph_walk;
pub mod index;
mod inputs;
pub mod output_refs;
pub mod process_registry;
mod shell;

// Re-export executor and graph modules
pub use backend::{
    BackendFactory, HostBackend, TaskBackend, TaskExecutionContext, create_backend,
    create_backend_with_factory, should_use_dagger,
};
pub use cache_policy::{TaskCacheMode, TaskCachePolicy};
pub use capture_types::{CaptureSource, TaskCapture, TaskCaptureRef};
pub use dependency::TaskDependency;
pub use executor::*;
pub use graph::*;
pub use index::{IndexedTask, TaskIndex, TaskPath, WorkspaceTask};
pub use inputs::{
    Input, Mapping, ProjectReference, SourceLocation, TaskDirectory, TaskDirectoryBase,
    TaskDirectoryOptions, TaskOutput,
};
pub use output_refs::{
    OutputRefResolver, TaskOutputField, TaskOutputRef, has_output_refs, process_output_refs,
};
pub use process_registry::global_registry;
pub(crate) use shell::TaskCommandSpec;
pub use shell::{ScriptShell, Shell, ShellOptions};

use serde::{Deserialize, Serialize};
use shell::EffectiveScriptShell;
use std::collections::HashMap;
use std::path::Path;

fn default_hermetic() -> bool {
    true
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

    /// Task result cache policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<TaskCachePolicy>,

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

    /// Named regex captures extracted from task stdout/stderr after execution
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub captures: HashMap<String, TaskCapture>,

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

    /// Source file location where this task is bound in the current CUE instance.
    /// Used by object-shaped `dir` values with `from: "caller"`.
    #[serde(
        default,
        rename = "_callerSource",
        skip_serializing_if = "Option::is_none"
    )]
    pub caller_source: Option<SourceLocation>,

    /// Working directory override.
    /// String values remain relative to the cue.mod root. Object values can
    /// resolve relative to the task definition, caller, or module root.
    #[serde(default, rename = "dir", skip_serializing_if = "Option::is_none")]
    pub directory: Option<TaskDirectory>,
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
            cache: Option<TaskCachePolicy>,
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
            captures: HashMap<String, TaskCapture>,
            #[serde(default)]
            task_ref: Option<String>,
            #[serde(default)]
            project_root: Option<std::path::PathBuf>,
            #[serde(default, rename = "_source")]
            source: Option<SourceLocation>,
            #[serde(default, rename = "_callerSource")]
            caller_source: Option<SourceLocation>,
            #[serde(default, rename = "dir")]
            directory: Option<TaskDirectory>,
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
            cache: helper.cache,
            description: helper.description,
            params: helper.params,
            labels: helper.labels,
            timeout: helper.timeout,
            retry: helper.retry,
            continue_on_error: helper.continue_on_error,
            captures: helper.captures,
            task_ref: helper.task_ref,
            project_root: helper.project_root,
            source: helper.source,
            caller_source: helper.caller_source,
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
            cache: None,
            description: None,
            params: None,
            labels: vec![],
            timeout: None,
            retry: None,
            continue_on_error: false,
            captures: HashMap::new(),
            task_ref: None,
            project_root: None,
            source: None,
            caller_source: None,
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

    /// Returns the effective cache policy for this task.
    #[must_use]
    pub fn cache_policy(&self) -> TaskCachePolicy {
        self.cache.clone().unwrap_or_default()
    }

    /// Returns the description, or a default if not set.
    pub fn description(&self) -> &str {
        self.description
            .as_deref()
            .unwrap_or("No description provided")
    }

    /// Build the executable invocation for this task using the provided command resolver.
    pub(crate) fn command_spec<F>(&self, mut resolve_command: F) -> crate::Result<TaskCommandSpec>
    where
        F: FnMut(&str) -> String,
    {
        if let Some(script) = &self.script {
            let shell = self.effective_script_shell();
            let script = self.prepare_script(script, &shell)?;

            return Ok(TaskCommandSpec {
                program: resolve_command(&shell.command),
                args: vec![shell.flag, script],
            });
        }

        if let Some(shell) = &self.shell
            && let (Some(shell_command), Some(shell_flag)) = (&shell.command, &shell.flag)
        {
            let full_command = if self.command.is_empty() {
                self.args.join(" ")
            } else if self.args.is_empty() {
                resolve_command(&self.command)
            } else {
                let resolved_command = resolve_command(&self.command);
                format!("{} {}", resolved_command, self.args.join(" "))
            };

            return Ok(TaskCommandSpec {
                program: resolve_command(shell_command),
                args: vec![shell_flag.clone(), full_command],
            });
        }

        Ok(TaskCommandSpec {
            program: resolve_command(&self.command),
            args: self.args.clone(),
        })
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

    fn effective_script_shell(&self) -> EffectiveScriptShell {
        if let Some(script_shell) = self.script_shell {
            let (command, flag) = script_shell.command_and_flag();

            return EffectiveScriptShell {
                command: command.to_string(),
                flag: flag.to_string(),
                display_name: command.to_string(),
                supports_shell_options: script_shell.supports_shell_options(),
                supports_pipefail: script_shell.supports_pipefail(),
            };
        }

        if let Some(shell) = &self.shell {
            let command = shell.command.clone().unwrap_or_else(|| "bash".to_string());
            let flag = shell.flag.clone().unwrap_or_else(|| "-c".to_string());
            let (supports_shell_options, supports_pipefail) = ScriptShell::from_command(&command)
                .map(|script_shell| {
                    (
                        script_shell.supports_shell_options(),
                        script_shell.supports_pipefail(),
                    )
                })
                .unwrap_or((false, false));

            return EffectiveScriptShell {
                display_name: command.clone(),
                command,
                flag,
                supports_shell_options,
                supports_pipefail,
            };
        }

        let default_shell = ScriptShell::default();
        let (command, flag) = default_shell.command_and_flag();

        EffectiveScriptShell {
            command: command.to_string(),
            flag: flag.to_string(),
            display_name: command.to_string(),
            supports_shell_options: default_shell.supports_shell_options(),
            supports_pipefail: default_shell.supports_pipefail(),
        }
    }

    fn prepare_script(&self, script: &str, shell: &EffectiveScriptShell) -> crate::Result<String> {
        let Some(shell_options) = self.shell_options else {
            return Ok(script.to_string());
        };

        if !shell.supports_shell_options {
            return Err(crate::Error::configuration(format!(
                "Task uses shellOptions with unsupported script shell '{}'. \
                 Use scriptShell 'bash', 'sh', or 'zsh'.",
                shell.display_name
            )));
        }

        if shell_options.pipefail && !shell.supports_pipefail {
            return Err(crate::Error::configuration(format!(
                "Task uses shellOptions.pipefail with unsupported script shell '{}'. \
                 Disable pipefail or use scriptShell 'bash' or 'zsh'.",
                shell.display_name
            )));
        }

        let set_commands = shell_options.to_set_commands();
        if set_commands.is_empty() {
            return Ok(script.to_string());
        }

        Ok(format!("{set_commands}{script}"))
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
#[path = "tasks_tests.rs"]
mod tests;
