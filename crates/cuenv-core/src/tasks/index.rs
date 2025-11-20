use super::{Task, TaskDefinition, TaskGroup, Tasks};
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap};

/// Parsed task path that normalizes dotted/colon-separated identifiers
#[derive(Debug, Clone, Eq, PartialEq)]
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

#[derive(Debug, Clone)]
pub struct IndexedTask {
    pub name: String,
    pub definition: TaskDefinition,
    pub is_group: bool,
}

/// Flattened index of all addressable tasks with canonical names
#[derive(Debug, Clone, Default)]
pub struct TaskIndex {
    entries: BTreeMap<String, IndexedTask>,
}

impl TaskIndex {
    /// Build a canonical index from the hierarchical task map
    pub fn build(tasks: &HashMap<String, TaskDefinition>) -> Result<Self> {
        let mut entries = BTreeMap::new();

        for (name, definition) in tasks {
            let path = TaskPath::parse(name)?;
            let _ = canonicalize_definition(definition, &path, &mut entries)?;
        }

        Ok(Self { entries })
    }

    /// Resolve a raw task name (dot or colon separated) to an indexed task
    pub fn resolve(&self, raw: &str) -> Result<&IndexedTask> {
        let path = TaskPath::parse(raw)?;
        self.entries.get(&path.canonical()).ok_or_else(|| {
            let available: Vec<&str> = self.entries.keys().map(String::as_str).collect();
            Error::configuration(format!(
                "Task '{}' not found. Available tasks: {:?}",
                path.canonical(),
                available
            ))
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

fn canonicalize_definition(
    definition: &TaskDefinition,
    path: &TaskPath,
    entries: &mut BTreeMap<String, IndexedTask>,
) -> Result<TaskDefinition> {
    match definition {
        TaskDefinition::Single(task) => {
            let canon_task = canonicalize_task(task.as_ref(), path)?;
            let name = path.canonical();
            entries.insert(
                name.clone(),
                IndexedTask {
                    name,
                    definition: TaskDefinition::Single(Box::new(canon_task.clone())),
                    is_group: false,
                },
            );
            Ok(TaskDefinition::Single(Box::new(canon_task)))
        }
        TaskDefinition::Group(group) => match group {
            TaskGroup::Parallel(children) => {
                let mut canon_children = HashMap::new();
                for (child_name, child_def) in children {
                    let child_path = path.join(child_name)?;
                    let canon_child = canonicalize_definition(child_def, &child_path, entries)?;
                    canon_children.insert(child_name.clone(), canon_child);
                }

                let name = path.canonical();
                let definition = TaskDefinition::Group(TaskGroup::Parallel(canon_children));
                entries.insert(
                    name.clone(),
                    IndexedTask {
                        name,
                        definition: definition.clone(),
                        is_group: true,
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
                    let canon_child = canonicalize_definition(child, path, entries)?;
                    canon_children.push(canon_child);
                }

                let name = path.canonical();
                let definition = TaskDefinition::Group(TaskGroup::Sequential(canon_children));
                entries.insert(
                    name.clone(),
                    IndexedTask {
                        name,
                        definition: definition.clone(),
                        is_group: true,
                    },
                );

                Ok(definition)
            }
        },
    }
}

fn canonicalize_task(task: &Task, path: &TaskPath) -> Result<Task> {
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
