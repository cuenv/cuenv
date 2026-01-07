//! Task reference resolution
//!
//! Handles resolving `TaskRef` placeholders to concrete tasks by looking up
//! referenced tasks in the discovery context and merging dependencies.

use std::collections::HashMap;
use std::fs;

use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::TaskNode;
use cuenv_task_discovery::TaskDiscovery;

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
pub fn resolve_task_refs_in_node(
    node: &mut TaskNode,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    match node {
        TaskNode::Task(task) => {
            let Some(task_ref_str) = task.task_ref.clone() else {
                return;
            };
            let parsed_ref = TaskRef { ref_: task_ref_str };

            let placeholder_deps: Vec<cuenv_core::tasks::TaskDependency> =
                std::mem::take(&mut task.depends_on)
                    .into_iter()
                    .map(|d| {
                        let normalized =
                            normalize_dep(d.task_name(), manifest_project_id, project_id_by_name);
                        cuenv_core::tasks::TaskDependency::from_name(normalized)
                    })
                    .collect();

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
                        .map(|d| {
                            let canonical =
                                canonicalize_dep_for_task_name(d.task_name(), &matched.task_name);
                            cuenv_core::tasks::TaskDependency::from_name(canonical)
                        })
                        .collect();

                    for dep in placeholder_deps {
                        if !resolved
                            .depends_on
                            .iter()
                            .any(|d| d.task_name() == dep.task_name())
                        {
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
        TaskNode::Group(group) => {
            for t in group.parallel.values_mut() {
                resolve_task_refs_in_node(t, discovery, manifest_project_id, project_id_by_name);
            }
        }
        TaskNode::List(list) => {
            for t in &mut list.steps {
                resolve_task_refs_in_node(t, discovery, manifest_project_id, project_id_by_name);
            }
        }
    }
}

/// Resolve all `TaskRefs` in a project manifest.
///
/// Iterates through all task nodes in the manifest and resolves
/// any `TaskRef` placeholders.
pub fn resolve_task_refs_in_manifest(
    manifest: &mut cuenv_core::manifest::Project,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    for node in manifest.tasks.values_mut() {
        resolve_task_refs_in_node(node, discovery, manifest_project_id, project_id_by_name);
    }
}
