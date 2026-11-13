use super::{
    BTreeMap, CI, Compiler, CompilerOptions, HashMap, Input, Pipeline, PipelineCondition,
    PipelineTask, Project, StringOrVec, Task, TaskGroup, TaskNode, TaskRef,
};
#[test]
fn test_derive_paths_from_task_group() {
    // Create a task group (like "check" with nested tasks "lint", "test", etc.)
    let mut project = Project::new("test-project");

    let mut group_tasks = HashMap::new();
    group_tasks.insert(
        "lint".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["clippy".to_string()],
            inputs: vec![
                Input::Path("Cargo.toml".to_string()),
                Input::Path("crates/**".to_string()),
            ],
            ..Default::default()
        })),
    );
    group_tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            inputs: vec![
                Input::Path("Cargo.toml".to_string()),
                Input::Path("crates/**".to_string()),
                Input::Path("tests/**".to_string()),
            ],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "check".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: group_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("check"))],
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

    // Root project (no project_path prefix)
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Should collect inputs from all nested tasks in the group
    assert!(
        trigger.paths.contains(&"Cargo.toml".to_string()),
        "Should contain Cargo.toml from group tasks. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"crates/**".to_string()),
        "Should contain crates/** from group tasks. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"tests/**".to_string()),
        "Should contain tests/** from group tasks. Paths: {:?}",
        trigger.paths
    );
    // Should NOT fallback to ** since we have inputs
    assert!(
        !trigger.paths.contains(&"**".to_string()),
        "Should not fallback to ** when task group has inputs. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_root_project_no_dot_prefix() {
    // When project_path is "." (root), paths should not have "./" prefix
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![Input::Path("src/**".to_string())],
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

    // project_path = "." (root project, as set by sync command)
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some(".".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should NOT have "./" prefix - GitHub Actions doesn't handle it correctly
    assert!(
        trigger.paths.contains(&"src/**".to_string()),
        "Should contain src/** without ./ prefix. Paths: {:?}",
        trigger.paths
    );
    assert!(
        !trigger.paths.iter().any(|p| p.starts_with("./")),
        "No path should have ./ prefix. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"env.cue".to_string()),
        "Should contain env.cue without ./ prefix. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_subproject_has_prefix() {
    // When project_path is "projects/api", paths should be prefixed
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![Input::Path("src/**".to_string())],
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

    // Subproject path
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("projects/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should have the project prefix
    assert!(
        trigger.paths.contains(&"projects/api/src/**".to_string()),
        "Should contain prefixed path. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"projects/api/env.cue".to_string()),
        "Should contain prefixed env.cue. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_nested_project_normalizes_parent_inputs() {
    let paths = super::trigger_paths_for_project(
        "server",
        &[
            "../flake.nix",
            "../infrastructure/waddle.cloud/gitops/waddle-server/**",
            "src/**",
        ],
    );

    assert!(paths.contains(&"flake.nix".to_string()), "Paths: {paths:?}");
    assert!(
        paths.contains(&"infrastructure/waddle.cloud/gitops/waddle-server/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/src/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/env.cue".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/schema/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("../")),
        "Paths should not contain parent traversal: {paths:?}"
    );
}

#[test]
fn test_derive_paths_deep_nested_project_normalizes_parent_inputs() {
    let paths =
        super::trigger_paths_for_project("apps/server", &["../../flake.nix", "../shared/**"]);

    assert!(paths.contains(&"flake.nix".to_string()), "Paths: {paths:?}");
    assert!(
        paths.contains(&"apps/shared/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"apps/server/env.cue".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("../")),
        "Paths should not contain parent traversal: {paths:?}"
    );
}

#[test]
fn test_derive_paths_skips_inputs_that_escape_repo_root() {
    let paths = super::trigger_paths_for_project("server", &["../../outside/**"]);

    assert!(
        !paths.iter().any(|path| path.contains("outside")),
        "Escaping paths should be skipped: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("..")),
        "Paths should not contain parent traversal: {paths:?}"
    );
    assert!(
        !paths.contains(&"server/**".to_string()),
        "Escaping task input should not trigger project fallback: {paths:?}"
    );
}

#[test]
fn test_derive_paths_emits_recursive_glob_for_simple_directory_inputs() {
    // Inputs without glob metacharacters can refer to files or directories.
    // GitHub Actions path filters do not treat a bare `server/src` as
    // matching files beneath it, so we emit `<path>/**` alongside the
    // literal path. This keeps derived filters aligned with
    // `cuenv_core::affected::matches_pattern`, which treats non-glob
    // patterns as prefixes.
    let paths = super::trigger_paths_for_project("server", &["src", "../flake.nix"]);

    assert!(
        paths.contains(&"server/src".to_string()),
        "literal path should still be emitted: {paths:?}"
    );
    assert!(
        paths.contains(&"server/src/**".to_string()),
        "directory input should also emit recursive glob: {paths:?}"
    );
    assert!(
        paths.contains(&"flake.nix".to_string()),
        "literal file from parent should be emitted: {paths:?}"
    );
    assert!(
        paths.contains(&"flake.nix/**".to_string()),
        "simple parent path should also emit recursive glob (covers \
             the case where it turns out to be a directory): {paths:?}"
    );
}

#[test]
fn a_glob_contributes_the_directory_it_is_rooted_in() {
    // GitHub's filter syntax is not the glob dialect inputs are matched with
    // at runtime: `*` stops at `/` there, and `?`, `+`, `[]` and a leading `!`
    // mean something else. No glob is translated; the workflow starts for the
    // glob's literal directory and `cuenv ci` selects precisely.
    let paths = super::trigger_paths_for_project(
        "server",
        &["src/**/*.rs", "data/?.json", "gen/*.txt", "conf/{a,b}.yaml"],
    );

    for expected in [
        "server/src/**",
        "server/data/**",
        "server/gen/**",
        "server/conf/**",
    ] {
        assert!(
            paths.contains(&expected.to_string()),
            "{expected}: {paths:?}"
        );
    }
    assert!(
        !paths
            .iter()
            .any(|path| path.contains(['?', '{', '[']) || path.contains("*.")),
        "no glob is emitted: {paths:?}"
    );
}

#[test]
fn a_glob_with_no_literal_directory_falls_back_to_the_project() {
    assert!(
        super::trigger_paths_for_project("server", &["**/*.proto"])
            .contains(&"server/**".to_string())
    );
    assert!(super::trigger_paths_for_project(".", &["*.md"]).contains(&"**".to_string()));
    for whole_project in ["", ".", "./"] {
        assert!(
            super::trigger_paths_for_project("server", &[whole_project])
                .contains(&"server/**".to_string()),
            "{whole_project:?}"
        );
    }
}

#[test]
fn an_input_is_read_the_same_however_it_is_spelled() {
    let plain = super::trigger_paths_for_project("server", &["src/lib", "bun.lock", "gen/**"]);
    let spelled =
        super::trigger_paths_for_project("server", &["./src//lib/", " bun.lock ", "./gen/./**"]);
    assert_eq!(plain, spelled);
    assert!(plain.contains(&"server/bun.lock".to_string()), "{plain:?}");
}

#[test]
fn a_segment_github_would_read_as_a_pattern_cuts_the_path_at_its_parent() {
    let paths = super::trigger_paths_for_project(
        "web",
        &[
            "src/routes/+page.svelte",
            "native/c++/main.cc",
            "notes/!important.md",
        ],
    );

    for expected in ["web/src/routes/**", "web/native/**", "web/notes/**"] {
        assert!(
            paths.contains(&expected.to_string()),
            "{expected}: {paths:?}"
        );
    }
    assert!(
        !paths.iter().any(|path| path.contains(['+', '!'])),
        "{paths:?}"
    );

    // The project's own directory is cut the same way.
    let paths = super::trigger_paths_for_project("apps/c++", &["src/**"]);
    assert!(paths.contains(&"apps/**".to_string()), "{paths:?}");
}

/// Whether one of GitHub's `paths` filters starts a workflow for `file`,
/// for the two shapes derivation emits: an exact path and `<dir>/**`.
fn starts_a_workflow(paths: &[String], file: &str) -> bool {
    paths.iter().any(|path| {
        path == "**"
            || path == file
            || path
                .strip_suffix("/**")
                .is_some_and(|directory| file.starts_with(&format!("{directory}/")))
    })
}

#[test]
fn every_file_that_affects_a_task_at_runtime_starts_its_workflow() {
    // Parity between `cuenv ci`, which matches inputs as globs, and the
    // derived filters. A file the runtime treats as affecting and that no
    // filter covers is a pipeline that silently never runs.
    let files = [
        "app/bun.lock",
        "app/src/a.txt",
        "app/src/b.txt",
        "app/src/deep/x.txt",
        "app/src/deep/deeper/y.ts",
        "app/gen/one.json",
        "app/conf/a.yaml",
        "app/README.md",
        "app/src/routes/+page.svelte",
    ];
    let inputs = [
        "bun.lock",
        "./bun.lock",
        "src",
        "src/",
        "src/*.txt",
        "src/**",
        "src/**/*.ts",
        "src/{a,b}.txt",
        "src/deep/?.txt",
        "src/[ab].txt",
        "gen/*.json",
        "conf/{a,b}.yaml",
        "*.md",
        "**/*.md",
        "",
        ".",
        " src/a.txt ",
        "src/./deep",
        "src//deep/x.txt",
        "src/routes/+page.svelte",
    ];

    for input in inputs {
        let paths = super::trigger_paths_for_project("app", &[input]);
        for file in files {
            let affects = cuenv_core::matches_pattern(
                &[std::path::PathBuf::from(file)],
                std::path::Path::new("app"),
                input,
            );
            assert!(
                !affects || starts_a_workflow(&paths, file),
                "input {input:?} is affected by {file} at runtime, and no derived path \
                 starts the workflow: {paths:?}"
            );
        }
    }
}

fn pipeline_paths(project: Project, pipeline_tasks: &[&str]) -> Vec<String> {
    let pipeline = Pipeline {
        tasks: pipeline_tasks
            .iter()
            .map(|name| PipelineTask::Simple(TaskRef::from_name(*name)))
            .collect(),
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
    let mut project = project;
    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("app".to_string()),
        ..Default::default()
    };
    let ir = Compiler::with_options(project, options).compile().unwrap();
    ir.pipeline.trigger.expect("should have trigger").paths
}

fn task_with(inputs: &[&str], depends_on: &[&str]) -> TaskNode {
    TaskNode::Task(Box::new(Task {
        command: "true".to_string(),
        inputs: inputs
            .iter()
            .map(|input| Input::Path((*input).to_string()))
            .collect(),
        depends_on: depends_on
            .iter()
            .map(|name| cuenv_core::tasks::TaskDependency::from_name(*name))
            .collect(),
        ..Default::default()
    }))
}

#[test]
fn a_reached_task_without_inputs_makes_the_whole_project_a_trigger() {
    // `build` has inputs and depends on `gen`, which has none. `gen` always
    // runs, so `cuenv ci` runs `build` for any change: a README change has
    // to start the workflow.
    let mut project = Project::new("app");
    project
        .tasks
        .insert("build".to_string(), task_with(&["src/**"], &["gen"]));
    project.tasks.insert("gen".to_string(), task_with(&[], &[]));

    let paths = pipeline_paths(project, &["build"]);

    assert!(paths.contains(&"app/**".to_string()), "{paths:?}");
    assert!(starts_a_workflow(&paths, "app/README.md"));
}

#[test]
fn a_pipeline_whose_tasks_all_have_inputs_does_not_trigger_on_the_whole_project() {
    let mut project = Project::new("app");
    project
        .tasks
        .insert("build".to_string(), task_with(&["src/**"], &["gen"]));
    project
        .tasks
        .insert("gen".to_string(), task_with(&["schema/**"], &[]));

    let paths = pipeline_paths(project, &["build"]);

    assert!(!paths.contains(&"app/**".to_string()), "{paths:?}");
    assert!(paths.contains(&"app/src/**".to_string()), "{paths:?}");
    assert!(paths.contains(&"app/schema/**".to_string()), "{paths:?}");
}

#[test]
fn a_dependency_declared_on_a_group_is_followed() {
    let mut project = Project::new("app");
    project.tasks.insert(
        "check".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: HashMap::from([("lint".to_string(), task_with(&["src/**"], &[]))]),
            depends_on: vec![cuenv_core::tasks::TaskDependency::from_name("gen")],
            description: None,
            max_concurrency: None,
        }),
    );
    project
        .tasks
        .insert("gen".to_string(), task_with(&["schema/**"], &[]));

    let paths = pipeline_paths(project, &["check"]);

    assert!(paths.contains(&"app/schema/**".to_string()), "{paths:?}");
}
