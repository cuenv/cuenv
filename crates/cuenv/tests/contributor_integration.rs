//! Integration tests for workspace contributor DAG injection
//!
//! These tests load real fixtures via CUE evaluation and verify that
//! workspace contributors properly inject tasks and wire dependencies.
//!
//! Run with: cargo test --test contributor_integration

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::expect_used)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::contributors::{
    CONTRIBUTOR_TASK_PREFIX, ContributorContext, ContributorEngine, builtin_workspace_contributors,
};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use std::path::{Path, PathBuf};

/// Get the path to the contributor-tests examples directory
fn get_contributor_tests_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .join("examples")
        .join("contributor-tests")
}

/// Load a Project manifest from an example directory.
fn load_manifest(example_path: &Path) -> Result<Project, String> {
    evaluate_cue_package_typed::<Project>(example_path, "cuenv").map_err(|e| {
        format!(
            "Failed to load manifest from {}: {e}",
            example_path.display()
        )
    })
}

/// Check if the FFI/module evaluation is available for these tests.
fn ffi_available() -> bool {
    let test_dir = get_contributor_tests_dir();
    let test_path = test_dir.join("bun-workspace");
    load_manifest(&test_path).is_ok()
}

/// Skip test with message if FFI is unavailable
macro_rules! skip_if_ffi_unavailable {
    () => {
        if !ffi_available() {
            eprintln!(
                "Skipping test: FFI/module evaluation unavailable (examples need cue.mod root)"
            );
            return;
        }
    };
}

/// Apply workspace contributors to a manifest and return the task index
fn apply_contributors(manifest: &mut Project, project_root: &Path) -> TaskIndex {
    let context = ContributorContext::detect(project_root).with_task_commands(&manifest.tasks);
    let contributors = builtin_workspace_contributors();
    let engine = ContributorEngine::new(&contributors, context);
    engine
        .apply(&mut manifest.tasks)
        .expect("Failed to apply contributors");
    TaskIndex::build(&manifest.tasks).expect("Failed to build task index")
}

/// Get the canonical (dotted) form of a contributor task name
fn canonical_contributor_task(suffix: &str) -> String {
    // CONTRIBUTOR_TASK_PREFIX uses colons, TaskIndex normalizes to dots
    format!("{}{}", CONTRIBUTOR_TASK_PREFIX.replace(':', "."), suffix)
}

// ============================================================================
// Bun Workspace Contributor Tests
// ============================================================================

#[test]
fn test_bun_contributor_injects_install_task() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("bun-workspace");
    let mut project = load_manifest(&project_path).expect("Failed to load bun-workspace");

    let index = apply_contributors(&mut project, &project_path);

    // Verify bun.workspace.install task was injected
    let install_task = canonical_contributor_task("bun.workspace.install");
    let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();

    assert!(
        task_names.contains(&install_task.as_str()),
        "bun.workspace.install should be injected when bun.lock exists, got: {:?}",
        task_names
    );
}

#[test]
fn test_bun_contributor_injects_setup_task() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("bun-workspace");
    let mut project = load_manifest(&project_path).expect("Failed to load bun-workspace");

    let index = apply_contributors(&mut project, &project_path);

    // Verify bun.workspace.setup task was injected
    let setup_task = canonical_contributor_task("bun.workspace.setup");
    let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();

    assert!(
        task_names.contains(&setup_task.as_str()),
        "bun.workspace.setup should be injected when bun.lock exists, got: {:?}",
        task_names
    );
}

// ============================================================================
// NPM Workspace Contributor Tests
// ============================================================================

#[test]
fn test_npm_contributor_injects_install_task() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("npm-workspace");
    let mut project = load_manifest(&project_path).expect("Failed to load npm-workspace");

    let index = apply_contributors(&mut project, &project_path);

    // Verify npm.workspace.install task was injected
    let install_task = canonical_contributor_task("npm.workspace.install");
    let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();

    assert!(
        task_names.contains(&install_task.as_str()),
        "npm.workspace.install should be injected when package-lock.json exists, got: {:?}",
        task_names
    );
}

// ============================================================================
// Auto-Association Tests
// ============================================================================

#[test]
fn test_auto_association_adds_depends_on_for_bun_command() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("auto-associate");
    let mut project = load_manifest(&project_path).expect("Failed to load auto-associate");

    let index = apply_contributors(&mut project, &project_path);

    // The 'dev' task uses bun command, should depend on bun.workspace.setup
    let dev_task = index.resolve("dev").expect("dev task should exist");
    let expected_dep = canonical_contributor_task("bun.workspace.setup");

    if let cuenv_core::tasks::TaskDefinition::Single(task) = &dev_task.definition {
        assert!(
            task.resolved_deps.contains(&expected_dep),
            "dev task should auto-depend on {}, got: {:?}",
            expected_dep,
            task.resolved_deps
        );
    } else {
        panic!("expected single task definition for 'dev'");
    }
}

#[test]
fn test_auto_association_does_not_affect_non_bun_tasks() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("auto-associate");
    let mut project = load_manifest(&project_path).expect("Failed to load auto-associate");

    let index = apply_contributors(&mut project, &project_path);

    // The 'lint' task uses echo command, should NOT depend on bun.workspace.setup
    let lint_task = index.resolve("lint").expect("lint task should exist");
    let bun_setup = canonical_contributor_task("bun.workspace.setup");

    if let cuenv_core::tasks::TaskDefinition::Single(task) = &lint_task.definition {
        assert!(
            !task.resolved_deps.contains(&bun_setup),
            "lint task should NOT auto-depend on bun setup (it doesn't use bun), got: {:?}",
            task.resolved_deps
        );
    } else {
        panic!("expected single task definition for 'lint'");
    }
}

// ============================================================================
// No Contributors Tests
// ============================================================================

#[test]
fn test_no_lockfile_no_injection() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("no-contributors");
    let mut project = load_manifest(&project_path).expect("Failed to load no-contributors");

    let index = apply_contributors(&mut project, &project_path);

    // No lockfiles = no workspace contributor tasks should be injected
    let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();

    // Should only have the original tasks (build, test)
    assert_eq!(
        task_names.len(),
        2,
        "without lockfiles, only original tasks should exist, got: {:?}",
        task_names
    );
    assert!(task_names.contains(&"build"));
    assert!(task_names.contains(&"test"));
}

// ============================================================================
// Idempotency Tests
// ============================================================================

#[test]
fn test_idempotent_injection() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("bun-workspace");
    let mut project = load_manifest(&project_path).expect("Failed to load bun-workspace");

    // Apply contributors once
    let context1 = ContributorContext::detect(&project_path).with_task_commands(&project.tasks);
    let contributors = builtin_workspace_contributors();
    let engine1 = ContributorEngine::new(&contributors, context1);
    engine1
        .apply(&mut project.tasks)
        .expect("First apply failed");
    let first_index = TaskIndex::build(&project.tasks).expect("First index build failed");
    let first_tasks: Vec<_> = first_index.list().iter().map(|t| t.name.clone()).collect();

    // Apply contributors again (should be idempotent)
    let context2 = ContributorContext::detect(&project_path).with_task_commands(&project.tasks);
    let engine2 = ContributorEngine::new(&contributors, context2);
    engine2
        .apply(&mut project.tasks)
        .expect("Second apply failed");
    let second_index = TaskIndex::build(&project.tasks).expect("Second index build failed");
    let second_tasks: Vec<_> = second_index.list().iter().map(|t| t.name.clone()).collect();

    // Task lists should be identical
    assert_eq!(
        first_tasks, second_tasks,
        "Applying contributors twice should produce the same task list"
    );
}

// ============================================================================
// Task Naming Convention Tests
// ============================================================================

#[test]
fn test_contributor_task_naming_convention() {
    skip_if_ffi_unavailable!();

    let test_dir = get_contributor_tests_dir();
    let project_path = test_dir.join("bun-workspace");
    let mut project = load_manifest(&project_path).expect("Failed to load bun-workspace");

    let index = apply_contributors(&mut project, &project_path);

    // All injected contributor tasks should have the cuenv.contributor. prefix
    let canonical_prefix = CONTRIBUTOR_TASK_PREFIX.replace(':', ".");
    let task_names: Vec<_> = index.list().iter().map(|t| t.name.as_str()).collect();

    let contributor_tasks: Vec<_> = task_names
        .iter()
        .filter(|name| name.starts_with(&canonical_prefix))
        .collect();

    assert!(
        !contributor_tasks.is_empty(),
        "Should have at least one contributor task with prefix '{}'",
        canonical_prefix
    );

    for task_name in contributor_tasks {
        assert!(
            task_name.starts_with(&canonical_prefix),
            "Contributor task '{}' should have prefix '{}'",
            task_name,
            canonical_prefix
        );
    }
}
