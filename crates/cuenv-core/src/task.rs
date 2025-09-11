//! Task data structures and definitions
//!
//! This module provides the core types for task execution, matching the CUE schema.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Shell configuration for task execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shell {
    /// Shell executable name (e.g., "bash", "fish", "zsh")
    pub command: Option<String>,
    /// Flag for command execution (e.g., "-c", "--command")
    pub flag: Option<String>,
}

/// A single executable task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    /// Shell configuration for command execution (optional)
    #[serde(default)]
    pub shell: Option<Shell>,
    
    /// Command to execute
    pub command: String,
    
    /// Arguments for the command
    #[serde(default)]
    pub args: Vec<String>,
    
    /// Task dependencies (names of tasks that must run first)
    #[serde(default)]
    pub dependencies: Vec<String>,
    
    /// Input files/resources
    #[serde(default)]
    pub inputs: Vec<String>,
    
    /// Output files/resources
    #[serde(default)]
    pub outputs: Vec<String>,
    
    /// Description of the task
    #[serde(default = "default_description", deserialize_with = "deserialize_null_default")]
    pub description: String,
}


fn default_description() -> String {
    "No description provided".to_string()
}

/// Custom deserializer that handles null values by using the default
fn deserialize_null_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(default_description))
}

/// Represents a group of tasks with execution mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskGroup {
    /// Sequential execution: array of tasks executed in order
    Sequential(Vec<TaskDefinition>),
    
    /// Parallel execution: named tasks that can run concurrently
    Parallel(HashMap<String, TaskDefinition>),
}

/// A task definition can be either a single task or a group of tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum TaskDefinition {
    /// A single task
    Single(Task),
    
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
            TaskDefinition::Single(task) => Some(task),
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
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: default_description(),
        };
        
        assert!(task.shell.is_none());
        assert_eq!(task.command, "echo");
        assert_eq!(task.description, "No description provided");
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
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(task1),
            TaskDefinition::Single(task2),
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
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["task2".to_string()],
            shell: None,
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("task1".to_string(), TaskDefinition::Single(task1));
        parallel_tasks.insert("task2".to_string(), TaskDefinition::Single(task2));
        
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
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        tasks.tasks.insert("greet".to_string(), TaskDefinition::Single(task));
        
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
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            description: String::new(),
        };
        
        let single = TaskDefinition::Single(task.clone());
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