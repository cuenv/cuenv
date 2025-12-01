//! Task execution and management module
//!
//! This module provides the core types for task execution, matching the CUE schema.

pub mod executor;
pub mod graph;
pub mod index;
pub mod io;

// Re-export executor and graph modules
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
}

impl Input {
    pub fn as_path(&self) -> Option<&String> {
        match self {
            Input::Path(path) => Some(path),
            Input::Project(_) => None,
        }
    }

    pub fn as_project(&self) -> Option<&ProjectReference> {
        match self {
            Input::Path(_) => None,
            Input::Project(reference) => Some(reference),
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Task {
    /// Shell configuration for command execution (optional)
    #[serde(default)]
    pub shell: Option<Shell>,

    /// Command to execute
    pub command: String,

    /// Arguments for the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables for this task
    #[serde(default)]
    pub env: HashMap<String, serde_json::Value>,

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
}

impl Default for Task {
    fn default() -> Self {
        Self {
            shell: None,
            command: String::new(),
            args: vec![],
            env: HashMap::new(),
            hermetic: true, // Default to hermetic execution
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            inputs_from: None,
            workspaces: vec![],
            description: None,
        }
    }
}

impl Task {
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

/// Represents a group of tasks with execution mode
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum TaskGroup {
    /// Sequential execution: array of tasks executed in order
    Sequential(Vec<TaskDefinition>),

    /// Parallel execution: named tasks that can run concurrently
    Parallel(HashMap<String, TaskDefinition>),
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
            TaskGroup::Parallel(tasks) => tasks.len(),
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

        let group = TaskGroup::Parallel(parallel_tasks);

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
}
