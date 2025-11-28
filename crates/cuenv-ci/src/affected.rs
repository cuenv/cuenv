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
        {
            // Check inputs
            if !task.inputs.is_empty() {
                for input_glob in &task.inputs {
                    // Resolve glob relative to project root
                    // Check if any changed file matches this glob
                    if matches_any(changed_files, project_root, input_glob) {
                        directly_affected.insert(task_name.clone());
                        affected.insert(task_name.clone());
                        break;
                    }
                }
            }
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
    let Ok(glob) = glob::Pattern::new(pattern) else {
        return false;
    };

    for file in files {
        // Check if file is inside the project root
        if let Ok(relative_path) = file.strip_prefix(root)
            && glob.matches_path(relative_path)
        {
            return true;
        }
    }

    false
}
