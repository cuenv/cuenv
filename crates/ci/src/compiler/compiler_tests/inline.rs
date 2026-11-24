//! Tasks a pipeline defines inline.

use super::{Compiler, CompilerOptions, Pipeline, PipelineTask, Project, Task, TaskNode};
use crate::error::CompilerError;

fn inline(description: &str) -> PipelineTask {
    PipelineTask::Node(TaskNode::Task(Box::new(Task {
        command: "echo".to_string(),
        description: Some(description.to_string()),
        ..Default::default()
    })))
}

fn compiler(project: Project, entries: Vec<PipelineTask>) -> Compiler {
    Compiler::with_options(
        project,
        CompilerOptions {
            pipeline: Some(Pipeline {
                tasks: entries,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
}

#[test]
fn an_inline_task_compiles_under_its_description() {
    let ir = compiler(Project::new("p"), vec![inline("announce")])
        .compile()
        .unwrap();
    assert!(ir.tasks.iter().any(|task| task.id == "announce"));
    assert_eq!(ir.pipeline.pipeline_tasks, vec!["announce".to_string()]);
}

#[test]
fn an_inline_task_is_found_by_name_but_not_listed_in_the_project() {
    let compiler = compiler(Project::new("p"), vec![inline("announce")]);
    assert!(compiler.find_task("announce").is_some());
    assert!(compiler.project.tasks.is_empty());
}

#[test]
fn an_inline_task_named_like_a_different_project_task_is_an_error() {
    let mut project = Project::new("p");
    project.tasks.insert(
        "announce".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "true".to_string(),
            ..Default::default()
        })),
    );
    let error = compiler(project, vec![inline("announce")])
        .compile()
        .unwrap_err();
    assert!(matches!(error, CompilerError::InlineTaskNameClash(name) if name == "announce"));
}

#[test]
fn two_inline_tasks_with_one_name_are_an_error() {
    let error = compiler(Project::new("p"), vec![inline("a"), inline("a")])
        .compile()
        .unwrap_err();
    assert!(matches!(error, CompilerError::InlineTaskNameClash(_)));
}

#[test]
fn a_project_that_already_holds_the_inline_task_compiles_it_once() {
    let entry = inline("announce");
    let PipelineTask::Node(node) = &entry else {
        unreachable!("inline builds a node");
    };
    let mut project = Project::new("p");
    project.tasks.insert("announce".to_string(), node.clone());
    let ir = compiler(project, vec![entry]).compile().unwrap();
    assert_eq!(ir.tasks.iter().filter(|t| t.id == "announce").count(), 1);
}
