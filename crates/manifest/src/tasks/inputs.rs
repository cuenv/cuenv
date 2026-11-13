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
    /// For a path of a file reference (`<pkg>.#Files`, `schema.#ModuleFiles`):
    /// the directory its paths start from, relative to the CUE module root,
    /// empty for the module root itself. No task produces these files. They
    /// are read where they lie, never through a symbolic link or inside a
    /// nested repository, and every one of them lies beneath this directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
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

/// Where the paths of a [`FilesReference`] start from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FilesAnchor {
    /// The directory of the project with this name: `<pkg>.#Files`.
    Project(String),
    /// The root of the CUE module: `schema.#ModuleFiles`.
    Module,
}

impl std::fmt::Display for FilesAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(name) => write!(formatter, "project '{name}'"),
            Self::Module => formatter.write_str("the module root"),
        }
    }
}

impl FilesAnchor {
    /// How a reference to this anchor is written in CUE, for messages.
    #[must_use]
    pub fn spelling(&self) -> String {
        match self {
            Self::Project(name) => format!("{name}.#Files"),
            Self::Module => "schema.#ModuleFiles".to_string(),
        }
    }
}

/// Why `path` cannot be a path of a file reference, if it cannot.
///
/// These are the rules of the schema's `#AnchoredPath`: a path is relative
/// to its anchor and stays inside it, and every reader of inputs sees the
/// same string. They are checked again on every path bound to an anchor,
/// because a value the schema never checked (an old pinned schema,
/// hand-written JSON) can still arrive, and joining an absolute path to an
/// anchor would replace the anchor.
///
/// ```
/// use cuenv_manifest::tasks::anchored_path_problem;
///
/// assert_eq!(anchored_path_problem("articles/**"), None);
/// assert_eq!(anchored_path_problem(".github/**"), None);
/// assert!(anchored_path_problem("../shared/**").is_some());
/// assert!(anchored_path_problem("/etc/hosts").is_some());
/// assert!(anchored_path_problem("./bun.lock").is_some());
/// assert!(anchored_path_problem("articles/** ").is_some());
/// ```
#[must_use]
pub fn anchored_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("it is empty");
    }
    if path.trim() != path {
        return Some("it starts or ends with whitespace");
    }
    if path.starts_with(['/', '\\']) {
        return Some("it starts with `/`");
    }
    if path.contains("//") {
        return Some("it has an empty segment");
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Some("it has a `.` or `..` segment");
    }
    None
}

/// Files a task reads from outside its own project, chosen by the task that
/// reads them. The project that holds the files declares nothing.
///
/// The paths are globs or paths relative to the anchor. Reading them adds no
/// dependency on any task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesReference {
    /// Where the paths start from.
    pub anchor: FilesAnchor,
    /// Paths and globs relative to the anchor; at least one.
    pub paths: Vec<String>,
}

/// A [`FilesReference`] as the schema exports it.
#[derive(Serialize, Deserialize)]
struct FilesReferenceShape {
    /// `project` or `module`. Its presence is what makes an input a file
    /// reference.
    #[serde(rename = "cuenvFiles")]
    kind: String,
    #[serde(
        rename = "cuenvProject",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    project: Option<String>,
    paths: Vec<String>,
}

impl Serialize for FilesReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (kind, project) = match &self.anchor {
            FilesAnchor::Project(project) => ("project", Some(project.clone())),
            FilesAnchor::Module => ("module", None),
        };
        FilesReferenceShape {
            kind: kind.to_string(),
            project,
            paths: self.paths.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FilesReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let shape = FilesReferenceShape::deserialize(deserializer)?;
        let anchor = match (shape.kind.as_str(), shape.project) {
            ("project", Some(project)) if !project.is_empty() => FilesAnchor::Project(project),
            ("project", _) => {
                return Err(D::Error::custom(
                    "a project file reference names no project (cuenvProject is empty)",
                ));
            }
            ("module", None) => FilesAnchor::Module,
            ("module", Some(_)) => {
                return Err(D::Error::custom(
                    "a module file reference cannot name a project",
                ));
            }
            (other, _) => {
                return Err(D::Error::custom(format!(
                    "unknown kind of file reference '{other}'"
                )));
            }
        };
        if shape.paths.is_empty() {
            return Err(D::Error::custom("a file reference needs at least one path"));
        }
        Ok(Self {
            anchor,
            paths: shape.paths,
        })
    }
}

/// A single task input definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Input {
    /// Local path/glob input
    Path(String),
    /// Files outside the project, chosen by this task: of another project
    /// (`<pkg>.#Files`) or of the module (`schema.#ModuleFiles`). It is
    /// tried before the other object shapes and needs the `cuenvFiles`
    /// field, which no other input has.
    Files(FilesReference),
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

/// One pattern an input asks a reader of files to compare, and the
/// directory it starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPattern<'input> {
    /// A path or glob relative to the directory of the task's own project.
    Project(&'input str),
    /// A path or glob of a file reference, relative to its anchor.
    Anchored {
        /// Where the path starts from.
        anchor: &'input FilesAnchor,
        /// The path or glob, as written.
        path: &'input str,
    },
}

impl InputPattern<'_> {
    /// The path or glob, as written.
    #[must_use]
    pub const fn path(&self) -> &str {
        match self {
            Self::Project(path) | Self::Anchored { path, .. } => path,
        }
    }
}

impl std::fmt::Display for InputPattern<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(path) => formatter.write_str(path),
            Self::Anchored { anchor, path } => {
                write!(formatter, "{} & {{paths: [\"{path}\"]}}", anchor.spelling())
            }
        }
    }
}

impl Input {
    /// The file patterns this input names, each with where it starts.
    ///
    /// Every reader that compares inputs with files (affected detection,
    /// trigger derivation) reads them here, and this decides for every kind
    /// of input. A cross-project reference and a task output name the files
    /// of a task, which those readers follow through the task instead.
    #[must_use]
    pub fn patterns(&self) -> Vec<InputPattern<'_>> {
        match self {
            Self::Path(path) => vec![InputPattern::Project(path)],
            Self::Files(reference) => reference
                .paths
                .iter()
                .map(|path| InputPattern::Anchored {
                    anchor: &reference.anchor,
                    path,
                })
                .collect(),
            Self::Project(_) | Self::Task(_) | Self::Mapped(_) => Vec::new(),
        }
    }

    /// The file reference, if this input is one.
    #[must_use]
    pub const fn as_files(&self) -> Option<&FilesReference> {
        match self {
            Self::Files(reference) => Some(reference),
            Self::Path(_) | Self::Project(_) | Self::Task(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the path/glob pattern if this input is a local path.
    #[must_use]
    pub fn as_path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path.as_str()),
            Self::Files(_) | Self::Project(_) | Self::Task(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the project reference if this input references another project.
    #[must_use]
    pub fn as_project(&self) -> Option<&ProjectReference> {
        match self {
            Self::Project(reference) => Some(reference),
            Self::Path(_) | Self::Files(_) | Self::Task(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the task output reference if this input references a task output.
    #[must_use]
    pub fn as_task_output(&self) -> Option<&TaskOutput> {
        match self {
            Self::Task(output) => Some(output),
            Self::Path(_) | Self::Files(_) | Self::Project(_) | Self::Mapped(_) => None,
        }
    }

    /// Return the producer task name for a task output, including an
    /// internally expanded mapped input.
    #[must_use]
    pub fn task_output_name(&self) -> Option<&str> {
        match self {
            Self::Task(output) => Some(&output.task),
            Self::Mapped(mapping) => mapping.producer_task.as_deref(),
            Self::Path(_) | Self::Files(_) | Self::Project(_) => None,
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
            anchor: None,
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
