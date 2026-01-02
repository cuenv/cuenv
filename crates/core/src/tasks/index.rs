use super::{ParallelGroup, Task, TaskDefinition, TaskGroup, Tasks};
use crate::{Error, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Parsed task path that normalizes dotted/colon-separated identifiers
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct TaskPath {
    segments: Vec<String>,
}

impl TaskPath {
    /// Parse a raw task path that may use '.' or ':' separators
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.trim().is_empty() {
            return Err(Error::configuration("Task name cannot be empty"));
        }

        let normalized = raw.replace(':', ".");
        let segments: Vec<String> = normalized
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        if segments.is_empty() {
            return Err(Error::configuration("Task name cannot be empty"));
        }

        for segment in &segments {
            validate_segment(segment)?;
        }

        Ok(Self { segments })
    }

    /// Create a new path with an additional segment appended
    pub fn join(&self, segment: &str) -> Result<Self> {
        validate_segment(segment)?;
        let mut next = self.segments.clone();
        next.push(segment.to_string());
        Ok(Self { segments: next })
    }

    /// Convert to canonical dotted representation
    pub fn canonical(&self) -> String {
        self.segments.join(".")
    }

    /// Return the underlying path segments
    pub fn segments(&self) -> &[String] {
        &self.segments
    }
}

fn validate_segment(segment: &str) -> Result<()> {
    if segment.is_empty() {
        return Err(Error::configuration("Task name segment cannot be empty"));
    }

    if segment.contains('.') || segment.contains(':') {
        return Err(Error::configuration(format!(
            "Task name segment '{segment}' may not contain '.' or ':'"
        )));
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexedTask {
    /// Display name (with _ prefix stripped if present)
    pub name: String,
    /// Original name from CUE (may have _ prefix)
    pub original_name: String,
    pub definition: TaskDefinition,
    pub is_group: bool,
    /// Source file where this task was defined (relative to cue.mod root)
    pub source_file: Option<String>,
}

/// Task reference for workspace-wide task listing (used by IDE completions)
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTask {
    /// Project name from env.cue `name` field
    pub project: String,
    /// Task name within the project (canonical dotted path)
    pub task: String,
    /// Full task reference string in format "#project:task"
    pub task_ref: String,
    /// Task description if available
    pub description: Option<String>,
    /// Whether this is a task group
    pub is_group: bool,
}

/// Flattened index of all addressable tasks with canonical names
#[derive(Debug, Clone, Default)]
pub struct TaskIndex {
    entries: BTreeMap<String, IndexedTask>,
}

impl TaskIndex {
    /// Build a canonical index from the hierarchical task map
    ///
    /// Handles:
    /// - Stripping `_` prefix from task names (CUE hidden fields for local-only tasks)
    /// - Extracting source file from task metadata
    /// - Canonicalizing nested task paths
    pub fn build(tasks: &HashMap<String, TaskDefinition>) -> Result<Self> {
        let mut entries = BTreeMap::new();

        for (name, definition) in tasks {
            // Strip _ prefix for display/execution name
            let (display_name, original_name) = if let Some(stripped) = name.strip_prefix('_') {
                (stripped.to_string(), name.clone())
            } else {
                (name.clone(), name.clone())
            };

            // Extract source file from task definition
            let source_file = extract_source_file(definition);

            let path = TaskPath::parse(&display_name)?;
            let _ = canonicalize_definition(
                definition,
                &path,
                &mut entries,
                original_name,
                source_file,
            )?;
        }

        Ok(Self { entries })
    }

    /// Resolve a raw task name (dot or colon separated) to an indexed task
    pub fn resolve(&self, raw: &str) -> Result<&IndexedTask> {
        let path = TaskPath::parse(raw)?;
        let canonical = path.canonical();
        self.entries.get(&canonical).ok_or_else(|| {
            let available: Vec<&str> = self.entries.keys().map(String::as_str).collect();

            // Find similar task names for suggestions
            let suggestions: Vec<&str> = available
                .iter()
                .filter(|t| is_similar(&canonical, t))
                .copied()
                .collect();

            let mut msg = format!("Task '{}' not found.", canonical);

            if !suggestions.is_empty() {
                msg.push_str("\n\nDid you mean one of these?\n");
                for s in &suggestions {
                    msg.push_str(&format!("  - {s}\n"));
                }
            }

            if !available.is_empty() {
                msg.push_str("\nAvailable tasks:\n");
                for t in &available {
                    msg.push_str(&format!("  - {t}\n"));
                }
            }

            Error::configuration(msg)
        })
    }

    /// List all indexed tasks in deterministic order
    pub fn list(&self) -> Vec<&IndexedTask> {
        self.entries.values().collect()
    }

    /// Convert the index back into a Tasks collection keyed by canonical names
    pub fn to_tasks(&self) -> Tasks {
        let tasks = self
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.definition.clone()))
            .collect();

        Tasks { tasks }
    }
}

/// Extract source file from a task definition
fn extract_source_file(definition: &TaskDefinition) -> Option<String> {
    match definition {
        TaskDefinition::Single(task) => task.source.as_ref().map(|s| s.file.clone()),
        TaskDefinition::Group(group) => {
            // For groups, use source from first child task
            match group {
                TaskGroup::Sequential(tasks) => tasks.first().and_then(extract_source_file),
                TaskGroup::Parallel(parallel) => {
                    parallel.tasks.values().next().and_then(extract_source_file)
                }
            }
        }
    }
}

fn canonicalize_definition(
    definition: &TaskDefinition,
    path: &TaskPath,
    entries: &mut BTreeMap<String, IndexedTask>,
    original_name: String,
    source_file: Option<String>,
) -> Result<TaskDefinition> {
    match definition {
        TaskDefinition::Single(task) => {
            let canon_task = canonicalize_task(task.as_ref(), path)?;
            let name = path.canonical();
            entries.insert(
                name.clone(),
                IndexedTask {
                    name,
                    original_name,
                    definition: TaskDefinition::Single(Box::new(canon_task.clone())),
                    is_group: false,
                    source_file,
                },
            );
            Ok(TaskDefinition::Single(Box::new(canon_task)))
        }
        TaskDefinition::Group(group) => match group {
            TaskGroup::Parallel(parallel) => {
                let mut canon_children = HashMap::new();
                for (child_name, child_def) in &parallel.tasks {
                    let child_path = path.join(child_name)?;
                    // For children, extract their own source file and use display name
                    let child_source = extract_source_file(child_def);
                    let child_original = child_name.clone();
                    let canon_child = canonicalize_definition(
                        child_def,
                        &child_path,
                        entries,
                        child_original,
                        child_source,
                    )?;
                    canon_children.insert(child_name.clone(), canon_child);
                }

                let name = path.canonical();
                let definition = TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                    tasks: canon_children,
                    depends_on: parallel.depends_on.clone(),
                }));
                entries.insert(
                    name.clone(),
                    IndexedTask {
                        name,
                        original_name,
                        definition: definition.clone(),
                        is_group: true,
                        source_file,
                    },
                );

                Ok(definition)
            }
            TaskGroup::Sequential(children) => {
                // Preserve sequential children order; dependencies inside them remain as-is
                let mut canon_children = Vec::with_capacity(children.len());
                for child in children {
                    // We still recurse so nested parallel groups are indexed, but we do not
                    // rewrite names with numeric indices to avoid changing existing graph semantics.
                    // For sequential children, extract their source file
                    let child_source = extract_source_file(child);
                    let canon_child = canonicalize_definition(
                        child,
                        path,
                        entries,
                        original_name.clone(),
                        child_source,
                    )?;
                    canon_children.push(canon_child);
                }

                let name = path.canonical();
                let definition = TaskDefinition::Group(TaskGroup::Sequential(canon_children));
                entries.insert(
                    name.clone(),
                    IndexedTask {
                        name,
                        original_name,
                        definition: definition.clone(),
                        is_group: true,
                        source_file,
                    },
                );

                Ok(definition)
            }
        },
    }
}

fn canonicalize_task(task: &Task, path: &TaskPath) -> Result<Task> {
    // Tasks resolved from TaskRef placeholders have their own dependency context (their
    // deps are relative to the referenced task name, not the placeholder name). Avoid
    // re-canonicalizing dependencies under the placeholder namespace.
    if task.project_root.is_some() && task.task_ref.is_none() {
        return Ok(task.clone());
    }

    let mut clone = task.clone();
    let mut canonical_deps = Vec::new();
    for dep in &task.depends_on {
        canonical_deps.push(canonicalize_dep(dep, path)?);
    }
    clone.depends_on = canonical_deps;
    Ok(clone)
}

fn canonicalize_dep(dep: &str, current_path: &TaskPath) -> Result<String> {
    if dep.contains('.') || dep.contains(':') {
        return Ok(TaskPath::parse(dep)?.canonical());
    }

    let mut segments: Vec<String> = current_path.segments().to_vec();
    segments.pop(); // relative to the parent namespace
    segments.push(dep.to_string());

    let rel = TaskPath { segments };
    Ok(rel.canonical())
}

/// Check if two task names are similar (for typo suggestions)
fn is_similar(input: &str, candidate: &str) -> bool {
    // Exact prefix match
    if candidate.starts_with(input) || input.starts_with(candidate) {
        return true;
    }

    // Simple edit distance check for short strings
    let input_lower = input.to_lowercase();
    let candidate_lower = candidate.to_lowercase();

    // Check if they share a common prefix of at least 3 chars
    let common_prefix = input_lower
        .chars()
        .zip(candidate_lower.chars())
        .take_while(|(a, b)| a == b)
        .count();
    if common_prefix >= 3 {
        return true;
    }

    // Check Levenshtein distance for short names
    if input.len() <= 10 && candidate.len() <= 10 {
        let distance = levenshtein(&input_lower, &candidate_lower);
        return distance <= 2;
    }

    false
}

/// Simple Levenshtein distance implementation
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // TaskPath tests
    // ==========================================================================

    #[test]
    fn test_task_path_parse_simple() {
        let path = TaskPath::parse("build").unwrap();
        assert_eq!(path.canonical(), "build");
        assert_eq!(path.segments(), &["build"]);
    }

    #[test]
    fn test_task_path_parse_dotted() {
        let path = TaskPath::parse("test.unit").unwrap();
        assert_eq!(path.canonical(), "test.unit");
        assert_eq!(path.segments(), &["test", "unit"]);
    }

    #[test]
    fn test_task_path_parse_colon_separated() {
        let path = TaskPath::parse("test:integration").unwrap();
        assert_eq!(path.canonical(), "test.integration");
        assert_eq!(path.segments(), &["test", "integration"]);
    }

    #[test]
    fn test_task_path_parse_mixed_separators() {
        let path = TaskPath::parse("build:release.optimized").unwrap();
        assert_eq!(path.canonical(), "build.release.optimized");
    }

    #[test]
    fn test_task_path_parse_empty_error() {
        assert!(TaskPath::parse("").is_err());
        assert!(TaskPath::parse("   ").is_err());
    }

    #[test]
    fn test_task_path_parse_only_separators_error() {
        assert!(TaskPath::parse("...").is_err());
        assert!(TaskPath::parse(":::").is_err());
    }

    #[test]
    fn test_task_path_join() {
        let path = TaskPath::parse("build").unwrap();
        let joined = path.join("release").unwrap();
        assert_eq!(joined.canonical(), "build.release");
    }

    #[test]
    fn test_task_path_join_invalid_segment() {
        let path = TaskPath::parse("build").unwrap();
        assert!(path.join("").is_err());
        assert!(path.join("foo.bar").is_err());
        assert!(path.join("foo:bar").is_err());
    }

    #[test]
    fn test_task_path_equality() {
        let path1 = TaskPath::parse("test.unit").unwrap();
        let path2 = TaskPath::parse("test:unit").unwrap();
        assert_eq!(path1, path2);
    }

    // ==========================================================================
    // validate_segment tests
    // ==========================================================================

    #[test]
    fn test_validate_segment_valid() {
        assert!(validate_segment("build").is_ok());
        assert!(validate_segment("test-unit").is_ok());
        assert!(validate_segment("my_task").is_ok());
        assert!(validate_segment("task123").is_ok());
    }

    #[test]
    fn test_validate_segment_empty() {
        assert!(validate_segment("").is_err());
    }

    #[test]
    fn test_validate_segment_with_dot() {
        assert!(validate_segment("foo.bar").is_err());
    }

    #[test]
    fn test_validate_segment_with_colon() {
        assert!(validate_segment("foo:bar").is_err());
    }

    // ==========================================================================
    // TaskIndex tests
    // ==========================================================================

    #[test]
    fn test_task_index_build_single_task() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo build".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();
        assert_eq!(index.list().len(), 1);

        let resolved = index.resolve("build").unwrap();
        assert_eq!(resolved.name, "build");
        assert!(!resolved.is_group);
    }

    #[test]
    fn test_task_index_build_underscore_prefix() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "_private".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo private".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();

        // Should be accessible without underscore
        let resolved = index.resolve("private").unwrap();
        assert_eq!(resolved.name, "private");
        assert_eq!(resolved.original_name, "_private");
    }

    #[test]
    fn test_task_index_build_nested_tasks() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "test.unit".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo test".to_string(),
                ..Default::default()
            })),
        );
        tasks.insert(
            "test.integration".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo test --test integration".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();
        assert_eq!(index.list().len(), 2);

        // Can resolve with dots
        assert!(index.resolve("test.unit").is_ok());
        // Can resolve with colons
        assert!(index.resolve("test:integration").is_ok());
    }

    #[test]
    fn test_task_index_resolve_not_found() {
        let tasks = HashMap::new();
        let index = TaskIndex::build(&tasks).unwrap();

        let result = index.resolve("nonexistent");
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_task_index_resolve_with_suggestions() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo build".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();

        // Typo: "buld" instead of "build"
        let result = index.resolve("buld");
        assert!(result.is_err());

        let err = result.unwrap_err().to_string();
        assert!(err.contains("Did you mean"));
        assert!(err.contains("build"));
    }

    #[test]
    fn test_task_index_list_deterministic_order() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "zebra".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo z".to_string(),
                ..Default::default()
            })),
        );
        tasks.insert(
            "apple".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo a".to_string(),
                ..Default::default()
            })),
        );
        tasks.insert(
            "mango".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo m".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();
        let list = index.list();

        // BTreeMap should give alphabetical order
        assert_eq!(list[0].name, "apple");
        assert_eq!(list[1].name, "mango");
        assert_eq!(list[2].name, "zebra");
    }

    #[test]
    fn test_task_index_to_tasks() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo build".to_string(),
                ..Default::default()
            })),
        );

        let index = TaskIndex::build(&tasks).unwrap();
        let converted = index.to_tasks();

        assert!(converted.tasks.contains_key("build"));
    }

    // ==========================================================================
    // is_similar and levenshtein tests
    // ==========================================================================

    #[test]
    fn test_is_similar_prefix_match() {
        assert!(is_similar("build", "build-release"));
        assert!(is_similar("test", "testing"));
    }

    #[test]
    fn test_is_similar_common_prefix() {
        assert!(is_similar("build", "builder"));
        assert!(is_similar("testing", "tester"));
    }

    #[test]
    fn test_is_similar_edit_distance() {
        assert!(is_similar("build", "buld")); // 1 deletion
        assert!(is_similar("test", "tset")); // 1 transposition
        assert!(is_similar("task", "taks")); // 1 transposition
    }

    #[test]
    fn test_is_similar_not_similar() {
        assert!(!is_similar("build", "zebra"));
        assert!(!is_similar("a", "xyz"));
    }

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "hello"), 5);
        assert_eq!(levenshtein("hello", ""), 5);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_levenshtein_single_edit() {
        assert_eq!(levenshtein("cat", "car"), 1); // substitution
        assert_eq!(levenshtein("cat", "cats"), 1); // insertion
        assert_eq!(levenshtein("cats", "cat"), 1); // deletion
    }

    #[test]
    fn test_levenshtein_multiple_edits() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    // ==========================================================================
    // IndexedTask tests
    // ==========================================================================

    #[test]
    fn test_indexed_task_debug() {
        let task = IndexedTask {
            name: "build".to_string(),
            original_name: "build".to_string(),
            definition: TaskDefinition::Single(Box::default()),
            is_group: false,
            source_file: Some("env.cue".to_string()),
        };

        let debug = format!("{:?}", task);
        assert!(debug.contains("build"));
        assert!(debug.contains("env.cue"));
    }

    #[test]
    fn test_indexed_task_clone() {
        let task = IndexedTask {
            name: "build".to_string(),
            original_name: "_build".to_string(),
            definition: TaskDefinition::Single(Box::default()),
            is_group: false,
            source_file: None,
        };

        let cloned = task.clone();
        assert_eq!(cloned.name, task.name);
        assert_eq!(cloned.original_name, task.original_name);
    }

    // ==========================================================================
    // WorkspaceTask tests
    // ==========================================================================

    #[test]
    fn test_workspace_task_debug() {
        let task = WorkspaceTask {
            project: "my-project".to_string(),
            task: "build".to_string(),
            task_ref: "#my-project:build".to_string(),
            description: Some("Build the project".to_string()),
            is_group: false,
        };

        let debug = format!("{:?}", task);
        assert!(debug.contains("my-project"));
        assert!(debug.contains("build"));
    }

    #[test]
    fn test_workspace_task_serialize() {
        let task = WorkspaceTask {
            project: "api".to_string(),
            task: "test.unit".to_string(),
            task_ref: "#api:test.unit".to_string(),
            description: None,
            is_group: false,
        };

        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("api"));
        assert!(json.contains("test.unit"));
    }

    // ==========================================================================
    // TaskPath additional tests
    // ==========================================================================

    #[test]
    fn test_task_path_clone() {
        let path = TaskPath::parse("build.release").unwrap();
        let cloned = path.clone();
        assert_eq!(path, cloned);
    }

    #[test]
    fn test_task_path_serialize() {
        let path = TaskPath::parse("test.unit").unwrap();
        let json = serde_json::to_string(&path).unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("unit"));
    }
}
