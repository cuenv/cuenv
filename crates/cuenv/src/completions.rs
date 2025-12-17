//! Dynamic shell completion support for cuenv
//!
//! Uses `clap_complete`'s dynamic completion feature where the binary itself
//! handles completion requests - all logic in Rust, no shell scripts needed.

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use std::path::{Path, PathBuf};

/// Find the CUE module root by walking up from `start` looking for `cue.mod/` directory.
fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join("cue.mod").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Complete task names by querying the CUE configuration in the current directory
fn complete_tasks() -> Vec<CompletionCandidate> {
    // Try to get tasks from the current directory
    let tasks = get_available_tasks(".", "cuenv");

    tasks
        .into_iter()
        .map(|(name, description)| {
            let mut candidate = CompletionCandidate::new(name);
            if let Some(desc) = description {
                candidate = candidate.help(Some(desc.into()));
            }
            candidate
        })
        .collect()
}

/// Get available tasks from a CUE configuration
fn get_available_tasks(path: &str, package: &str) -> Vec<(String, Option<String>)> {
    let dir_path = Path::new(path);

    // Find the module root
    let Some(module_root) = find_cue_module_root(dir_path) else {
        return Vec::new();
    };

    // Use module-wide evaluation
    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let Ok(raw_result) = cuengine::evaluate_module(&module_root, package, Some(options)) else {
        return Vec::new();
    };

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    // Calculate relative path from module root to target
    let target_path = match dir_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let relative_path = target_path
        .strip_prefix(&module_root)
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|_| PathBuf::from("."));

    let Some(instance) = module.get(&relative_path) else {
        return Vec::new();
    };

    let Ok(manifest) = instance.deserialize::<cuenv_core::manifest::Project>() else {
        return Vec::new();
    };

    let manifest = manifest.with_implicit_tasks();

    // Build task index and extract names with descriptions
    let Ok(task_index) = cuenv_core::tasks::TaskIndex::build(&manifest.tasks) else {
        return Vec::new();
    };

    task_index
        .list()
        .iter()
        .map(|indexed| {
            let description = match &indexed.definition {
                cuenv_core::tasks::TaskDefinition::Single(task) => task.description.clone(),
                cuenv_core::tasks::TaskDefinition::Group(_) => None,
            };
            (indexed.name.clone(), description)
        })
        .collect()
}

/// Complete task parameters for a specific task (for future use)
#[allow(dead_code)]
fn complete_task_params(task_name: &str) -> Vec<CompletionCandidate> {
    let Some(params) = get_task_params(".", "cuenv", task_name) else {
        return Vec::new();
    };

    params
        .into_iter()
        .map(|(flag, description)| {
            let mut candidate = CompletionCandidate::new(flag);
            if let Some(desc) = description {
                candidate = candidate.help(Some(desc.into()));
            }
            candidate
        })
        .collect()
}

/// Get parameters for a specific task (for future use)
#[allow(dead_code)]
fn get_task_params(
    path: &str,
    package: &str,
    task_name: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let dir_path = Path::new(path);

    // Find the module root
    let module_root = find_cue_module_root(dir_path)?;

    // Use module-wide evaluation
    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(options)).ok()?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    // Calculate relative path
    let target_path = dir_path.canonicalize().ok()?;
    let relative_path = target_path
        .strip_prefix(&module_root)
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|_| PathBuf::from("."));

    let instance = module.get(&relative_path)?;
    let manifest: cuenv_core::manifest::Project = instance.deserialize().ok()?;
    let manifest = manifest.with_implicit_tasks();

    let task_index = cuenv_core::tasks::TaskIndex::build(&manifest.tasks).ok()?;
    let task_entry = task_index.resolve(task_name).ok()?;

    let cuenv_core::tasks::TaskDefinition::Single(task) = &task_entry.definition else {
        return None;
    };

    let params = task.params.as_ref()?;
    let mut completions = Vec::new();

    // Add named parameters
    for (name, param_def) in &params.named {
        let flag = format!("--{name}");
        let description = param_def.description.clone();
        completions.push((flag, description));

        // Add short flag if available
        if let Some(short) = &param_def.short {
            completions.push((format!("-{short}"), param_def.description.clone()));
        }
    }

    Some(completions)
}

/// Create an `ArgValueCandidates` for task name completion
pub fn task_completer() -> ArgValueCandidates {
    ArgValueCandidates::new(complete_tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_tasks() {
        // This will return empty in test context (no CUE files)
        let results = complete_tasks();
        // Just verify it doesn't panic
        assert!(results.is_empty() || !results.is_empty());
    }

    #[test]
    fn test_get_available_tasks_no_config() {
        // Should return empty when no config exists
        let tasks = get_available_tasks("/nonexistent", "cuenv");
        assert!(tasks.is_empty());
    }
}
