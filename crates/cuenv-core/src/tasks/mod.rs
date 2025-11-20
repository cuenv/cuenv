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

/// Cross-project external input declaration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ExternalInput {
    /// Path to project root relative to env.cue or absolute-from-repo-root
    pub project: String,
    /// Name of the external task in that project
    pub task: String,
    /// Required explicit mappings
    pub map: Vec<Mapping>,
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

    /// Task dependencies (names of tasks that must run first)
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,

    /// Input files/resources
    #[serde(default)]
    pub inputs: Vec<String>,

    /// Output files/resources
    #[serde(default)]
    pub outputs: Vec<String>,

    /// Cross-project inputs
    #[serde(default, rename = "externalInputs")]
    pub external_inputs: Option<Vec<ExternalInput>>,

    /// Workspace resolution configuration
    #[serde(default, rename = "workspaceInputs")]
    pub workspace_inputs: Option<WorkspaceInputs>,

    /// Description of the task
    #[serde(default)]
    pub description: Option<String>,
}

/// Configuration for workspace-aware dependency resolution
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInputs {
    /// Whether to enable workspace resolution
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Explicit package manager override
    pub package_manager: Option<String>,
    /// Explicit list of packages to include
    #[serde(default)]
    pub packages: Vec<String>,
    /// Path to the lockfile
    pub lockfile: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Task {
    /// Returns the description, or a default if not set.
    pub fn description(&self) -> &str {
        self.description
            .as_deref()
            .unwrap_or("No description provided")
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
            shell: None,
            args: vec![],
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: None,
        };

        assert!(task.shell.is_none());
        assert_eq!(task.command, "echo");
        assert_eq!(task.description(), "No description provided");
        assert!(task.args.is_empty());
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
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("First task".to_string()),
        };

        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("Second task".to_string()),
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
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("Task 1".to_string()),
        };

        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["task2".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("Task 2".to_string()),
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
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("Hello task".to_string()),
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
            shell: None,
            args: vec![],
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: Some("Test task".to_string()),
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
}
