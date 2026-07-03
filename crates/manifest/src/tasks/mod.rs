//! Task DTO types matching the CUE schema.
//!
//! # Task API v2
//!
//! Users annotate tasks with their type to unlock specific semantics:
//! - [`Task`]: Single command or script
//! - [`TaskGroup`]: Parallel execution (all children run concurrently)
//! - Sequences (`Vec<TaskNode>`): Sequential execution (steps run in order)
//!
//! Execution behavior (command resolution, affected-detection, graph
//! building) lives in `cuenv-core`; this module holds the serde types and
//! pure accessors only.

mod cache_policy;
mod capture_types;
mod dagger;
mod dependency;
mod inputs;
mod params;
mod resolver;
mod retry;
mod shell;

pub use cache_policy::{TaskCacheMode, TaskCachePolicy};
pub use capture_types::{CaptureSource, TaskCapture, TaskCaptureRef};
pub use dagger::{DaggerCacheMount, DaggerSecret, DaggerTaskConfig};
pub use dependency::TaskDependency;
pub use inputs::{
    Input, Mapping, ProjectReference, SourceLocation, TaskDirectory, TaskDirectoryBase, TaskOutput,
};
pub use params::{ParamDef, ParamType, ResolvedArgs, TaskParams};
pub use retry::RetryConfig;
pub use shell::{ScriptShell, Shell, ShellOptionToggle, ShellOptions};

use serde::{Deserialize, Serialize};
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
    /// Example: `labels: ["projen", "codegen"]`
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
    /// Resolves relative to the task definition, caller, or module root.
    #[serde(default, rename = "dir", skip_serializing_if = "Option::is_none")]
    pub directory: Option<TaskDirectory>,
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

        Ok(Self {
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
    #[must_use]
    pub fn from_task_ref(ref_str: &str) -> Self {
        Self {
            task_ref: Some(ref_str.to_string()),
            description: Some(format!("Reference to {ref_str}")),
            ..Default::default()
        }
    }

    /// Returns true if this task is a TaskRef placeholder that needs resolution.
    #[must_use]
    pub fn is_task_ref(&self) -> bool {
        self.task_ref.is_some()
    }

    /// Returns an iterator over dependency task names.
    pub fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.depends_on.iter().map(TaskDependency::task_name)
    }

    /// Returns the effective cache policy for this task.
    #[must_use]
    pub fn cache_policy(&self) -> TaskCachePolicy {
        self.cache.clone().unwrap_or_default()
    }

    /// Returns the description, or a default if not set.
    #[must_use]
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
    #[must_use]
    pub fn collect_path_inputs_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        self.iter_path_inputs()
            .map(|path| apply_prefix(prefix, path))
            .collect()
    }

    /// Collects mapped destinations from project references, applying an optional prefix.
    #[must_use]
    pub fn collect_project_destinations_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        self.iter_project_refs()
            .flat_map(|reference| reference.map.iter().map(|m| apply_prefix(prefix, &m.to)))
            .collect()
    }

    /// Collects all input patterns (local + project destinations) with an optional prefix.
    #[must_use]
    pub fn collect_all_inputs_with_prefix(&self, prefix: Option<&Path>) -> Vec<String> {
        let mut inputs = self.collect_path_inputs_with_prefix(prefix);
        inputs.extend(self.collect_project_destinations_with_prefix(prefix));
        inputs
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
    Sequence(Vec<Self>),
}

/// Root tasks structure from CUE
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tasks {
    /// Map of task names to their definitions
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskNode>,
}

impl Tasks {
    /// Create a new empty tasks collection
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a task node by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TaskNode> {
        self.tasks.get(name)
    }

    /// List all task names
    #[must_use]
    pub fn list_tasks(&self) -> Vec<&str> {
        self.tasks.keys().map(String::as_str).collect()
    }

    /// Check if a task exists
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.tasks.contains_key(name)
    }
}

impl TaskNode {
    /// Check if this is a single task
    #[must_use]
    pub fn is_task(&self) -> bool {
        matches!(self, Self::Task(_))
    }

    /// Check if this is a task group (parallel)
    #[must_use]
    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }

    /// Check if this is a sequence (sequential)
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        matches!(self, Self::Sequence(_))
    }

    /// Get as single task if it is one
    #[must_use]
    pub fn as_task(&self) -> Option<&Task> {
        match self {
            Self::Task(task) => Some(task.as_ref()),
            _ => None,
        }
    }

    /// Get as task group if it is one
    #[must_use]
    pub fn as_group(&self) -> Option<&TaskGroup> {
        match self {
            Self::Group(group) => Some(group),
            _ => None,
        }
    }

    /// Get as sequence if it is one
    #[must_use]
    pub fn as_sequence(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Sequence(seq) => Some(seq),
            _ => None,
        }
    }

    /// Get dependencies for this node
    #[must_use]
    pub fn depends_on(&self) -> &[TaskDependency] {
        match self {
            Self::Task(task) => &task.depends_on,
            Self::Group(group) => &group.depends_on,
            Self::Sequence(_) => &[], // Sequences don't have top-level deps
        }
    }

    /// Get description for this node
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Task(task) => task.description.as_deref(),
            Self::Group(group) => group.description.as_deref(),
            Self::Sequence(_) => None, // Sequences don't have descriptions
        }
    }
}

impl TaskGroup {
    /// Get the number of tasks in this group
    #[must_use]
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Check if the group is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

// =============================================================================
// Task graph trait implementations
// =============================================================================

impl cuenv_task_graph::TaskNodeData for Task {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.depends_on.iter().map(TaskDependency::task_name)
    }
}

impl cuenv_task_graph::MutableTaskNodeData for Task {
    fn add_dependency(&mut self, dep: String) {
        if !cuenv_task_graph::TaskNodeData::has_dependency(self, &dep) {
            self.depends_on.push(TaskDependency::from_name(dep));
        }
    }
}
