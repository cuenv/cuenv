use cuenv_core::AffectedBy;
use cuenv_core::manifest::Project;
use cuenv_task_exec::TaskIndex;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

/// Changed files relative to a project root.
#[derive(Clone, Copy)]
pub struct ChangedInputs<'changes> {
    /// Files changed for the current event.
    pub files: &'changes [PathBuf],
    /// Project root used to match task input paths.
    pub project_root: &'changes Path,
}

/// Project data used to resolve direct and cross-project dependencies.
pub struct AffectedProjects<'projects, S> {
    /// Project whose pipeline tasks are being selected.
    pub project: &'projects Project,
    /// Workspace projects available to external task references.
    pub all_projects: &'projects HashMap<String, (PathBuf, Project), S>,
}

struct ExternalDependencyContext<'context, S> {
    projects: &'context HashMap<String, (PathBuf, Project), S>,
    changed_files: &'context [PathBuf],
    cache: &'context mut HashMap<String, bool>,
    index_cache: &'context mut HashMap<String, Option<TaskIndex>>,
}

/// Return pipeline tasks affected by changed files or their dependencies.
///
/// Results follow the pipeline's task order. If the task index cannot be
/// built, only direct matches can be determined.
#[must_use]
pub fn compute_affected_tasks<S>(
    changed: ChangedInputs<'_>,
    pipeline_tasks: &[String],
    projects: &AffectedProjects<'_, S>,
) -> Vec<String>
where
    S: BuildHasher,
{
    let ChangedInputs {
        files: changed_files,
        project_root,
    } = changed;
    let AffectedProjects {
        project: config,
        all_projects,
    } = projects;
    tracing::trace!(
        project_root = %project_root.display(),
        pipeline_tasks = ?pipeline_tasks,
        changed_files = ?changed_files,
        "Computing affected tasks for CI pipeline"
    );

    let mut affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();
    let mut external_index_cache: HashMap<String, Option<TaskIndex>> = HashMap::new();
    let changed_inputs = ChangedInputs {
        files: changed_files,
        project_root,
    };

    // Build task index for resolving nested task names
    let index = match TaskIndex::build(&config.tasks) {
        Ok(index) => index,
        Err(error) => {
            // Fail closed: without an index nothing can be shown to be
            // unaffected, and reporting "no affected tasks" would turn CI
            // green without running anything.
            tracing::warn!(
                %error,
                "Failed to build task index; treating every pipeline task as affected"
            );
            return pipeline_tasks.to_vec();
        }
    };

    // Expand pipeline tasks to the full DAG so every dependency is evaluated
    let all_dag_tasks = collect_dag_tasks(pipeline_tasks, &index);
    tracing::trace!(all_dag_tasks = ?all_dag_tasks, "Expanded pipeline task DAG");

    // 1. Identify directly affected tasks across the entire DAG
    for task_name in &all_dag_tasks {
        let directly_affected =
            is_indexed_task_directly_affected(&index, task_name, changed_inputs);
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
                            &mut ExternalDependencyContext {
                                projects: all_projects,
                                changed_files,
                                cache: &mut visited_external_cache,
                                index_cache: &mut external_index_cache,
                            },
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

/// Return the input patterns of `task_name` that match the changed files.
///
/// Returns an empty list when the task or its task index cannot be resolved.
#[must_use]
pub fn matched_inputs_for_task(
    task_name: &str,
    config: &Project,
    changed: ChangedInputs<'_>,
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
        .filter(|input_glob| {
            cuenv_core::matches_pattern(changed.files, changed.project_root, input_glob)
        })
        .map(str::to_owned)
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
#[cfg(test)]
fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed: ChangedInputs<'_>,
) -> bool {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return false;
    };

    is_indexed_task_directly_affected(&index, task_name, changed)
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
fn check_external_dependency<S>(dep: &str, context: &mut ExternalDependencyContext<'_, S>) -> bool
where
    S: BuildHasher,
{
    // dep format: "#project:task"; task may itself contain ':' for nested paths.
    if let Some(result) = context.cache.get(dep) {
        return *result;
    }

    // Insert false as a sentinel to prevent infinite recursion on circular dependencies.
    // This will be updated with the actual result once the check completes.
    context.cache.insert(dep.to_string(), false);

    let Some((project_name, task_name)) = dep[1..].split_once(':') else {
        return false;
    };

    let Some((project_path, project_config)) = context.projects.get(project_name) else {
        return false;
    };

    let dependency_names = {
        let Some(index) = cached_task_index(context.index_cache, project_name, project_config)
        else {
            return false;
        };

        if is_indexed_task_directly_affected(
            index,
            task_name,
            ChangedInputs {
                files: context.changed_files,
                project_root: project_path,
            },
        ) {
            context.cache.insert(dep.to_string(), true);
            return true;
        }

        task_dependency_names(index, task_name)
    };

    for sub_dep_name in dependency_names {
        let dependency_ref = if sub_dep_name.starts_with('#') {
            sub_dep_name
        } else {
            format!("#{project_name}:{sub_dep_name}")
        };

        if check_external_dependency(&dependency_ref, context) {
            context.cache.insert(dep.to_string(), true);
            return true;
        }
    }

    false
}

fn cached_task_index<'a>(
    cache: &'a mut HashMap<String, Option<TaskIndex>>,
    project_name: &str,
    project_config: &Project,
) -> Option<&'a TaskIndex> {
    cache
        .entry(project_name.to_string())
        .or_insert_with(|| TaskIndex::build(&project_config.tasks).ok())
        .as_ref()
}

fn task_dependency_names(index: &TaskIndex, task_name: &str) -> Vec<String> {
    index
        .resolve(task_name)
        .ok()
        .and_then(|entry| {
            entry.node.as_task().map(|task| {
                task.depends_on
                    .iter()
                    .map(|dep| dep.task_name().to_string())
                    .collect()
            })
        })
        .unwrap_or_default()
}

fn is_indexed_task_directly_affected(
    index: &TaskIndex,
    task_name: &str,
    changed: ChangedInputs<'_>,
) -> bool {
    index.resolve(task_name).is_ok_and(|entry| {
        entry
            .node
            .is_affected_by(changed.files, changed.project_root)
    })
}

#[cfg(test)]
#[path = "affected_tests.rs"]
mod tests;
