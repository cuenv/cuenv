//! Test fixture helpers for the execution-engine tests.
//!
//! Duplicated from `cuenv-core`'s `#[cfg(test)]` `test_utils` module (which
//! is invisible across crate boundaries); candidates for the shared
//! `crates/test-support` crate planned in RFC-0006 phase 7a.

use crate::{Input, Mapping, ProjectReference, Task, TaskDependency};

/// Create a test task with dependencies and optional labels
pub fn create_task(name: &str, deps: Vec<&str>, labels: Vec<&str>) -> Task {
    Task {
        command: format!("echo {}", name),
        depends_on: deps.into_iter().map(TaskDependency::from_name).collect(),
        description: Some(format!("Test task {}", name)),
        labels: labels.into_iter().map(String::from).collect(),
        ..Default::default()
    }
}

/// Create a task that references another project's task (TaskRef placeholder)
pub fn create_task_ref(ref_str: &str, deps: Vec<&str>) -> Task {
    let mut task = Task::from_task_ref(ref_str);
    task.depends_on = deps.into_iter().map(TaskDependency::from_name).collect();
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
        depends_on: deps.into_iter().map(TaskDependency::from_name).collect(),
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
