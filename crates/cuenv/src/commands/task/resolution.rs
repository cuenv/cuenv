//! Task reference resolution
//!
//! Handles resolving `TaskRef` placeholders to concrete tasks by looking up
//! referenced tasks in the discovery context and merging dependencies.

use std::collections::HashMap;
use std::fs;

use cuenv_core::tasks::discovery::TaskDiscovery;
use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::{Task, TaskDefinition};

use super::normalization::{canonicalize_dep_for_task_name, normalize_dep};

/// Resolve `TaskRef` placeholders in a task definition tree.
///
/// When a task has a `task_ref` field (e.g., "#project:task"), this function
/// looks up the referenced task in the discovery context and replaces the
/// placeholder with the actual task definition. Dependencies from both the
/// placeholder and the referenced task are merged.
///
/// The resolved task's dependencies are canonicalized relative to the
/// referenced task's name (not the placeholder name) to ensure correct
/// dependency resolution during indexing.
pub(crate) fn resolve_task_refs_in_definition(
    def: &mut TaskDefinition,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    match def {
        TaskDefinition::Single(task) => {
            let Some(task_ref_str) = task.task_ref.clone() else {
                return;
            };
            let parsed_ref = TaskRef { ref_: task_ref_str };

            let placeholder_deps = std::mem::take(&mut task.depends_on)
                .into_iter()
                .map(|d| normalize_dep(&d, manifest_project_id, project_id_by_name))
                .collect::<Vec<_>>();

            match discovery.resolve_ref(&parsed_ref) {
                Ok(matched) => {
                    let mut resolved = matched.task;
                    let resolved_root =
                        fs::canonicalize(&matched.project_root).unwrap_or(matched.project_root);
                    resolved.project_root = Some(resolved_root);
                    resolved.task_ref = None;

                    // Canonicalize the referenced task's dependencies relative to the
                    // referenced task name (NOT the placeholder task name), so later indexing
                    // doesn't reinterpret them under the hook task namespace.
                    let deps = std::mem::take(&mut resolved.depends_on);
                    resolved.depends_on = deps
                        .into_iter()
                        .map(|d| canonicalize_dep_for_task_name(&d, &matched.task_name))
                        .collect();

                    for dep in placeholder_deps {
                        if !resolved.depends_on.contains(&dep) {
                            resolved.depends_on.push(dep);
                        }
                    }
                    **task = resolved;
                }
                Err(e) => {
                    tracing::warn!("Failed to resolve TaskRef {}: {}", parsed_ref.ref_, e);
                    // Restore placeholder deps so later normalization still has them.
                    task.depends_on = placeholder_deps;
                }
            }
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    resolve_task_refs_in_definition(
                        t,
                        discovery,
                        manifest_project_id,
                        project_id_by_name,
                    );
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                for t in parallel.tasks.values_mut() {
                    resolve_task_refs_in_definition(
                        t,
                        discovery,
                        manifest_project_id,
                        project_id_by_name,
                    );
                }
            }
        },
    }
}

/// Resolve all `TaskRefs` in a project manifest.
///
/// Iterates through all task definitions in the manifest and resolves
/// any `TaskRef` placeholders.
pub(crate) fn resolve_task_refs_in_manifest(
    manifest: &mut cuenv_core::manifest::Project,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    for def in manifest.tasks.values_mut() {
        resolve_task_refs_in_definition(def, discovery, manifest_project_id, project_id_by_name);
    }
}

/// Get a mutable reference to a task by its dotted/colon path.
///
/// Navigates through parallel task groups to find the task at the specified path.
/// Returns `None` if the path doesn't exist or points to a group (not a single task).
///
/// # Path Format
/// - `"build"` - top-level task named "build"
/// - `"test.unit"` or `"test:unit"` - task "unit" inside parallel group "test"
pub(crate) fn get_task_mut_by_path<'a>(
    tasks: &'a mut HashMap<String, TaskDefinition>,
    raw_path: &str,
) -> Option<&'a mut Task> {
    let normalized = raw_path.replace(':', ".");
    let mut segments = normalized
        .split('.')
        .filter(|s| !s.is_empty())
        .map(str::trim)
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return None;
    }

    let first = segments.remove(0);
    let mut current = tasks.get_mut(first)?;
    for seg in segments {
        match current {
            TaskDefinition::Group(cuenv_core::tasks::TaskGroup::Parallel(group)) => {
                current = group.tasks.get_mut(seg)?;
            }
            _ => return None,
        }
    }

    match current {
        TaskDefinition::Single(task) => Some(task.as_mut()),
        TaskDefinition::Group(_) => None,
    }
}

/// Get a mutable reference to a task by name or path.
///
/// First tries a direct lookup for the normalized name (covers injected implicit
/// tasks like "bun.install" that are stored as top-level keys). Falls back to
/// nested path lookup for tasks defined within groups.
pub(crate) fn get_task_mut_by_name_or_path<'a>(
    tasks: &'a mut HashMap<String, TaskDefinition>,
    raw_path: &str,
) -> Option<&'a mut Task> {
    let normalized = raw_path.replace(':', ".");

    // Prefer direct lookup for top-level keys (covers injected implicit tasks like "bun.install")
    if tasks.contains_key(&normalized) {
        return match tasks.get_mut(&normalized) {
            Some(TaskDefinition::Single(task)) => Some(task.as_mut()),
            _ => None,
        };
    }

    // Fallback: nested lookup (covers `tasks: bun: install: {}`)
    get_task_mut_by_path(tasks, &normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::{ParallelGroup, TaskDefinition, TaskGroup};

    #[test]
    fn test_get_task_mut_by_path_simple() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo build".to_string(),
                ..Default::default()
            })),
        );

        let task = get_task_mut_by_path(&mut tasks, "build");
        assert!(task.is_some());
        assert_eq!(task.unwrap().command, "echo build");
    }

    #[test]
    fn test_get_task_mut_by_path_nested() {
        use cuenv_core::tasks::{ParallelGroup, TaskGroup};

        let mut inner = HashMap::new();
        inner.insert(
            "unit".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo test".to_string(),
                ..Default::default()
            })),
        );

        let mut tasks = HashMap::new();
        tasks.insert(
            "test".to_string(),
            TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                tasks: inner,
                depends_on: Vec::new(),
            })),
        );

        let task = get_task_mut_by_path(&mut tasks, "test.unit");
        assert!(task.is_some());
        assert_eq!(task.unwrap().command, "cargo test");

        // Also works with colon separator
        let task2 = get_task_mut_by_path(&mut tasks, "test:unit");
        assert!(task2.is_some());
    }

    #[test]
    fn test_get_task_mut_by_path_not_found() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task::default())),
        );

        assert!(get_task_mut_by_path(&mut tasks, "nonexistent").is_none());
        assert!(get_task_mut_by_path(&mut tasks, "").is_none());
    }

    #[test]
    fn test_get_task_mut_by_name_or_path_direct() {
        let mut tasks = HashMap::new();
        // Simulate an injected task with a dotted key
        tasks.insert(
            "bun.install".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun install".to_string(),
                ..Default::default()
            })),
        );

        let task = get_task_mut_by_name_or_path(&mut tasks, "bun.install");
        assert!(task.is_some());
        assert_eq!(task.unwrap().command, "bun install");
    }
}
