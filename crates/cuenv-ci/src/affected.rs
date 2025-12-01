use cuenv_core::manifest::Cuenv;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[must_use]
pub fn compute_affected_tasks(
    changed_files: &[PathBuf],
    pipeline_tasks: &[String],
    config: &Cuenv,
    project_root: &Path,
) -> Vec<String> {
    let mut affected = HashSet::new();
    let mut directly_affected = HashSet::new();

    // 1. Identify directly affected tasks
    for task_name in pipeline_tasks {
        if let Some(task_def) = config.tasks.get(task_name)
            && let Some(task) = task_def.as_single()
            && task
                .iter_path_inputs()
                .any(|input_glob| matches_any(changed_files, project_root, input_glob))
        {
            directly_affected.insert(task_name.clone());
            affected.insert(task_name.clone());
        }
        // TODO: Handle task groups recursively?
    }

    // 2. Transitive dependencies (simplified)
    // We need to know dependencies between tasks.
    // TaskDefinition has `depends_on` via `as_single()`.

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
                    if affected.contains(dep) {
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
