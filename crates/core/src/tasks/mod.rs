//! Task execution and management module
//!
//! This module provides the core types for task execution, matching the CUE schema.

pub mod backend;
pub mod discovery;
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
pub use index::{IndexedTask, TaskIndex, TaskPath};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

fn default_hermetic() -> bool {
    true
}

/// Shell configuration for task execution
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Shell {
    /// Shell executable name (e.g., "bash", "fish", "zsh")
    pub command: Option<String>,
    /// Flag for command execution (e.g., "-c", "--command")
    pub flag: Option<String>,
}

/// Mapping of external output to local workspace path
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Mapping {
    /// Path relative to external project root of a declared output from the external task
    pub from: String,
    /// Path inside the dependent task’s hermetic workspace where this file/dir will be materialized
    pub to: String,
}

/// A single task input definition
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ProjectReference {
    /// Path to project root relative to env.cue or absolute-from-repo-root
    pub project: String,
    /// Name of the external task in that project
    pub task: String,
    /// Required explicit mappings
    pub map: Vec<Mapping>,
}

/// Reference to another task's outputs within the same project
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskOutput {
    /// Name of the task whose cached outputs to consume (e.g. "docs.build")
    pub task: String,
    /// Optional explicit mapping of outputs. If omitted, all outputs are
    /// materialized at their original paths in the hermetic workspace.
    #[serde(default)]
    pub map: Option<Vec<Mapping>>,
}

/// A single executable task
///
/// Note: Custom deserialization is used to ensure that a Task can only be
/// deserialized when it has a `command` or `script` field. This is necessary
/// because TaskDefinition uses untagged enum, and we need to distinguish
/// between a Task and a TaskGroup during deserialization.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct Task {
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
    #[serde(default)]
    pub dagger: Option<DaggerTaskConfig>,

    /// When true (default), task runs in isolated hermetic directory.
    /// When false, task runs directly in workspace/project root.
    #[serde(default = "default_hermetic")]
    pub hermetic: bool,

    /// Task dependencies (names of tasks that must run first)
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,

    /// Input files/resources
    #[serde(default)]
    pub inputs: Vec<Input>,

    /// Output files/resources
    #[serde(default)]
    pub outputs: Vec<String>,

    /// Consume cached outputs from other tasks in the same project
    #[serde(default, rename = "inputsFrom")]
    pub inputs_from: Option<Vec<TaskOutput>>,

    /// Workspaces to mount/enable for this task
    #[serde(default)]
    pub workspaces: Vec<String>,

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
            #[serde(default = "default_hermetic")]
            hermetic: bool,
            #[serde(default, rename = "dependsOn")]
            depends_on: Vec<String>,
            #[serde(default)]
            inputs: Vec<Input>,
            #[serde(default)]
            outputs: Vec<String>,
            #[serde(default, rename = "inputsFrom")]
            inputs_from: Option<Vec<TaskOutput>>,
            #[serde(default)]
            workspaces: Vec<String>,
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
            args: helper.args,
            env: helper.env,
            dagger: helper.dagger,
            hermetic: helper.hermetic,
            depends_on: helper.depends_on,
            inputs: helper.inputs,
            outputs: helper.outputs,
            inputs_from: helper.inputs_from,
            workspaces: helper.workspaces,
            description: helper.description,
            params: helper.params,
            labels: helper.labels,
            task_ref: helper.task_ref,
            project_root: helper.project_root,
        })
    }
}

/// Dagger-specific task configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DaggerCacheMount {
    /// Path inside the container to mount the cache (e.g., "/root/.npm")
    pub path: String,

    /// Unique name for the cache volume
    pub name: String,
}

/// Task parameter definitions for CLI arguments
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    #[default]
    String,
    Bool,
    Int,
}

/// Parameter definition for task arguments
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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
            args: vec![],
            env: HashMap::new(),
            dagger: None,
            hermetic: true, // Default to hermetic execution
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            inputs_from: None,
            workspaces: vec![],
            description: None,
            params: None,
            labels: vec![],
            task_ref: None,
            project_root: None,
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

fn apply_prefix(prefix: Option<&Path>, value: &str) -> String {
    if let Some(prefix) = prefix {
        prefix.join(value).to_string_lossy().to_string()
    } else {
        value.to_string()
    }
}

/// A parallel task group with optional shared dependencies
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ParallelGroup {
    /// Named tasks that can run concurrently
    #[serde(flatten)]
    pub tasks: HashMap<String, TaskDefinition>,

    /// Optional group-level dependencies applied to all subtasks
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
}

/// Represents a group of tasks with execution mode
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum TaskGroup {
    /// Sequential execution: array of tasks executed in order
    Sequential(Vec<TaskDefinition>),

    /// Parallel execution: named tasks that can run concurrently
    Parallel(ParallelGroup),
}

/// A task definition can be either a single task or a group of tasks
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum TaskDefinition {
    /// A single task
    Single(Box<Task>),

    /// A group of tasks
    Group(TaskGroup),
}

/// Root tasks structure from CUE
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
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

    /// Check if this task definition uses a specific workspace
    ///
    /// Returns true if any task within this definition (including nested tasks
    /// in groups) has the specified workspace in its `workspaces` field.
    pub fn uses_workspace(&self, workspace_name: &str) -> bool {
        match self {
            TaskDefinition::Single(task) => task.workspaces.contains(&workspace_name.to_string()),
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    tasks.iter().any(|t| t.uses_workspace(workspace_name))
                }
                TaskGroup::Parallel(parallel) => parallel
                    .tasks
                    .values()
                    .any(|t| t.uses_workspace(workspace_name)),
            },
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
}
