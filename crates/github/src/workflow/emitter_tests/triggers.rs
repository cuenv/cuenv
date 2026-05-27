use super::*;

#[test]
fn test_workflow_includes_own_path_in_triggers() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);
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

    let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);
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

    let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);
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

    let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);
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

    assert!(result.is_empty());
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
