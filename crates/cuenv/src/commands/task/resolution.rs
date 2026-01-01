//! Task reference resolution
//!
//! Handles resolving `TaskRef` placeholders to concrete tasks by looking up
//! referenced tasks in the discovery context and merging dependencies.

use std::collections::HashMap;
use std::fs;

use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::discovery::TaskDiscovery;
use cuenv_core::tasks::TaskDefinition;

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
pub fn resolve_task_refs_in_definition(
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
pub fn resolve_task_refs_in_manifest(
    manifest: &mut cuenv_core::manifest::Project,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    for def in manifest.tasks.values_mut() {
        resolve_task_refs_in_definition(def, discovery, manifest_project_id, project_id_by_name);
    }
}

