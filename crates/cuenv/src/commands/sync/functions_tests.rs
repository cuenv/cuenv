use super::*;
use cuenv_core::ci::{MatrixTask, PipelineTask, TaskRef};
use std::collections::BTreeMap;

#[test]
fn test_has_matrix_tasks_empty() {
    let tasks: Vec<PipelineTask> = vec![];
    assert!(!has_matrix_tasks(&tasks));
}

#[test]
fn test_has_matrix_tasks_simple_only() {
    let tasks = vec![
        PipelineTask::Simple(TaskRef::from_name("build")),
        PipelineTask::Simple(TaskRef::from_name("test")),
    ];
    assert!(!has_matrix_tasks(&tasks));
}

#[test]
fn test_has_matrix_tasks_with_matrix() {
    let mut matrix = BTreeMap::new();
    matrix.insert(
        "arch".to_string(),
        vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
    );

    let tasks = vec![PipelineTask::Matrix(MatrixTask {
        task_type: Some("matrix".to_string()),
        task: TaskRef::from_name("cargo.build"),
        matrix,
        artifacts: None,
        params: None,
    })];
    assert!(has_matrix_tasks(&tasks));
}

#[test]
fn test_has_matrix_tasks_aggregation_only() {
    // Aggregation task has empty matrix but artifacts
    let tasks = vec![PipelineTask::Matrix(MatrixTask {
        task_type: Some("matrix".to_string()),
        task: TaskRef::from_name("publish"),
        matrix: BTreeMap::new(),
        artifacts: Some(vec![]),
        params: None,
    })];
    // Aggregation tasks are NOT matrix tasks (they don't have matrix dimensions)
    assert!(!has_matrix_tasks(&tasks));
}

#[test]
fn test_has_matrix_tasks_mixed() {
    let mut matrix = BTreeMap::new();
    matrix.insert("arch".to_string(), vec!["linux-x64".to_string()]);

    let tasks = vec![
        PipelineTask::Simple(TaskRef::from_name("check")),
        PipelineTask::Matrix(MatrixTask {
            task_type: Some("matrix".to_string()),
            task: TaskRef::from_name("build"),
            matrix,
            artifacts: None,
            params: None,
        }),
        PipelineTask::Simple(TaskRef::from_name("deploy")),
    ];
    assert!(has_matrix_tasks(&tasks));
}
