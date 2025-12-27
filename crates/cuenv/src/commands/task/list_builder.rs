//! Unified task list building
//!
//! Provides a single entry point for building task lists with all synthetic
//! tasks properly injected. This ensures consistency across completions,
//! workspace listings, and task execution.

use cuenv_core::Result;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use cuenv_core::tasks::discovery::TaskDiscovery;

use super::workspace::inject_workspace_setup_tasks;

/// Prepare a task index with all synthetic tasks injected.
///
/// This is the single entry point for building task lists. It:
/// 1. Applies implicit tasks (npm, bun workspace detection)
/// 2. Injects workspace setup tasks if discovery is provided
/// 3. Builds the TaskIndex
///
/// # Arguments
///
/// * `manifest` - The project manifest (will be mutated to add implicit/synthetic tasks)
/// * `discovery` - Optional task discovery for cross-project hook resolution
/// * `project_id` - The project ID used for workspace setup injection
///
/// # Errors
///
/// Returns an error if workspace setup task injection fails (e.g., invalid regex
/// in a hook matcher) or if `TaskIndex::build` fails (e.g., cyclic dependencies).
///
/// # Example
///
/// ```ignore
/// let mut manifest = instance.deserialize::<Project>()?;
/// let task_index = prepare_task_index(&mut manifest, Some(&discovery), &project_id)?;
/// ```
pub fn prepare_task_index(
    manifest: &mut Project,
    discovery: Option<&TaskDiscovery>,
    project_id: &str,
) -> Result<TaskIndex> {
    // 1. Apply implicit tasks (npm, bun workspace detection)
    *manifest = manifest.clone().with_implicit_tasks();

    // 2. Inject workspace setup tasks if discovery available
    if let Some(discovery) = discovery {
        inject_workspace_setup_tasks(manifest, discovery, project_id)?;
    }

    // 3. Build TaskIndex
    TaskIndex::build(&manifest.tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::WorkspaceConfig;
    use cuenv_core::tasks::{Task, TaskDefinition};
    use std::collections::HashMap;

    #[test]
    fn test_prepare_task_index_empty_manifest_returns_empty() {
        let mut manifest = Project::default();
        let result = prepare_task_index(&mut manifest, None, "test");
        assert!(result.is_ok());
        let index = result.unwrap();
        // Empty manifest with no workspaces should have no tasks
        assert!(index.list().is_empty());
    }

    #[test]
    fn test_prepare_task_index_preserves_explicit_tasks() {
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

        let result = prepare_task_index(&mut manifest, None, "test");
        assert!(result.is_ok());
        let index = result.unwrap();

        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(task_names.contains(&"build"), "should contain 'build' task");
        assert!(task_names.contains(&"test"), "should contain 'test' task");
        assert_eq!(task_names.len(), 2, "should have exactly 2 tasks");
    }

    #[test]
    fn test_prepare_task_index_injects_workspace_tasks() {
        let mut manifest = Project::default();

        // Configure a bun workspace with an inject task
        let mut inject_tasks = HashMap::new();
        inject_tasks.insert(
            "install".to_string(),
            Task {
                command: "bun install".to_string(),
                description: Some("Install bun dependencies".to_string()),
                ..Default::default()
            },
        );

        let mut workspaces = HashMap::new();
        workspaces.insert(
            "bun".to_string(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: Vec::new(),
                inject: inject_tasks,
            },
        );
        manifest.workspaces = Some(workspaces);

        let result = prepare_task_index(&mut manifest, None, "test");
        assert!(result.is_ok());
        let index = result.unwrap();

        // Should have the injected bun.install task
        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(
            task_names.contains(&"bun.install"),
            "should contain injected 'bun.install' task, got: {:?}",
            task_names
        );
    }

    #[test]
    fn test_prepare_task_index_disabled_workspace_no_inject() {
        let mut manifest = Project::default();

        // Configure a disabled workspace
        let mut inject_tasks = HashMap::new();
        inject_tasks.insert(
            "install".to_string(),
            Task {
                command: "bun install".to_string(),
                ..Default::default()
            },
        );

        let mut workspaces = HashMap::new();
        workspaces.insert(
            "bun".to_string(),
            WorkspaceConfig {
                enabled: false, // Disabled!
                root: None,
                package_manager: None,
                hooks: None,
                commands: Vec::new(),
                inject: inject_tasks,
            },
        );
        manifest.workspaces = Some(workspaces);

        let result = prepare_task_index(&mut manifest, None, "test");
        assert!(result.is_ok());
        let index = result.unwrap();

        // Should NOT have the injected task since workspace is disabled
        let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();
        assert!(
            !task_names.contains(&"bun.install"),
            "disabled workspace should not inject tasks"
        );
    }

    #[test]
    fn test_prepare_task_index_explicit_task_not_overridden() {
        let mut manifest = Project::default();

        // Add explicit bun.install task
        manifest.tasks.insert(
            "bun.install".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo custom install".to_string(),
                description: Some("Custom install".to_string()),
                ..Default::default()
            })),
        );

        // Configure workspace that would also inject bun.install
        let mut inject_tasks = HashMap::new();
        inject_tasks.insert(
            "install".to_string(),
            Task {
                command: "bun install".to_string(),
                description: Some("Injected install".to_string()),
                ..Default::default()
            },
        );

        let mut workspaces = HashMap::new();
        workspaces.insert(
            "bun".to_string(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
                commands: Vec::new(),
                inject: inject_tasks,
            },
        );
        manifest.workspaces = Some(workspaces);

        let result = prepare_task_index(&mut manifest, None, "test");
        assert!(result.is_ok());
        let index = result.unwrap();

        // Find the bun.install task and verify it's the explicit one
        let bun_install = index.resolve("bun.install").unwrap();
        if let TaskDefinition::Single(task) = &bun_install.definition {
            assert_eq!(
                task.command, "echo custom install",
                "explicit task should not be overridden by workspace inject"
            );
        } else {
            panic!("expected single task");
        }
    }
}
