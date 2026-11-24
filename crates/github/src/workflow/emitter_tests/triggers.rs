use super::{Emitter, GitHubActionsEmitter, TriggerCondition};
#[test]
fn test_workflow_includes_own_path_in_triggers() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        paths: vec!["src/**".to_string(), "Cargo.toml".to_string()],
        pull_request: Some(true),
        ..Default::default()
    });

    let yaml = emitter.emit(&ir).unwrap();

    // Workflow should trigger on its own file path
    assert!(
        yaml.contains(".github/workflows/test-pipeline.yml"),
        "Workflow should include its own path in triggers. Got:\n{yaml}"
    );
}

#[test]
fn test_workflow_path_not_added_when_paths_empty() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        paths: vec![], // Empty paths = no path filtering
        ..Default::default()
    });

    let yaml = emitter.emit(&ir).unwrap();

    // Workflow should NOT add path when there's no path filtering
    assert!(
        !yaml.contains(".github/workflows/test-pipeline.yml"),
        "Workflow should NOT include its own path when no path filtering. Got:\n{yaml}"
    );
}

#[test]
fn test_workflow_path_added_to_both_push_and_pr_triggers() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        paths: vec!["src/**".to_string()],
        pull_request: Some(true),
        ..Default::default()
    });

    let yaml = emitter.emit(&ir).unwrap();

    // Count occurrences of the workflow path (should appear in both push and PR triggers)
    let workflow_path_count = yaml.matches(".github/workflows/test-pipeline.yml").count();
    assert_eq!(
        workflow_path_count, 2,
        "Workflow path should appear in both push and PR triggers. Got:\n{yaml}"
    );
}

#[test]
fn test_workflow_never_emits_paths_ignore() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        paths: vec!["src/**".to_string()],
        pull_request: Some(true),
        ..Default::default()
    });

    let yaml = emitter.emit(&ir).unwrap();

    assert!(
        !yaml.contains("paths-ignore:"),
        "Workflow should only emit positive path filters. Got:\n{yaml}"
    );
}

#[test]
fn test_build_trigger_paths_adds_workflow_path() {
    let paths = vec!["src/**".to_string(), "Cargo.toml".to_string()];

    let result = GitHubActionsEmitter::build_trigger_paths(&paths, "ci.yml");

    assert!(result.contains(&".github/workflows/ci.yml".to_string()));
    assert!(result.contains(&"src/**".to_string()));
    assert!(result.contains(&"Cargo.toml".to_string()));
}

#[test]
fn test_build_trigger_paths_empty_input() {
    let paths: Vec<String> = vec![];

    let result = GitHubActionsEmitter::build_trigger_paths(&paths, "ci.yml");

    assert_eq!(result, [] as [String; 0]);
}

#[test]
fn test_build_trigger_paths_deduplication() {
    let paths = vec![".github/workflows/ci.yml".to_string(), "src/**".to_string()];

    let result = GitHubActionsEmitter::build_trigger_paths(&paths, "ci.yml");

    // Should not duplicate the workflow path
    let count = result
        .iter()
        .filter(|p| *p == ".github/workflows/ci.yml")
        .count();
    assert_eq!(count, 1);
}

#[test]
fn test_build_trigger_paths_sorted() {
    let paths = vec!["z-file".to_string(), "a-file".to_string()];

    let result = GitHubActionsEmitter::build_trigger_paths(&paths, "ci.yml");

    // Result should be sorted
    let mut sorted = result.clone();
    sorted.sort();
    assert_eq!(result, sorted);
}

#[test]
fn push_trigger_includes_tags_without_branches() {
    let emitter = GitHubActionsEmitter::new();
    let mut ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        tags: vec!["v*".to_string()],
        ..Default::default()
    });

    let triggers = emitter.build_triggers(&ir, "ci.yml");
    let push = triggers.push.expect("tag pushes trigger the workflow");
    assert_eq!(push.branches, [] as [String; 0]);
    assert_eq!(push.tags, vec!["v*".to_string()]);
}

#[test]
fn merge_group_trigger_emits_without_paths_and_isolates_concurrency() {
    use cuenv_ci::ir::MergeGroupTriggerIr;

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut ir = super::make_ir(vec![super::make_task("check", &["cargo", "check"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(true),
        merge_group: Some(MergeGroupTriggerIr {
            types: vec!["checks_requested".to_string()],
        }),
        paths: vec!["src/**".to_string()],
        ..Default::default()
    });

    for yaml in [
        emitter.emit_thin(&ir).unwrap(),
        emitter.emit_expanded(&ir).unwrap(),
    ] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["on"]["merge_group"]["types"],
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(
                "checks_requested".to_string()
            )])
        );
        // Derived paths must not be applied to merge_group.
        assert!(doc["on"]["merge_group"].get("paths").is_none());
        // Concurrency group isolates merge_group queue entries.
        assert_eq!(
            doc["concurrency"]["group"].as_str(),
            Some(GitHubActionsEmitter::MERGE_GROUP_CONCURRENCY_GROUP)
        );
    }
}

#[test]
fn workflow_run_trigger_emits_without_paths() {
    use cuenv_ci::ir::WorkflowRunTriggerIr;

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut ir = super::make_ir(vec![super::make_task("deploy", &["deploy"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        workflow_run: Some(WorkflowRunTriggerIr {
            workflows: vec!["CI".to_string()],
            types: vec!["completed".to_string()],
            branches: vec!["main".to_string()],
        }),
        paths: vec!["apps/web/**".to_string()],
        ..Default::default()
    });

    for yaml in [
        emitter.emit_thin(&ir).unwrap(),
        emitter.emit_expanded(&ir).unwrap(),
    ] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["on"]["workflow_run"]["workflows"],
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("CI".to_string())])
        );
        assert_eq!(
            doc["on"]["workflow_run"]["types"],
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("completed".to_string())])
        );
        assert_eq!(
            doc["on"]["workflow_run"]["branches"],
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("main".to_string())])
        );
        assert!(doc["on"]["workflow_run"].get("paths").is_none());
    }
}

#[test]
fn pull_request_detailed_config_controls_types_branches_paths_and_paths_ignore() {
    use cuenv_ci::ir::PullRequestTriggerIr;

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    // 1. types + no branches filter + explicit paths overriding derived paths
    let mut ir = super::make_ir(vec![super::make_task("check", &["check"])]);
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(true),
        pull_request_config: Some(PullRequestTriggerIr {
            types: vec![
                "opened".to_string(),
                "edited".to_string(),
                "synchronize".to_string(),
                "reopened".to_string(),
            ],
            branches: None,
            paths: Some(vec!["tools/kernel/**".to_string()]),
            paths_ignore: Vec::new(),
        }),
        paths: vec!["derived/**".to_string()],
        ..Default::default()
    });

    let doc: serde_yaml::Value =
        serde_yaml::from_str(&emitter.emit_expanded(&ir).unwrap()).unwrap();
    let pr = &doc["on"]["pull_request"];
    assert!(pr.get("branches").is_none());
    assert_eq!(pr["types"].as_sequence().unwrap().len(), 4);
    assert_eq!(
        pr["paths"],
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(
            "tools/kernel/**".to_string()
        )])
    );

    // 2. paths-ignore suppresses derived paths
    let mut ir_ignore = super::make_ir(vec![super::make_task("check", &["check"])]);
    ir_ignore.pipeline.trigger = Some(TriggerCondition {
        pull_request: Some(true),
        pull_request_config: Some(PullRequestTriggerIr {
            types: Vec::new(),
            branches: Some(vec!["main".to_string()]),
            paths: None,
            paths_ignore: vec!["docs/**".to_string()],
        }),
        paths: vec!["derived/**".to_string()],
        ..Default::default()
    });
    let doc_ignore: serde_yaml::Value =
        serde_yaml::from_str(&emitter.emit_expanded(&ir_ignore).unwrap()).unwrap();
    let pr_ignore = &doc_ignore["on"]["pull_request"];
    assert!(pr_ignore.get("paths").is_none());
    assert_eq!(
        pr_ignore["paths-ignore"],
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("docs/**".to_string())])
    );
    assert_eq!(
        pr_ignore["branches"],
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("main".to_string())])
    );
}
