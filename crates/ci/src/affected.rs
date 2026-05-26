use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use cuenv_core::{AffectedBy, matches_pattern};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn compute_affected_tasks(
    changed_files: &[PathBuf],
    pipeline_tasks: &[String],
    project_root: &Path,
    config: &Project,
    all_projects: &HashMap<String, (PathBuf, Project)>,
) -> Vec<String> {
    tracing::trace!(
        project_root = %project_root.display(),
        pipeline_tasks = ?pipeline_tasks,
        changed_files = ?changed_files,
        "Computing affected tasks for CI pipeline"
    );

    let mut affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();

    // Build task index for resolving nested task names
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        tracing::trace!("Failed to build task index; falling back to direct pipeline checks");
        // Without an index we can only do direct checks on pipeline tasks
        for task_name in pipeline_tasks {
            if is_task_directly_affected(task_name, config, changed_files, project_root) {
                tracing::trace!(
                    task = task_name,
                    "Marked pipeline task affected during fallback"
                );
                affected.insert(task_name.clone());
            }
        }
        let fallback_result: Vec<String> = pipeline_tasks
            .iter()
            .filter(|t| affected.contains(*t))
            .cloned()
            .collect();
        tracing::trace!(affected_tasks = ?fallback_result, "Affected task computation complete");
        return fallback_result;
    };

    // Expand pipeline tasks to the full DAG so every dependency is evaluated
    let all_dag_tasks = collect_dag_tasks(pipeline_tasks, &index);
    tracing::trace!(all_dag_tasks = ?all_dag_tasks, "Expanded pipeline task DAG");

    // 1. Identify directly affected tasks across the entire DAG
    for task_name in &all_dag_tasks {
        let directly_affected =
            is_task_directly_affected(task_name, config, changed_files, project_root);
        tracing::trace!(
            task = task_name,
            directly_affected,
            "Evaluated direct task affect"
        );
        if directly_affected {
            affected.insert(task_name.clone());
        }
    }

    // 2. Propagate affected status through dependencies (fix-point loop)
    // A task is transitively affected if any of its dependencies are affected.
    let mut changed = true;
    while changed {
        changed = false;
        for task_name in &all_dag_tasks {
            if affected.contains(task_name) {
                continue;
            }

            if let Ok(entry) = index.resolve(task_name)
                && let Some(task) = entry.node.as_task()
                && !task.depends_on.is_empty()
            {
                for dep in &task.depends_on {
                    let dep_name = dep.task_name();
                    let dep_affected = if dep_name.starts_with('#') {
                        check_external_dependency(
                            dep_name,
                            all_projects,
                            changed_files,
                            &mut visited_external_cache,
                        )
                    } else {
                        affected.contains(dep_name)
                    };

                    tracing::trace!(
                        task = task_name,
                        dependency = dep_name,
                        dep_affected,
                        "Evaluated dependency while propagating affected status"
                    );

                    if dep_affected {
                        tracing::trace!(
                            task = task_name,
                            dependency = dep_name,
                            "Marked task transitively affected via dependency"
                        );
                        affected.insert(task_name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
    }

    // Return in pipeline order, filtered to pipeline tasks only.
    // The executor handles dependency resolution when running each task.
    let result: Vec<String> = pipeline_tasks
        .iter()
        .filter(|t| affected.contains(*t))
        .cloned()
        .collect();
    tracing::trace!(affected_tasks = ?result, "Affected task computation complete");
    result
}

/// Walk the dependency graph from `roots` and collect every task reachable
/// through `depends_on`, including the roots themselves.
fn collect_dag_tasks(roots: &[String], index: &TaskIndex) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut queue = std::collections::VecDeque::new();

    for root in roots {
        if visited.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }

    while let Some(task_name) = queue.pop_front() {
        if let Ok(entry) = index.resolve(&task_name)
            && let Some(task) = entry.node.as_task()
        {
            for dep in &task.depends_on {
                let dep_name = dep.task_name();
                // Only follow internal dependencies; external (#project:task) are
                // handled separately during propagation.
                if !dep_name.starts_with('#') && visited.insert(dep_name.to_string()) {
                    queue.push_back(dep_name.to_string());
                }
            }
        }
    }

    visited.into_iter().collect()
}

#[must_use]
pub fn matched_inputs_for_task(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> Vec<String> {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return Vec::new();
    };

    let Ok(entry) = index.resolve(task_name) else {
        return Vec::new();
    };

    let Some(task) = entry.node.as_task() else {
        return Vec::new();
    };

    task.iter_path_inputs()
        .filter(|input_glob| matches_pattern(changed_files, project_root, input_glob))
        .cloned()
        .collect()
}

/// Check if a task is directly affected by file changes.
///
/// Uses the [`AffectedBy`] trait implementation from cuenv-core, which handles:
/// - Single tasks: affected if any input pattern matches changed files
/// - Task groups: affected if ANY subtask is affected
/// - Tasks with no inputs: always considered affected (safe default)
///
/// This function uses `TaskIndex` to resolve nested task names like "deploy.preview"
/// which are stored in CUE as hierarchical structures (e.g., `deploy: preview: {...}`).
fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return false;
    };

    index
        .resolve(task_name)
        .ok()
        .is_some_and(|entry| entry.node.is_affected_by(changed_files, project_root))
}

/// Check if an external dependency (cross-project task) is affected by file changes.
///
/// External dependencies are specified in the format `#project:task`. This function
/// recursively checks if the referenced task or any of its dependencies are affected.
///
/// # Recursion Prevention
///
/// To prevent infinite loops with circular dependencies, we insert a `false` sentinel
/// value into the cache before checking. If we encounter this dependency again during
/// recursion, we return false (not affected). Once the check completes, the cache is
/// updated with the actual result.
#[allow(clippy::implicit_hasher)]
fn check_external_dependency(
    dep: &str,
    all_projects: &HashMap<String, (PathBuf, Project)>,
    changed_files: &[PathBuf],
    cache: &mut HashMap<String, bool>,
) -> bool {
    // dep format: "#project:task"
    if let Some(result) = cache.get(dep) {
        return *result;
    }

    // Insert false as a sentinel to prevent infinite recursion on circular dependencies.
    // This will be updated with the actual result once the check completes.
    cache.insert(dep.to_string(), false);

    let parts: Vec<&str> = dep[1..].split(':').collect();
    if parts.len() < 2 {
        return false;
    }
    let project_name = parts[0];
    let task_name = parts[1];

    let Some((project_path, project_config)) = all_projects.get(project_name) else {
        return false;
    };

    // Check if directly affected
    if is_task_directly_affected(task_name, project_config, changed_files, project_path) {
        cache.insert(dep.to_string(), true);
        return true;
    }

    // Check transitive dependencies of the external task
    // Use TaskIndex to resolve nested task names
    let Ok(index) = TaskIndex::build(&project_config.tasks) else {
        return false;
    };
    if let Ok(entry) = index.resolve(task_name)
        && let Some(task) = entry.node.as_task()
    {
        for sub_dep in &task.depends_on {
            let sub_dep_name = sub_dep.task_name();
            if sub_dep_name.starts_with('#') {
                // External ref - no longer supported but keeping check for safety
                if check_external_dependency(sub_dep_name, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            } else {
                // Internal ref within that project
                // We need to resolve internal deps of the external project recursively.
                // Construct implicit external ref: #project:sub_dep
                let implicit_ref = format!("#{project_name}:{sub_dep_name}");
                if check_external_dependency(&implicit_ref, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
#[path = "affected_tests.rs"]
mod tests;
