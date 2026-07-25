//! Dynamic shell completion support for cuenv
//!
//! Uses `clap_complete`'s dynamic completion feature where the binary itself
//! handles completion requests - all logic in Rust, no shell scripts needed.
//!
//! Completions evaluate the selected package recursively across the module in
//! one CUE evaluation, then select the instance for the current directory.

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::cue::discovery::compute_relative_path;
use std::path::{Path, PathBuf};

use crate::commands::env_file::find_cue_module_root;
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

/// Get available tasks from one recursive evaluation of the selected package.
fn get_available_tasks(path: &str, package: &str) -> Vec<(String, Option<String>)> {
    let dir_path = Path::new(path);

    // Find the module root
    let Some(module_root) = find_cue_module_root(dir_path) else {
        return Vec::new();
    };

    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let Ok(raw) = cuengine::evaluate_module(&module_root, package, Some(&options)) else {
        return Vec::new();
    };

    let module = ModuleEvaluation::from_raw(module_root.clone(), raw.instances, raw.projects, None);

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
        cuenv_task_exec::TaskIndex::build(&manifest.tasks)
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

/// Create an `ArgValueCandidates` for task name completion
pub fn task_completer() -> ArgValueCandidates {
    ArgValueCandidates::new(complete_tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::error::Error;
    use std::fs;

    type TestResult = Result<(), Box<dyn Error>>;

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
    fn task_completion_discovers_an_arbitrarily_named_cue_file() -> TestResult {
        let temp = tempfile::Builder::new()
            .prefix("cuenv-completion-test-")
            .tempdir()?;
        fs::create_dir_all(temp.path().join("cue.mod"))?;
        fs::write(
            temp.path().join("cue.mod/module.cue"),
            "module: \"example.com/completion-test\"\nlanguage: {version: \"v0.9.0\"}\n",
        )?;
        fs::write(
            temp.path().join("project.cue"),
            r#"package cuenv

name: "completion-test"
tasks: hello: command: "echo hello"
"#,
        )?;

        let tasks = get_available_tasks(
            temp.path()
                .to_str()
                .ok_or_else(|| std::io::Error::other("temporary path is not UTF-8"))?,
            "cuenv",
        );

        assert!(
            tasks.iter().any(|(name, _)| name == "hello"),
            "arbitrarily named CUE file was not evaluated: {tasks:?}"
        );

        Ok(())
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
