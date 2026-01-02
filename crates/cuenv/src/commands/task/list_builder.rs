//! Unified task list building
//!
//! Provides a single entry point for building task lists with all synthetic
//! tasks properly injected. This ensures consistency across completions,
//! workspace listings, and task execution.

use std::path::Path;

use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use cuenv_core::Result;

use super::workspace::inject_detected_workspace_tasks;

/// Prepare a task index with all synthetic tasks injected.
///
/// This is the single entry point for building task lists. It:
/// 1. Auto-detects workspaces from lockfiles and injects setup tasks
/// 2. Auto-associates tasks by command (e.g., `bun` tasks depend on `bun.setup`)
/// 3. Builds the TaskIndex
///
/// # Arguments
///
/// * `manifest` - The project manifest (will be mutated to add synthetic tasks)
/// * `project_root` - Path to the project directory (for lockfile detection)
///
/// # Errors
///
/// Returns an error if `TaskIndex::build` fails (e.g., cyclic dependencies).
///
/// # Example
///
/// ```ignore
/// let mut manifest = instance.deserialize::<Project>()?;
/// let task_index = prepare_task_index(&mut manifest, project_root)?;
/// ```
pub fn prepare_task_index(manifest: &mut Project, project_root: &Path) -> Result<TaskIndex> {
    // 1. Inject workspace setup tasks (auto-detected from lockfiles)
    inject_detected_workspace_tasks(manifest, project_root);

    // 2. Build TaskIndex
    TaskIndex::build(&manifest.tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::{Task, TaskDefinition};
    use std::fs;
    use tempfile::TempDir;

    fn create_test_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("cuenv_list_builder_test_")
            .tempdir()
            .expect("Failed to create temp directory")
    }

    #[test]
    fn test_prepare_task_index_empty_manifest_no_lockfile() {
        let tmp = create_test_dir();
        let mut manifest = Project::default();
        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();
        // Empty manifest with no lockfiles should have no tasks
        assert!(index.list().is_empty());
    }

    #[test]
    fn test_prepare_task_index_preserves_explicit_tasks() {
        let tmp = create_test_dir();
        let mut manifest = Project::default();
        manifest.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo build".to_string(),
                description: Some("Build the project".to_string()),
                ..Default::default()
            })),
        );
        manifest.tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo test".to_string(),
                ..Default::default()
            })),
        );

        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();

        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(task_names.contains(&"build"), "should contain 'build' task");
        assert!(task_names.contains(&"test"), "should contain 'test' task");
        assert_eq!(task_names.len(), 2, "should have exactly 2 tasks");
    }

    #[test]
    fn test_prepare_task_index_injects_bun_tasks_from_lockfile() {
        let tmp = create_test_dir();
        // Create bun.lock to trigger bun workspace detection
        fs::write(tmp.path().join("bun.lock"), "lockfile content").unwrap();

        let mut manifest = Project::default();

        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();

        // Should have auto-injected bun.install and bun.setup tasks
        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(
            task_names.contains(&"bun.install"),
            "should contain auto-injected 'bun.install' task, got: {:?}",
            task_names
        );
        assert!(
            task_names.contains(&"bun.setup"),
            "should contain auto-injected 'bun.setup' task, got: {:?}",
            task_names
        );
    }

    #[test]
    fn test_prepare_task_index_no_lockfile_no_inject() {
        let tmp = create_test_dir();
        // No lockfile - should not inject any workspace tasks

        let mut manifest = Project::default();

        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();

        // Should NOT have any injected tasks
        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(
            !task_names.contains(&"bun.install"),
            "no lockfile should mean no injected tasks"
        );
    }

    #[test]
    fn test_prepare_task_index_explicit_task_not_overridden() {
        let tmp = create_test_dir();
        // Create bun.lock to trigger detection
        fs::write(tmp.path().join("bun.lock"), "lockfile content").unwrap();

        let mut manifest = Project::default();

        // Add explicit bun.install task with custom command
        manifest.tasks.insert(
            "bun.install".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo custom install".to_string(),
                description: Some("Custom install".to_string()),
                ..Default::default()
            })),
        );

        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();

        // Find the bun.install task and verify it's the explicit one (not overridden)
        let bun_install = index.resolve("bun.install").unwrap();
        if let TaskDefinition::Single(task) = &bun_install.definition {
            assert_eq!(
                task.command, "echo custom install",
                "explicit task should not be overridden by auto-detection"
            );
        } else {
            panic!("expected single task");
        }
    }

    #[test]
    fn test_prepare_task_index_auto_associates_bun_tasks() {
        let tmp = create_test_dir();
        // Create bun.lock to trigger detection
        fs::write(tmp.path().join("bun.lock"), "lockfile content").unwrap();

        let mut manifest = Project::default();
        manifest.tasks.insert(
            "dev".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string(), "dev".to_string()],
                ..Default::default()
            })),
        );

        let result = prepare_task_index(&mut manifest, tmp.path());
        assert!(result.is_ok());
        let index = result.unwrap();

        // The 'dev' task should now depend on bun.setup
        let dev_task = index.resolve("dev").unwrap();
        if let TaskDefinition::Single(task) = &dev_task.definition {
            assert!(
                task.depends_on.contains(&"bun.setup".to_string()),
                "bun task should auto-depend on bun.setup, got: {:?}",
                task.depends_on
            );
        } else {
            panic!("expected single task");
        }
    }
}
