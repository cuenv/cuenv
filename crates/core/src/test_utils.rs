//! Shared test utilities for cuenv-core tests.
//!
//! This module provides common helper functions for creating test fixtures
//! across different test modules.

use crate::tasks::{Input, Mapping, ProjectReference, Task};

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

/// Create a task with workspace dependency
pub fn create_workspace_task(name: &str, deps: Vec<&str>, workspaces: Vec<&str>) -> Task {
    Task {
        command: format!("echo {}", name),
        depends_on: deps.into_iter().map(String::from).collect(),
        workspaces: if workspaces.is_empty() {
            None
        } else {
            Some(workspaces.into_iter().map(String::from).collect())
        },
        ..Default::default()
    }
}

/// Create a test hook for onEnter/onExit testing
pub fn create_test_hook(order: i32, command: &str) -> crate::hooks::Hook {
    crate::hooks::Hook {
        order,
        propagate: false,
        command: command.to_string(),
        args: vec![],
        dir: None,
        inputs: vec![],
        source: None,
    }
}
