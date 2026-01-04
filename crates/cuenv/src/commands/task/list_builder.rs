//! Unified task list building
//!
//! Provides a single entry point for building task lists with all contributor
//! tasks properly injected. This ensures consistency across completions,
//! workspace listings, and task execution.

use std::path::Path;

use cuenv_core::Result;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;

use super::workspace::apply_workspace_contributors;

/// Prepare a task index with all contributor tasks injected.
///
/// This is the single entry point for building task lists. It:
/// 1. Auto-detects workspaces from lockfiles and injects contributor tasks
/// 2. Auto-associates tasks by command (e.g., `bun` tasks depend on contributor setup)
/// 3. Builds the TaskIndex
///
/// Contributor tasks are prefixed with `cuenv:contributor:` namespace.
///
/// # Arguments
///
/// * `manifest` - The project manifest (will be mutated to add contributor tasks)
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
    // 1. Apply workspace contributors (auto-detected from lockfiles)
    apply_workspace_contributors(manifest, project_root);

    // 2. Build TaskIndex
    TaskIndex::build(&manifest.tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::contributors::CONTRIBUTOR_TASK_PREFIX;
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

        // Should have auto-injected contributor tasks
        // Note: TaskIndex normalizes colons to dots, so the canonical name uses dots
        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        // CONTRIBUTOR_TASK_PREFIX uses colons, but TaskIndex normalizes to dots
        let install_task = CONTRIBUTOR_TASK_PREFIX.replace(':', ".") + "bun.workspace.install";
        let setup_task = CONTRIBUTOR_TASK_PREFIX.replace(':', ".") + "bun.workspace.setup";
        assert!(
            task_names.contains(&install_task.as_str()),
            "should contain auto-injected '{}' task, got: {:?}",
            install_task,
            task_names
        );
        assert!(
            task_names.contains(&setup_task.as_str()),
            "should contain auto-injected '{}' task, got: {:?}",
            setup_task,
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
        // TaskIndex normalizes colons to dots
        let install_task = CONTRIBUTOR_TASK_PREFIX.replace(':', ".") + "bun.workspace.install";
        assert!(
            !task_names.contains(&install_task.as_str()),
            "no lockfile should mean no injected tasks"
        );
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

        // The 'dev' task should now depend on the bun workspace setup task
        // TaskIndex normalizes colons to dots in dependencies too
        let dev_task = index.resolve("dev").unwrap();
        let expected_dep = CONTRIBUTOR_TASK_PREFIX.replace(':', ".") + "bun.workspace.setup";
        if let TaskDefinition::Single(task) = &dev_task.definition {
            assert!(
                task.depends_on.contains(&expected_dep),
                "bun task should auto-depend on {}, got: {:?}",
                expected_dep,
                task.depends_on
            );
        } else {
            panic!("expected single task");
        }
    }
}
