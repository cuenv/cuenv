//! Task execution and management module
//!
//! This module provides the core types for task execution, matching the CUE schema.

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

/// Shell configuration for task execution
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

/// Task dependency - always an embedded task from CUE reference.
///
/// In CUE, dependencies are expressed via references: `tasks.build` or imported tasks.
/// The embedded task carries metadata (`_name`, `_source`) for resolution.
///
/// Cross-project dependencies are detected by comparing the `_source.file` directory
/// against the current project's directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct TaskDependency(pub Box<Task>);

impl TaskDependency {
    /// Create a new task dependency from an embedded task
    pub fn new(task: Task) -> Self {
        Self(Box::new(task))
    }

    /// Create a same-project task dependency (for programmatic/internal use).
    ///
    /// This creates a minimal task with just the name set.
    /// For CUE-evaluated dependencies, the full task metadata is embedded automatically.
    pub fn same_project(name: impl Into<String>) -> Self {
        Self(Box::new(Task {
            name: Some(name.into()),
            ..Task::default()
        }))
    }

    /// Create a cross-project task dependency (for programmatic/internal use).
    ///
    /// This is used by hook refs (`#project:task` format) and contributor injection.
    /// The project name is stored in a synthetic `_source.file` path for detection.
    pub fn cross_project(project: impl Into<String>, task: impl Into<String>) -> Self {
        let project_str = project.into();
        Self(Box::new(Task {
            name: Some(task.into()),
            source: Some(SourceLocation {
                // Store project path in source file for cross_project_path() detection
                file: format!("{}/env.cue", project_str),
                line: 0,
                column: 0,
            }),
            ..Task::default()
        }))
    }

    /// Get the task name from the `_name` field
    pub fn task_name(&self) -> Option<&str> {
        self.0.name.as_deref()
    }

    /// Get the source location of the dependency definition
    pub fn source(&self) -> Option<&SourceLocation> {
        self.0.source.as_ref()
    }

    /// Get the source directory (for cross-project detection)
    pub fn source_directory(&self) -> Option<&str> {
        self.0.source.as_ref().and_then(|s| s.directory())
    }

    /// Check if this is a cross-project dependency.
    /// Returns true if the source directory differs from the current project directory.
    pub fn is_cross_project(&self, current_dir: &str) -> bool {
        self.source_directory()
            .is_some_and(|dir| dir != current_dir)
    }

    /// Get the cross-project path if this is a cross-project dependency.
    /// Returns the source directory if it differs from the current project directory.
    pub fn cross_project_path(&self, current_dir: &str) -> Option<&str> {
        self.source_directory().filter(|dir| *dir != current_dir)
    }

    /// Get the embedded task
    pub fn task(&self) -> &Task {
        &self.0
    }
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

/// A single executable task
///
/// Note: Custom deserialization is used to ensure that a Task can only be
/// deserialized when it has a `command` or `script` field. This is necessary
/// because TaskDefinition uses untagged enum, and we need to distinguish
/// between a Task and a TaskGroup during deserialization.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Task {
    /// Internal name field - auto-populated by CUE's `[Name=string]: #Task & {_name: Name}` pattern.
    /// Used to identify embedded tasks when using CUE references in dependsOn.
    #[serde(default, rename = "_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Shell configuration for command execution (optional)
    #[serde(default)]
    pub shell: Option<Shell>,

    /// Command to execute. Required unless 'script' is provided.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,

    /// Inline script to execute (alternative to command).
    /// When script is provided, shell defaults to bash if not specified.
    /// Supports multiline strings and shebang lines for polyglot scripts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

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

    /// Task dependencies - specify tasks that must complete before this task runs.
    /// Accepts either:
    /// - Explicit refs: {task: "build"} or {project: "other", task: "build"}
    /// - CUE references: tasks.build (provides LSP autocomplete, embeds _name)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,

    /// Resolved dependency names for graph building.
    /// Populated during task canonicalization from depends_on.
    /// This field is used by TaskNodeData trait for dependency resolution.
    #[serde(skip)]
    pub resolved_deps: Vec<String>,
}

// Custom deserialization for Task to ensure either command or script is present.
// This is necessary for untagged enum deserialization in TaskDefinition to work correctly.
impl<'de> serde::Deserialize<'de> for Task {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper struct that mirrors Task but with all optional fields
        #[derive(serde::Deserialize)]
        struct TaskHelper {
            #[serde(default, rename = "_name")]
            name: Option<String>,
            #[serde(default)]
            shell: Option<Shell>,
            #[serde(default)]
            command: Option<String>,
            #[serde(default)]
            script: Option<String>,
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
            task_ref: Option<String>,
            #[serde(default)]
            project_root: Option<std::path::PathBuf>,
            #[serde(default, rename = "_source")]
            source: Option<SourceLocation>,
            #[serde(default)]
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
            name: helper.name,
            shell: helper.shell,
            command: helper.command.unwrap_or_default(),
            script: helper.script,
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
            task_ref: helper.task_ref,
            project_root: helper.project_root,
            source: helper.source,
            directory: helper.directory,
            resolved_deps: Vec::new(),
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
            name: None,
            shell: None,
            command: String::new(),
            script: None,
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
            task_ref: None,
            project_root: None,
            source: None,
            directory: None,
            resolved_deps: vec![],
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

/// A parallel task group with optional shared dependencies
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParallelGroup {
    /// Named tasks that can run concurrently
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskDefinition>,

    /// Optional group-level dependencies applied to all subtasks
    /// Accepts same formats as Task.dependsOn: explicit refs or CUE references
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,
}

/// Represents a group of tasks with execution mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskGroup {
    /// Sequential execution: array of tasks executed in order
    Sequential(Vec<TaskDefinition>),

    /// Parallel execution: named tasks that can run concurrently
    Parallel(ParallelGroup),
}

/// A task definition can be either a single task or a group of tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskDefinition {
    /// A single task
    Single(Box<Task>),

    /// A group of tasks
    Group(TaskGroup),
}

/// Root tasks structure from CUE
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tasks {
    /// Map of task names to their definitions
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskDefinition>,
}

impl Tasks {
    /// Create a new empty tasks collection
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a task definition by name
    pub fn get(&self, name: &str) -> Option<&TaskDefinition> {
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

impl TaskDefinition {
    /// Check if this is a single task
    pub fn is_single(&self) -> bool {
        matches!(self, TaskDefinition::Single(_))
    }

    /// Check if this is a task group
    pub fn is_group(&self) -> bool {
        matches!(self, TaskDefinition::Group(_))
    }

    /// Get as single task if it is one
    pub fn as_single(&self) -> Option<&Task> {
        match self {
            TaskDefinition::Single(task) => Some(task.as_ref()),
            _ => None,
        }
    }

    /// Get as task group if it is one
    pub fn as_group(&self) -> Option<&TaskGroup> {
        match self {
            TaskDefinition::Group(group) => Some(group),
            _ => None,
        }
    }
}

impl TaskGroup {
    /// Check if this group is sequential
    pub fn is_sequential(&self) -> bool {
        matches!(self, TaskGroup::Sequential(_))
    }

    /// Check if this group is parallel
    pub fn is_parallel(&self) -> bool {
        matches!(self, TaskGroup::Parallel(_))
    }

    /// Get the number of tasks in this group
    pub fn len(&self) -> usize {
        match self {
            TaskGroup::Sequential(tasks) => tasks.len(),
            TaskGroup::Parallel(group) => group.tasks.len(),
        }
    }

    /// Check if the group is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl crate::AffectedBy for TaskGroup {
    /// A group is affected if ANY of its subtasks are affected.
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        match self {
            TaskGroup::Sequential(tasks) => tasks
                .iter()
                .any(|def| def.is_affected_by(changed_files, project_root)),
            TaskGroup::Parallel(group) => group
                .tasks
                .values()
                .any(|def| def.is_affected_by(changed_files, project_root)),
        }
    }

    fn input_patterns(&self) -> Vec<&str> {
        match self {
            TaskGroup::Sequential(tasks) => {
                tasks.iter().flat_map(|def| def.input_patterns()).collect()
            }
            TaskGroup::Parallel(group) => group
                .tasks
                .values()
                .flat_map(|def| def.input_patterns())
                .collect(),
        }
    }
}

impl crate::AffectedBy for TaskDefinition {
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        match self {
            TaskDefinition::Single(task) => task.is_affected_by(changed_files, project_root),
            TaskDefinition::Group(group) => group.is_affected_by(changed_files, project_root),
        }
    }

    fn input_patterns(&self) -> Vec<&str> {
        match self {
            TaskDefinition::Single(task) => task.input_patterns(),
            TaskDefinition::Group(group) => group.input_patterns(),
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
    fn test_task_definition_script_variant() {
        // Test that TaskDefinition::Single correctly deserializes script-only tasks
        let json = r#"{
            "script": "echo hello"
        }"#;

        let def: TaskDefinition = serde_json::from_str(json).unwrap();
        assert!(def.is_single());
    }

    #[test]
    fn test_task_group_with_script_task() {
        // Test parallel task group containing a script task (mimics cross.linux)
        let json = r#"{
            "linux": {
                "script": "echo building",
                "inputs": ["src/main.rs"]
            }
        }"#;

        let group: TaskGroup = serde_json::from_str(json).unwrap();
        assert!(group.is_parallel());
    }

    #[test]
    fn test_full_tasks_map_with_script() {
        // Test deserializing a full tasks map like in Project.tasks
        // This mimics the structure: tasks: { cross: { linux: { script: ... } } }
        let json = r#"{
            "pwd": { "command": "pwd" },
            "cross": {
                "linux": {
                    "script": "echo building",
                    "inputs": ["src/main.rs"]
                }
            }
        }"#;

        let tasks: HashMap<String, TaskDefinition> = serde_json::from_str(json).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.contains_key("pwd"));
        assert!(tasks.contains_key("cross"));

        // pwd should be Single
        assert!(tasks.get("pwd").unwrap().is_single());

        // cross should be Group (Parallel)
        assert!(tasks.get("cross").unwrap().is_group());
    }

    #[test]
    fn test_complex_nested_tasks_like_cuenv() {
        // Test a more complex structure mimicking cuenv's actual env.cue tasks
        // Note: dependsOn now uses embedded tasks (CUE refs) with _name field
        let json = r#"{
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
                    "script": "echo building",
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
                    "dependsOn": [{"_name": "docs.build", "command": "bash", "args": ["-c", "bun install"]}],
                    "inputs": [{"task": "docs.build"}]
                }
            }
        }"#;

        let result: Result<HashMap<String, TaskDefinition>, _> = serde_json::from_str(json);
        match result {
            Ok(tasks) => {
                assert_eq!(tasks.len(), 5);
                assert!(tasks.get("pwd").unwrap().is_single());
                assert!(tasks.get("check").unwrap().is_single());
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
    fn test_task_group_sequential() {
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

        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(Box::new(task1)),
            TaskDefinition::Single(Box::new(task2)),
        ]);

        assert!(group.is_sequential());
        assert!(!group.is_parallel());
        assert_eq!(group.len(), 2);
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
        parallel_tasks.insert("task1".to_string(), TaskDefinition::Single(Box::new(task1)));
        parallel_tasks.insert("task2".to_string(), TaskDefinition::Single(Box::new(task2)));

        let group = TaskGroup::Parallel(ParallelGroup {
            tasks: parallel_tasks,
            depends_on: vec![],
        });

        assert!(!group.is_sequential());
        assert!(group.is_parallel());
        assert_eq!(group.len(), 2);
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
            .insert("greet".to_string(), TaskDefinition::Single(Box::new(task)));

        assert!(tasks.contains("greet"));
        assert!(!tasks.contains("nonexistent"));
        assert_eq!(tasks.list_tasks(), vec!["greet"]);

        let retrieved = tasks.get("greet").unwrap();
        assert!(retrieved.is_single());
    }

    #[test]
    fn test_task_definition_helpers() {
        let task = Task {
            command: "test".to_string(),
            description: Some("Test task".to_string()),
            ..Default::default()
        };

        let single = TaskDefinition::Single(Box::new(task.clone()));
        assert!(single.is_single());
        assert!(!single.is_group());
        assert_eq!(single.as_single().unwrap().command, "test");
        assert!(single.as_group().is_none());

        let group = TaskDefinition::Group(TaskGroup::Sequential(vec![]));
        assert!(!group.is_single());
        assert!(group.is_group());
        assert!(group.as_single().is_none());
        assert!(group.as_group().is_some());
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
        fn test_task_definition_delegates_to_task() {
            let task = make_task(vec!["src/**"]);
            let def = TaskDefinition::Single(Box::new(task));
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(def.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_parallel_group_any_affected() {
            let lint_task = make_task(vec!["src/**"]);
            let test_task = make_task(vec!["tests/**"]);

            let mut parallel_tasks = HashMap::new();
            parallel_tasks.insert(
                "lint".to_string(),
                TaskDefinition::Single(Box::new(lint_task)),
            );
            parallel_tasks.insert(
                "test".to_string(),
                TaskDefinition::Single(Box::new(test_task)),
            );

            let group = TaskGroup::Parallel(ParallelGroup {
                tasks: parallel_tasks,
                depends_on: vec![],
            });

            // Change in src/ should affect the group (because lint is affected)
            let changed_files = vec![PathBuf::from("src/lib.rs")];
            let root = Path::new(".");

            assert!(group.is_affected_by(&changed_files, root));
        }

        #[test]
        fn test_parallel_group_none_affected() {
            let lint_task = make_task(vec!["src/**"]);
            let test_task = make_task(vec!["tests/**"]);

            let mut parallel_tasks = HashMap::new();
            parallel_tasks.insert(
                "lint".to_string(),
                TaskDefinition::Single(Box::new(lint_task)),
            );
            parallel_tasks.insert(
                "test".to_string(),
                TaskDefinition::Single(Box::new(test_task)),
            );

            let group = TaskGroup::Parallel(ParallelGroup {
                tasks: parallel_tasks,
                depends_on: vec![],
            });

            // Change in docs/ should not affect the group
            let changed_files = vec![PathBuf::from("docs/readme.md")];
            let root = Path::new(".");

            assert!(!group.is_affected_by(&changed_files, root));
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
