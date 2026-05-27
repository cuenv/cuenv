use super::*;

#[test]
fn test_derive_trigger_paths_with_project_path() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![
                cuenv_core::tasks::Input::Path("src/**/*.rs".to_string()),
                cuenv_core::tasks::Input::Path("Cargo.toml".to_string()),
            ],
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

    // Add CI config with a pipeline
    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("projects/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Task inputs should be prefixed with project_path
    assert!(
        trigger
            .paths
            .contains(&"projects/api/src/**/*.rs".to_string())
    );
    assert!(
        trigger
            .paths
            .contains(&"projects/api/Cargo.toml".to_string())
    );

    // CUE implicit paths should also be prefixed
    assert!(trigger.paths.contains(&"projects/api/env.cue".to_string()));
    assert!(
        trigger
            .paths
            .contains(&"projects/api/schema/**".to_string())
    );

    // cue.mod should NOT be prefixed (it's at module root)
    assert!(trigger.paths.contains(&"cue.mod/**".to_string()));
}

#[test]
fn test_derive_trigger_paths_fallback_to_project_dir() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    // Task with NO inputs
    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("deploy"))],
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
        project_path: Some("projects/rawkode.academy/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // When no task inputs, should fallback to project directory
    assert!(
        trigger
            .paths
            .contains(&"projects/rawkode.academy/api/**".to_string()),
        "Should contain fallback path. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_trigger_paths_root_project() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![cuenv_core::tasks::Input::Path("src/**".to_string())],
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

    // No project_path = root project
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should NOT be prefixed for root projects
    assert!(trigger.paths.contains(&"src/**".to_string()));
    assert!(trigger.paths.contains(&"env.cue".to_string()));
    assert!(trigger.paths.contains(&"schema/**".to_string()));
}

#[test]
fn test_derive_trigger_paths_root_project_no_inputs_fallback() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    // Task with NO inputs
    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("deploy"))],
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

    // No project_path = root project
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Root project with no inputs should fallback to **
    assert!(
        trigger.paths.contains(&"**".to_string()),
        "Root project with no inputs should fallback to **. Paths: {:?}",
        trigger.paths
    );
}
