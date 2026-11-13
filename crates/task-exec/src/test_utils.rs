//! Test fixture helpers for the execution-engine tests.
//!
//! Duplicated from `cuenv-core`'s `#[cfg(test)]` `test_utils` module (which
//! is invisible across crate boundaries); candidates for the shared
//! `crates/test-support` crate planned in RFC-0006 phase 7a.

use crate::{Task, TaskDependency};

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
