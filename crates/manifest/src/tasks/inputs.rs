use serde::{Deserialize, Serialize};

/// Mapping of external output to local workspace path
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    /// Path relative to external project root of a declared output from the external task
    pub from: String,
    /// Path inside the dependent task’s hermetic workspace where this file/dir will be materialized
    pub to: String,
}

/// Internal, expanded task-output mapping: files one task produces, and
/// where the task consuming them reads them.
///
/// Both paths are relative to the root of the CUE module of the project that
/// owns the consuming task, so one frame places every input of an action.
///
/// Serializable so an expanded task can still be listed (`cuenv task
/// --format json`), but never deserialized: CUE has no way to spell it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedInput {
    /// Source path/glob relative to the CUE module root.
    pub source: String,
    /// Destination path relative to the CUE module root. A file below the
    /// literal prefix of `source` keeps its relative location below it.
    pub destination: String,
    /// Task that produced this input: a name within the consumer's project,
    /// or `#project:task` for a task of another project.
    ///
    /// Retained after expansion so CI compilation can still create the
    /// orchestrator artifact handoff.
    pub producer_task: Option<String>,
}

impl MappedInput {
    /// Whether the files are staged anywhere other than where their producer
    /// writes them. Only a task that runs in an execution root can read a
    /// relocated file: nothing copies it into the live workspace.
    #[must_use]
    pub fn relocates(&self) -> bool {
        literal_path_prefix(&self.source) != literal_path_prefix(&self.destination)
    }
}

/// The leading literal directories of a path or glob: everything before its
/// first glob segment, without empty and `.` segments.
///
/// `dist/**/*.js` yields `dist`; `dist/app.js` yields itself; `**/*.js`
/// yields the empty string. This is where a glob's matches are guaranteed to
/// live, and so where they are staged when nothing relocates them: mapping
/// `dist/**/*.js` to the literal glob string would create paths such as
/// `dist/**/*.js/chunk.js`.
#[must_use]
pub fn literal_path_prefix(pattern: &str) -> String {
    pattern
        .split('/')
        .take_while(|segment| !segment.contains(['*', '{', '?', '[']))
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
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
    /// Internal expansion of a `from` → `to` task-output mapping, from the
    /// same project or another. The CUE schema never emits this shape
    /// directly, so it is never deserialized; it is serialized so expanded
    /// tasks can be listed.
    #[serde(skip_deserializing)]
    Mapped(MappedInput),
}

impl Input {
    /// Return the path/glob pattern if this input is a local path.
    #[must_use]
    pub fn as_path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path.as_str()),
            Self::Project(_) | Self::Task(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the project reference if this input references another project.
    #[must_use]
    pub fn as_project(&self) -> Option<&ProjectReference> {
        match self {
            Self::Project(reference) => Some(reference),
            Self::Path(_) | Self::Task(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the task output reference if this input references a task output.
    #[must_use]
    pub fn as_task_output(&self) -> Option<&TaskOutput> {
        match self {
            Self::Task(output) => Some(output),
            Self::Path(_) | Self::Project(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the producer task name for a task output, including an
    /// internally expanded mapped input.
    #[must_use]
    pub fn task_output_name(&self) -> Option<&str> {
        match self {
            Self::Task(output) => Some(&output.task),
            Self::Mapped(mapping) => mapping.producer_task.as_deref(),
            Self::Path(_) | Self::Project(_) => None,
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
    #[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expanded_mapped_input_serializes() {
        // `cuenv task --format json` serializes expanded tasks, and a task
        // consuming another task's outputs carries this variant.
        let input = Input::Mapped(MappedInput {
            source: "web/dist/**".to_string(),
            destination: "dist".to_string(),
            producer_task: Some("build".to_string()),
        });

        let json = serde_json::to_value(&input).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "source": "web/dist/**",
                "destination": "dist",
                "producerTask": "build",
            })
        );
    }

    #[test]
    fn a_mapped_input_is_never_deserialized() {
        let parsed: std::result::Result<Input, _> = serde_json::from_value(serde_json::json!({
            "source": "web/dist/**",
            "destination": "dist",
        }));
        assert!(!matches!(parsed, Ok(Input::Mapped(_))));
    }
}
