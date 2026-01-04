//! Shared test utilities for cuenv-core tests.
//!
//! This module provides common helper functions for creating test fixtures
//! across different test modules.

use crate::tasks::{Input, Mapping, ProjectReference, Task, TaskDefinition};
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;

/// Supported package manager types for mock workspaces
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Bun,
    Npm,
    Pnpm,
    Yarn,
}

/// Create a test task with dependencies and optional labels
pub fn create_task(name: &str, deps: Vec<&str>, labels: Vec<&str>) -> Task {
    Task {
        command: format!("echo {}", name),
        depends_on: deps.into_iter().map(String::from).collect(),
        description: Some(format!("Test task {}", name)),
        labels: labels.into_iter().map(String::from).collect(),
        ..Default::default()
    }
}

/// Create a task that references another project's task (TaskRef placeholder)
pub fn create_task_ref(ref_str: &str, deps: Vec<&str>) -> Task {
    let mut task = Task::from_task_ref(ref_str);
    task.depends_on = deps.into_iter().map(String::from).collect();
    task
}

/// Create a task with project reference input
pub fn create_task_with_project_ref(
    name: &str,
    deps: Vec<&str>,
    project: &str,
    task: &str,
    mappings: Vec<(&str, &str)>,
) -> Task {
    Task {
        command: format!("echo {}", name),
        depends_on: deps.into_iter().map(String::from).collect(),
        description: Some(format!("Test task {}", name)),
        inputs: vec![Input::Project(ProjectReference {
            project: project.to_string(),
            task: task.to_string(),
            map: mappings
                .into_iter()
                .map(|(from, to)| Mapping {
                    from: from.to_string(),
                    to: to.to_string(),
                })
                .collect(),
        })],
        ..Default::default()
    }
}

/// Create a test hook for onEnter/onExit testing
pub fn create_test_hook(order: i32, command: &str) -> cuenv_hooks::Hook {
    cuenv_hooks::Hook {
        order,
        propagate: false,
        command: command.to_string(),
        args: vec![],
        dir: None,
        inputs: vec![],
        source: None,
    }
}

/// Create a temporary directory with an env.cue file
///
/// Returns a `TempDir` that will be cleaned up when dropped.
/// The directory contains an env.cue file with the provided content.
///
/// # Example
///
/// ```ignore
/// let dir = create_temp_project(r#"
/// package cuenv
/// import "github.com/cuenv/cuenv/schema"
/// schema.#Project
/// name: "test"
/// "#);
/// assert!(dir.path().join("env.cue").exists());
/// ```
#[must_use]
pub fn create_temp_project(cue_content: &str) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    std::fs::write(dir.path().join("env.cue"), cue_content).expect("Failed to write env.cue");
    dir
}

/// Create a mock workspace with package manager lockfile
///
/// Creates a temporary directory containing:
/// - package.json with the project name
/// - The appropriate lockfile for the package manager
///
/// # Example
///
/// ```ignore
/// let dir = create_mock_workspace(PackageManager::Bun);
/// assert!(dir.path().join("bun.lock").exists());
/// assert!(dir.path().join("package.json").exists());
/// ```
#[must_use]
pub fn create_mock_workspace(manager: PackageManager) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("cuenv_workspace_test_")
        .tempdir()
        .expect("Failed to create temp directory");

    // Write package.json
    let package_json = r#"{"name": "test-workspace", "version": "1.0.0", "dependencies": {}}"#;
    std::fs::write(dir.path().join("package.json"), package_json)
        .expect("Failed to write package.json");

    // Write appropriate lockfile
    match manager {
        PackageManager::Bun => {
            std::fs::write(
                dir.path().join("bun.lock"),
                r#"{"lockfileVersion": 1, "workspaces": {"": {"name": "test-workspace"}}}"#,
            )
            .expect("Failed to write bun.lock");
        }
        PackageManager::Npm => {
            std::fs::write(
                dir.path().join("package-lock.json"),
                r#"{"name": "test-workspace", "version": "1.0.0", "lockfileVersion": 3}"#,
            )
            .expect("Failed to write package-lock.json");
        }
        PackageManager::Pnpm => {
            std::fs::write(
                dir.path().join("pnpm-lock.yaml"),
                "lockfileVersion: '9.0'\n",
            )
            .expect("Failed to write pnpm-lock.yaml");
        }
        PackageManager::Yarn => {
            std::fs::write(dir.path().join("yarn.lock"), "# yarn lockfile v1\n")
                .expect("Failed to write yarn.lock");
        }
    }

    dir
}

/// Extract task names from a task map
///
/// Useful for assertions about which tasks exist.
#[must_use]
pub fn task_names(tasks: &HashMap<String, TaskDefinition>) -> Vec<String> {
    let mut names: Vec<_> = tasks.keys().cloned().collect();
    names.sort();
    names
}

/// Check if two task maps have the same task names (ignoring definitions)
#[must_use]
pub fn tasks_have_same_names(
    a: &HashMap<String, TaskDefinition>,
    b: &HashMap<String, TaskDefinition>,
) -> bool {
    task_names(a) == task_names(b)
}

/// Get the dependencies of a task by name
///
/// Returns `None` if the task doesn't exist.
#[must_use]
pub fn get_task_deps(tasks: &HashMap<String, TaskDefinition>, name: &str) -> Option<Vec<String>> {
    tasks.get(name).map(|def| match def {
        TaskDefinition::Single(task) => task.depends_on.clone(),
        TaskDefinition::Group(group) => match group {
            crate::tasks::TaskGroup::Sequential(_) => vec![],
            crate::tasks::TaskGroup::Parallel(pg) => pg.depends_on.clone(),
        },
    })
}

/// Build a simple dependency graph from tasks
///
/// Returns a map of task name -> dependencies.
#[must_use]
pub fn build_dep_graph(tasks: &HashMap<String, TaskDefinition>) -> HashMap<String, Vec<String>> {
    tasks
        .iter()
        .map(|(name, def)| {
            let deps = match def {
                TaskDefinition::Single(task) => task.depends_on.clone(),
                TaskDefinition::Group(group) => match group {
                    crate::tasks::TaskGroup::Sequential(_) => vec![],
                    crate::tasks::TaskGroup::Parallel(pg) => pg.depends_on.clone(),
                },
            };
            (name.clone(), deps)
        })
        .collect()
}

/// Get the workspace root path from env
///
/// Useful for tests that need to locate the project root.
#[must_use]
pub fn get_workspace_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .to_path_buf()
}

/// Get the examples directory path
#[must_use]
pub fn get_examples_dir() -> PathBuf {
    get_workspace_root().join("examples")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_temp_project() {
        let dir = create_temp_project("package test\n");
        assert!(dir.path().join("env.cue").exists());
        let content = std::fs::read_to_string(dir.path().join("env.cue")).unwrap();
        assert_eq!(content, "package test\n");
    }

    #[test]
    fn test_create_mock_workspace_bun() {
        let dir = create_mock_workspace(PackageManager::Bun);
        assert!(dir.path().join("package.json").exists());
        assert!(dir.path().join("bun.lock").exists());
    }

    #[test]
    fn test_create_mock_workspace_npm() {
        let dir = create_mock_workspace(PackageManager::Npm);
        assert!(dir.path().join("package.json").exists());
        assert!(dir.path().join("package-lock.json").exists());
    }

    #[test]
    fn test_create_mock_workspace_pnpm() {
        let dir = create_mock_workspace(PackageManager::Pnpm);
        assert!(dir.path().join("package.json").exists());
        assert!(dir.path().join("pnpm-lock.yaml").exists());
    }

    #[test]
    fn test_create_mock_workspace_yarn() {
        let dir = create_mock_workspace(PackageManager::Yarn);
        assert!(dir.path().join("package.json").exists());
        assert!(dir.path().join("yarn.lock").exists());
    }

    #[test]
    fn test_task_names() {
        let mut tasks = HashMap::new();
        tasks.insert("b".to_string(), TaskDefinition::Single(Box::default()));
        tasks.insert("a".to_string(), TaskDefinition::Single(Box::default()));
        tasks.insert("c".to_string(), TaskDefinition::Single(Box::default()));

        let names = task_names(&tasks);
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_get_task_deps() {
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                depends_on: vec!["setup".to_string()],
                ..Default::default()
            })),
        );

        let deps = get_task_deps(&tasks, "build");
        assert_eq!(deps, Some(vec!["setup".to_string()]));

        let no_deps = get_task_deps(&tasks, "nonexistent");
        assert_eq!(no_deps, None);
    }

    #[test]
    fn test_get_workspace_root() {
        let root = get_workspace_root();
        assert!(root.join("Cargo.toml").exists());
    }

    #[test]
    fn test_get_examples_dir() {
        let examples = get_examples_dir();
        assert!(examples.exists());
        assert!(examples.is_dir());
    }
}
