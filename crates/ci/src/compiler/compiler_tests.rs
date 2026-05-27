use super::*;
use crate::ir::{BuildStage, TaskCondition};
use cuenv_core::ci::{
    ActivationCondition, CI, Contributor, ContributorTask, Pipeline, PipelineCondition,
    PipelineMode, PipelineTask, SecretRef, StringOrVec, TaskCondition as CueTaskCondition, TaskRef,
};
use cuenv_core::tasks::{Input, Task, TaskDependency, TaskNode};
use std::collections::HashMap;

fn test_compiler() -> Compiler {
    Compiler::new(Project::new("test-project"))
}

fn trigger_paths_for_project(project_path: &str, inputs: &[&str]) -> Vec<String> {
    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: inputs
                .iter()
                .map(|input| Input::Path((*input).to_string()))
                .collect(),
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some(project_path.to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    ir.pipeline.trigger.expect("should have trigger").paths
}

/// Helper to create a minimal Contributor for testing
fn test_contributor(id: &str, when: Option<ActivationCondition>) -> Contributor {
    Contributor {
        id: id.to_string(),
        when,
        tasks: vec![],
        auto_associate: None,
    }
}

/// Helper to create a minimal IR for testing
fn test_ir() -> IntermediateRepresentation {
    IntermediateRepresentation {
        version: "1.5".to_string(),
        pipeline: crate::ir::PipelineMetadata {
            name: "test".to_string(),
            mode: PipelineMode::default(),
            environment: None,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: vec![],
        tasks: vec![],
    }
}

#[path = "compiler_tests/basic.rs"]
mod basic;
#[path = "compiler_tests/contributors.rs"]
mod contributors;
#[path = "compiler_tests/dependencies.rs"]
mod dependencies;
#[path = "compiler_tests/paths.rs"]
mod paths;
#[path = "compiler_tests/providers.rs"]
mod providers;
#[path = "compiler_tests/purity.rs"]
mod purity;
#[path = "compiler_tests/triggers.rs"]
mod triggers;
