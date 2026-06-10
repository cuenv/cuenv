use serde::{Deserialize, Serialize};

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

/// Base used to resolve an object-shaped task `dir`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TaskDirectoryBase {
    /// Resolve relative to the directory where the executable task body is defined.
    #[default]
    Definition,
    /// Resolve relative to the directory where the task is bound in the current CUE file.
    Caller,
    /// Resolve relative to the CUE module root.
    Module,
}

fn default_task_directory_path() -> String {
    ".".to_string()
}

/// Working directory override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskDirectory {
    /// Base directory used to resolve `path`.
    #[serde(default, rename = "from")]
    pub from: TaskDirectoryBase,
    /// Relative path from `from`.
    #[serde(default = "default_task_directory_path")]
    pub path: String,
}
