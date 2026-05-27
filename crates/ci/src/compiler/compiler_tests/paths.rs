use super::*;

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
    let paths = trigger_paths_for_project(
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
    let paths = trigger_paths_for_project("apps/server", &["../../flake.nix", "../shared/**"]);

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
    let paths = trigger_paths_for_project("server", &["../../outside/**"]);

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
    let paths = trigger_paths_for_project("server", &["src", "../flake.nix"]);

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
fn test_derive_paths_does_not_expand_existing_globs() {
    // Inputs that already contain glob metacharacters must not get a
    // duplicate `/**` appended; their meaning is left to GitHub Actions.
    let paths = trigger_paths_for_project("server", &["src/**/*.rs", "data/?.json"]);

    assert!(
        paths.contains(&"server/src/**/*.rs".to_string()),
        "glob input should be emitted as-is: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "server/src/**/*.rs/**"),
        "glob input should not have /** appended: {paths:?}"
    );
    assert!(
        paths.contains(&"server/data/?.json".to_string()),
        "wildcard input should be emitted as-is: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "server/data/?.json/**"),
        "wildcard input should not have /** appended: {paths:?}"
    );
}
