use super::{Compiler, CompilerOptions, Project, Task, TaskNode};

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

    // Task inputs should be prefixed with project_path. A glob contributes
    // the directory it is rooted in, never the glob itself.
    assert!(trigger.paths.contains(&"projects/api/src/**".to_string()));
    assert!(!trigger.paths.iter().any(|path| path.contains("*.rs")));
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

#[test]
fn test_pipeline_tags_reach_the_trigger() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, StringOrVec};

    let pipeline = Pipeline {
        when: Some(PipelineCondition {
            tag: Some(StringOrVec::Vec(vec!["v*".to_string()])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut project = Project::new("test-project");
    project.ci = Some(CI::default());
    let options = CompilerOptions {
        pipeline_name: Some("release".to_string()),
        pipeline: Some(pipeline),
        ..Default::default()
    };

    let ir = Compiler::with_options(project, options).compile().unwrap();
    let trigger = ir.pipeline.trigger.expect("should have trigger");
    assert_eq!(trigger.tags, vec!["v*".to_string()]);
    assert!(trigger.branches.is_empty());
}

/// A pipeline of one task `build` in the project at `project_path`, reading
/// `inputs`, compiled with `content` declared in `content/`, `site` in
/// `site+web/` and `root` at the module root.
fn compiled_with_inputs(
    project_path: &str,
    inputs: Vec<cuenv_core::tasks::Input>,
) -> Result<crate::ir::IntermediateRepresentation, crate::compiler::CompilerError> {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("app");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "true".to_string(),
            inputs,
            ..Default::default()
        })),
    );
    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            ..Default::default()
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
        file_anchors: Some(crate::compiler::FileAnchors::Known(imported(&[
            ("content", "content"),
            ("site", "site+web"),
            ("root", "."),
        ]))),
        ..Default::default()
    };
    Compiler::with_options(project, options).compile()
}

/// What a project imports: each `(name, directory)` a project of this module
/// that declares its name in its own directory.
fn imported(projects: &[(&str, &str)]) -> cuenv_core::module::ImportedProjects {
    cuenv_core::module::ImportedProjects {
        module: "example.com/site@v0".to_string(),
        package: "cuenv".to_string(),
        records: projects
            .iter()
            .map(|(name, directory)| cuenv_core::module::ImportRecord {
                import_path: format!("example.com/site/{directory}:cuenv"),
                directory: (*directory).to_string(),
                package: "cuenv".to_string(),
                module: "example.com/site@v0".to_string(),
                name: Some((*name).to_string()),
                ..cuenv_core::module::ImportRecord::default()
            })
            .collect(),
    }
}

fn files(anchor: cuenv_core::tasks::FilesAnchor, paths: &[&str]) -> cuenv_core::tasks::Input {
    cuenv_core::tasks::Input::Files(cuenv_core::tasks::FilesReference {
        anchor,
        paths: paths.iter().map(ToString::to_string).collect(),
    })
}

fn project_files(name: &str, paths: &[&str]) -> cuenv_core::tasks::Input {
    files(
        cuenv_core::tasks::FilesAnchor::Project(name.to_string()),
        paths,
    )
}

fn module_files(paths: &[&str]) -> cuenv_core::tasks::Input {
    files(cuenv_core::tasks::FilesAnchor::Module, paths)
}

#[test]
fn file_reference_trigger_paths_start_from_their_anchor() {
    let ir = compiled_with_inputs(
        "services/app",
        vec![
            project_files("content", &["articles/**", "index.md", "*.md"]),
            module_files(&["bun.lock", "services/shared/**", "**/*.proto"]),
        ],
    )
    .unwrap();
    let paths = ir.pipeline.trigger.expect("a trigger").paths;

    for expected in [
        // A glob as its literal directory; a path as itself and below.
        "content/articles/**",
        "content/index.md",
        "content/index.md/**",
        // A first-segment glob falls back to its anchor's directory.
        "content/**",
        "bun.lock",
        "bun.lock/**",
        "services/shared/**",
        // ... which for the module root is everything.
        "**",
    ] {
        assert!(
            paths.contains(&expected.to_string()),
            "{expected}: {paths:?}"
        );
    }
    // Never read from the consumer's directory, and no fallback to the
    // whole project just because there are no plain path inputs.
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with("services/app/articles"))
    );
    assert!(!paths.contains(&"services/app/**".to_string()), "{paths:?}");
}

#[test]
fn a_file_reference_into_a_project_named_with_a_special_character_is_cut_at_it() {
    let ir = compiled_with_inputs("app", vec![project_files("site", &["pages/**"])]).unwrap();
    let paths = ir.pipeline.trigger.expect("a trigger").paths;

    assert!(paths.contains(&"**".to_string()), "{paths:?}");
    assert!(!paths.iter().any(|path| path.contains('+')), "{paths:?}");
}

#[test]
fn a_file_reference_to_an_unknown_project_is_a_compile_error() {
    let error = compiled_with_inputs("app", vec![project_files("elsewhere", &["a/**"])])
        .expect_err("an unknown project");
    let message = error.to_string();

    assert!(
        message.contains("Pipeline 'default' reaches what cannot run"),
        "{message}"
    );
    assert!(message.contains("task 'build'"), "{message}");
    assert!(
        message.contains("project 'elsewhere' is not imported here"),
        "{message}"
    );
}

#[test]
fn a_file_reference_path_the_schema_rejects_is_a_compile_error() {
    for path in ["../shared/**", "/etc/hosts", "./bun.lock", "bun.lock "] {
        let error = compiled_with_inputs("app", vec![module_files(&[path])])
            .expect_err("a path outside its anchor");
        assert!(
            error
                .to_string()
                .contains("is not a path inside the module root"),
            "{path}: {error}"
        );
    }
}

#[test]
fn a_file_reference_need_not_match_a_file_to_compile() {
    // `cuenv sync ci` does not depend on a working tree.
    let ir =
        compiled_with_inputs("app", vec![project_files("content", &["nothing-here/**"])]).unwrap();
    let paths = ir.pipeline.trigger.expect("a trigger").paths;

    assert!(
        paths.contains(&"content/nothing-here/**".to_string()),
        "{paths:?}"
    );
}

#[test]
fn a_problem_recorded_against_a_reached_node_fails_compilation() {
    use cuenv_core::ci::{CI, Pipeline, PipelineTask, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("app");
    for name in ["build", "unrelated"] {
        project.tasks.insert(
            name.to_string(),
            TaskNode::Task(Box::new(Task {
                command: "true".to_string(),
                ..Default::default()
            })),
        );
    }
    project.diagnostics.tasks.insert(
        "unrelated".to_string(),
        vec!["task 'unrelated': broken".to_string()],
    );
    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        ..Default::default()
    };
    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });
    let compiler = |project: &Project| {
        Compiler::with_options(
            project.clone(),
            CompilerOptions {
                pipeline_name: Some("default".to_string()),
                pipeline: Some(pipeline.clone()),
                ..Default::default()
            },
        )
    };

    // A node the pipeline does not reach never fails it.
    assert!(compiler(&project).compile().is_ok());

    project.diagnostics.tasks.insert(
        "build".to_string(),
        vec!["task 'build': dependsOn[0] is foreign".to_string()],
    );
    let error = compiler(&project)
        .compile()
        .expect_err("a reached task's problem");
    assert!(
        error.to_string().contains("dependsOn[0] is foreign"),
        "{error}"
    );

    project.diagnostics.tasks.clear();
    project.diagnostics.pipelines.insert(
        "default".to_string(),
        vec!["pipeline 'default' entry tasks[1] is foreign".to_string()],
    );
    let error = compiler(&project)
        .compile()
        .expect_err("a pipeline problem");
    assert!(
        error.to_string().contains("entry tasks[1] is foreign"),
        "{error}"
    );
}

/// `app` (in `services/app`), whose `build` depends on `#content:build`
/// and reads its outputs, compiled with every project of the module.
fn compiled_with_a_dependency_on_content(
    content: Project,
) -> Result<crate::ir::IntermediateRepresentation, crate::compiler::CompilerError> {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("app");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "true".to_string(),
            depends_on: vec![cuenv_core::tasks::TaskDependency::from_name(
                "#content:build",
            )],
            inputs: vec![cuenv_core::tasks::Input::Task(
                cuenv_core::tasks::TaskOutput {
                    task: "#content:build".to_string(),
                    map: None,
                },
            )],
            ..Default::default()
        })),
    );
    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            ..Default::default()
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
        project_path: Some("services/app".to_string()),
        file_anchors: Some(crate::compiler::FileAnchors::Known(imported(&[(
            "content", "content",
        )]))),
        module_projects: Some(std::sync::Arc::new(BTreeMap::from([
            (std::path::PathBuf::from("content"), content),
            (std::path::PathBuf::from("services/app"), project.clone()),
        ]))),
        ..Default::default()
    };
    Compiler::with_options(project, options).compile()
}

#[test]
fn a_dependency_on_another_projects_task_triggers_on_what_that_task_reads() {
    let mut content = Project::new("content");
    content.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "true".to_string(),
            inputs: vec![
                cuenv_core::tasks::Input::Path("articles/**".to_string()),
                module_files(&["bun.lock"]),
            ],
            outputs: vec!["dist/site.json".to_string()],
            ..Default::default()
        })),
    );

    let ir = compiled_with_a_dependency_on_content(content).unwrap();
    let paths = ir.pipeline.trigger.expect("a trigger").paths;

    for expected in ["content/articles/**", "bun.lock"] {
        assert!(
            paths.contains(&expected.to_string()),
            "{expected}: {paths:?}"
        );
    }
    // `app` reads only `content`'s outputs: it is triggered through
    // `content`, never by every file of its own directory.
    assert!(!paths.contains(&"services/app/**".to_string()), "{paths:?}");
    // The IR keeps the dependency out of its task graph.
    let build = ir
        .tasks
        .iter()
        .find(|task| task.id == "build")
        .expect("build is compiled");
    assert!(build.depends_on.is_empty(), "{:?}", build.depends_on);
    assert_eq!(build.external_dependencies, ["#content:build"]);
}

#[test]
fn a_dependency_on_a_task_without_inputs_triggers_on_its_whole_project() {
    let mut content = Project::new("content");
    content.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "true".to_string(),
            outputs: vec!["dist/site.json".to_string()],
            ..Default::default()
        })),
    );

    let ir = compiled_with_a_dependency_on_content(content).unwrap();
    let paths = ir.pipeline.trigger.expect("a trigger").paths;

    assert!(paths.contains(&"content/**".to_string()), "{paths:?}");
}

#[test]
fn a_dependency_on_a_task_another_project_lacks_is_a_compile_error() {
    let error = compiled_with_a_dependency_on_content(Project::new("content"))
        .expect_err("content has no build");

    assert!(
        error
            .to_string()
            .contains("project 'content' has no task 'build'"),
        "{error}"
    );
}
