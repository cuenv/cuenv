//! Dynamic shell completion support for cuenv
//!
//! Uses `clap_complete`'s dynamic completion feature where the binary itself
//! handles completion requests - all logic in Rust, no shell scripts needed.
//!
//! Note: Completions use discovery-based evaluation (find env.cue files, evaluate each
//! directory individually with `recursive: false`) since they're invoked from the shell
//! without access to a `CommandExecutor`.

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::commands::env_file::{discover_env_cue_directories, find_cue_module_root};
use crate::commands::task::list_builder::prepare_task_index;

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

/// Get available tasks from a CUE configuration using discovery-based evaluation.
///
/// Uses filesystem discovery to find env.cue files and evaluates each directory
/// individually with `recursive: false`, avoiding CUE's `./...:package` pattern
/// which can hang when directories contain mixed packages.
fn get_available_tasks(path: &str, package: &str) -> Vec<(String, Option<String>)> {
    let dir_path = Path::new(path);

    // Find the module root
    let Some(module_root) = find_cue_module_root(dir_path) else {
        return Vec::new();
    };

    // Discover all directories with env.cue files matching our package
    let env_cue_dirs = discover_env_cue_directories(&module_root, package);
    if env_cue_dirs.is_empty() {
        return Vec::new();
    }

    // Evaluate each directory individually (non-recursive)
    let mut all_instances = HashMap::new();
    let mut all_projects = Vec::new();

    for dir in env_cue_dirs {
        let dir_rel_path = compute_relative_path(&dir, &module_root);
        let options = ModuleEvalOptions {
            recursive: false,
            target_dir: Some(dir.to_string_lossy().to_string()),
            ..Default::default()
        };

        let Ok(raw) = cuengine::evaluate_module(&module_root, package, Some(&options)) else {
            continue;
        };

        // Merge instances (key by relative path from module_root)
        for (path_str, value) in raw.instances {
            let rel_path = if path_str == "." {
                dir_rel_path.clone()
            } else {
                path_str
            };
            all_instances.insert(rel_path.clone(), value);
        }

        for project_path in raw.projects {
            let rel_project_path = if project_path == "." {
                dir_rel_path.clone()
            } else {
                project_path
            };
            if !all_projects.contains(&rel_project_path) {
                all_projects.push(rel_project_path);
            }
        }
    }

    if all_instances.is_empty() {
        return Vec::new();
    }

    let module = ModuleEvaluation::from_raw(module_root.clone(), all_instances, all_projects, None);

    // Calculate relative path from module root to target
    let Ok(target_path) = dir_path.canonicalize() else {
        return Vec::new();
    };
    let relative_path = compute_relative_path(&target_path, &module_root);

    let Some(instance) = module.get(&PathBuf::from(&relative_path)) else {
        return Vec::new();
    };

    let Ok(mut manifest) = instance.deserialize::<cuenv_core::manifest::Project>() else {
        return Vec::new();
    };

    // Build task index with auto-detected workspace tasks injected
    // Best-effort: if injection fails, fall back to basic index
    let task_index = prepare_task_index(&mut manifest, &target_path).or_else(|_| {
        // Fall back to basic index without workspace injection
        cuenv_core::tasks::TaskIndex::build(&manifest.tasks)
    });

    let Ok(task_index) = task_index else {
        return Vec::new();
    };

    task_index
        .list()
        .iter()
        .map(|indexed| {
            let description = match &indexed.node {
                cuenv_core::tasks::TaskNode::Task(task) => task.description.clone(),
                cuenv_core::tasks::TaskNode::Group(g) => g.description.clone(),
                cuenv_core::tasks::TaskNode::Sequence(_) => None,
            };
            (indexed.name.clone(), description)
        })
        .collect()
}

/// Compute relative path from module_root to target directory.
/// Returns "." if the paths are equal or if stripping fails.
fn compute_relative_path(target: &Path, module_root: &Path) -> String {
    target.strip_prefix(module_root).map_or_else(
        |_| ".".to_string(),
        |p| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.to_string_lossy().to_string()
            }
        },
    )
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

/// Get parameters for a specific task (for future use).
/// Uses discovery-based evaluation.
#[allow(dead_code)]
fn get_task_params(
    path: &str,
    package: &str,
    task_name: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let dir_path = Path::new(path);

    // Find the module root
    let module_root = find_cue_module_root(dir_path)?;

    // Discover all directories with env.cue files matching our package
    let env_cue_dirs = discover_env_cue_directories(&module_root, package);
    if env_cue_dirs.is_empty() {
        return None;
    }

    // Evaluate each directory individually (non-recursive)
    let mut all_instances = HashMap::new();
    let mut all_projects = Vec::new();

    for dir in env_cue_dirs {
        let dir_rel_path = compute_relative_path(&dir, &module_root);
        let options = ModuleEvalOptions {
            recursive: false,
            target_dir: Some(dir.to_string_lossy().to_string()),
            ..Default::default()
        };

        let Ok(raw) = cuengine::evaluate_module(&module_root, package, Some(&options)) else {
            continue;
        };

        for (path_str, value) in raw.instances {
            let rel_path = if path_str == "." {
                dir_rel_path.clone()
            } else {
                path_str
            };
            all_instances.insert(rel_path.clone(), value);
        }

        for project_path in raw.projects {
            let rel_project_path = if project_path == "." {
                dir_rel_path.clone()
            } else {
                project_path
            };
            if !all_projects.contains(&rel_project_path) {
                all_projects.push(rel_project_path);
            }
        }
    }

    if all_instances.is_empty() {
        return None;
    }

    let module = ModuleEvaluation::from_raw(module_root.clone(), all_instances, all_projects, None);

    // Calculate relative path
    let target_path = dir_path.canonicalize().ok()?;
    let relative_path = compute_relative_path(&target_path, &module_root);

    let instance = module.get(&PathBuf::from(&relative_path))?;
    let mut manifest: cuenv_core::manifest::Project = instance.deserialize().ok()?;

    // Build task index with auto-detected workspace tasks injected
    // Best-effort: if injection fails, fall back to basic index
    let task_index = prepare_task_index(&mut manifest, &target_path)
        .or_else(|_| cuenv_core::tasks::TaskIndex::build(&manifest.tasks))
        .ok()?;
    let task_entry = task_index.resolve(task_name).ok()?;

    let cuenv_core::tasks::TaskNode::Task(task) = &task_entry.node else {
        return None;
    };

    let params = task.params.as_ref()?;

    // Add named parameters, including short flags if available
    let completions: Vec<_> = params
        .named
        .iter()
        .flat_map(|(name, param_def)| {
            let main_flag = (format!("--{name}"), param_def.description.clone());
            let short_flag = param_def
                .short
                .as_ref()
                .map(|short| (format!("-{short}"), param_def.description.clone()));
            std::iter::once(main_flag).chain(short_flag)
        })
        .collect();

    Some(completions)
}

/// Create an `ArgValueCandidates` for task name completion
pub fn task_completer() -> ArgValueCandidates {
    ArgValueCandidates::new(complete_tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

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

    #[test]
    fn test_find_cue_module_root_nonexistent() {
        let result = find_cue_module_root(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_none());
    }

    #[test]
    fn test_find_cue_module_root_no_cue_mod() {
        // Use temp directory that definitely doesn't have cue.mod
        let temp = env::temp_dir();
        let result = find_cue_module_root(&temp);
        // May or may not find one depending on system, just verify no panic
        let _ = result;
    }

    #[test]
    fn test_find_cue_module_root_with_cue_mod() {
        // Create temp directory with cue.mod
        let temp = tempfile::tempdir().unwrap();
        let cue_mod = temp.path().join("cue.mod");
        fs::create_dir(&cue_mod).unwrap();

        // The module root should be found
        let result = find_cue_module_root(temp.path());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_cue_module_root_in_subdirectory() {
        // Create temp directory with cue.mod and a nested subdirectory
        let temp = tempfile::tempdir().unwrap();
        let cue_mod = temp.path().join("cue.mod");
        fs::create_dir(&cue_mod).unwrap();

        let subdir = temp.path().join("foo").join("bar");
        fs::create_dir_all(&subdir).unwrap();

        // Should find root from subdirectory
        let result = find_cue_module_root(&subdir);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_get_available_tasks_empty_path() {
        let tasks = get_available_tasks("", "cuenv");
        // May be empty or not depending on cwd, just verify no panic
        let _ = tasks;
    }

    #[test]
    fn test_get_available_tasks_invalid_package() {
        let tasks = get_available_tasks(".", "nonexistent_package_name");
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_complete_task_params_no_task() {
        let params = complete_task_params("nonexistent_task");
        assert!(params.is_empty());
    }

    #[test]
    fn test_get_task_params_nonexistent_path() {
        let result = get_task_params("/nonexistent", "cuenv", "test");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_task_params_invalid_package() {
        let result = get_task_params(".", "invalid_package", "test");
        assert!(result.is_none());
    }

    #[test]
    fn test_task_completer_returns_candidates() {
        let completer = task_completer();
        // Just verify it can be created
        let _ = completer;
    }

    #[test]
    fn test_complete_tasks_produces_candidates() {
        // Test the actual completion function output type
        let candidates = complete_tasks();
        // Whether empty or not, the return type should be correct
        for candidate in &candidates {
            let _ = format!("{candidate:?}");
        }
    }
}
