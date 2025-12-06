use crate::discovery::Project;
use cuenv_core::manifest::Cuenv;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn compute_affected_tasks(
    changed_files: &[PathBuf],
    pipeline_tasks: &[String],
    project_root: &Path,
    config: &Cuenv,
    all_projects: &HashMap<String, Project>,
) -> Vec<String> {
    let mut affected = HashSet::new();
    let mut directly_affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();

    // 1. Identify directly affected tasks (file changes in this project)
    for task_name in pipeline_tasks {
        if is_task_directly_affected(task_name, config, changed_files, project_root) {
            directly_affected.insert(task_name.clone());
            affected.insert(task_name.clone());
        }
    }

    // 2. Transitive dependencies
    // We need to check dependencies recursively including cross-project ones
    let mut changed = true;
    while changed {
        changed = false;
        for task_name in pipeline_tasks {
            if affected.contains(task_name) {
                continue;
            }

            if let Some(task_def) = config.tasks.get(task_name)
                && let Some(task) = task_def.as_single()
                && !task.depends_on.is_empty()
            {
                for dep in &task.depends_on {
                    // Internal dependency
                    if !dep.starts_with('#') {
                        if affected.contains(dep) {
                            affected.insert(task_name.clone());
                            changed = true;
                            break;
                        }
                        continue;
                    }

                    // External dependency (#project:task)
                    if check_external_dependency(
                        dep,
                        all_projects,
                        changed_files,
                        &mut visited_external_cache,
                    ) {
                        affected.insert(task_name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
    }

    // Return in pipeline order
    pipeline_tasks
        .iter()
        .filter(|t| affected.contains(*t))
        .cloned()
        .collect()
}

fn is_task_directly_affected(
    task_name: &str,
    config: &Cuenv,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    if let Some(task_def) = config.tasks.get(task_name)
        && let Some(task) = task_def.as_single()
    {
        task.iter_path_inputs()
            .any(|input_glob| matches_any(changed_files, project_root, input_glob))
    } else {
        false
    }
}

#[allow(clippy::implicit_hasher)]
fn check_external_dependency(
    dep: &str,
    all_projects: &HashMap<String, Project>,
    changed_files: &[PathBuf],
    cache: &mut HashMap<String, bool>,
) -> bool {
    // dep format: "#project:task"
    if let Some(result) = cache.get(dep) {
        return *result;
    }

    // Break recursion cycle by assuming false initially (or handle cycles better?)
    // For DAGs, temporary false is okay.
    cache.insert(dep.to_string(), false);

    let parts: Vec<&str> = dep[1..].split(':').collect();
    if parts.len() < 2 {
        return false;
    }
    let project_name = parts[0];
    let task_name = parts[1];

    let Some(project) = all_projects.get(project_name) else {
        return false;
    };

    let project_root = project.path.parent().unwrap_or_else(|| Path::new("."));

    // Check if directly affected
    if is_task_directly_affected(task_name, &project.config, changed_files, project_root) {
        cache.insert(dep.to_string(), true);
        return true;
    }

    // Check transitive dependencies of the external task
    if let Some(task_def) = project.config.tasks.get(task_name)
        && let Some(task) = task_def.as_single()
    {
        for sub_dep in &task.depends_on {
            if sub_dep.starts_with('#') {
                // External ref
                if check_external_dependency(sub_dep, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            } else {
                // Internal ref within that project
                // We need to resolve internal deps of the external project recursively.
                // Construct implicit external ref: #project:sub_dep
                let implicit_ref = format!("#{project_name}:{sub_dep}");
                if check_external_dependency(&implicit_ref, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            }
        }
    }

    false
}

fn matches_any(files: &[PathBuf], root: &Path, pattern: &str) -> bool {
    // If pattern doesn't contain glob characters, treat it as a path prefix
    // e.g., "crates" should match "crates/foo/bar.rs"
    let is_simple_path = !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[');

    for file in files {
        // Get relative path - if root is "." or empty, use file as-is
        // Otherwise strip the prefix
        let relative_path = if root == Path::new(".") || root.as_os_str().is_empty() {
            file.as_path()
        } else {
            match file.strip_prefix(root) {
                Ok(p) => p,
                Err(_) => continue,
            }
        };

        if is_simple_path {
            // Check if the pattern is a prefix of the file path or exact match
            let pattern_path = Path::new(pattern);
            if relative_path.starts_with(pattern_path) || relative_path == pattern_path {
                return true;
            }
        } else {
            // Use glob matching for patterns with wildcards
            let Ok(glob) = glob::Pattern::new(pattern) else {
                continue;
            };
            if glob.matches_path(relative_path) {
                return true;
            }
        }
    }

    false
}
