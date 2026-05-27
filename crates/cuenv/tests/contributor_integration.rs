//! Integration tests for workspace contributor DAG injection
//!
//! These tests load real fixtures via CUE evaluation and verify that
//! workspace contributors properly inject tasks and wire dependencies.
//!
//! Run with: cargo test --test contributor_integration

use cuengine::evaluate_cue_package_typed;
use cuenv_core::contributors::{
    CONTRIBUTOR_TASK_PREFIX, Contributor, ContributorContext, ContributorEngine,
    builtin_workspace_contributors,
};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Task, TaskIndex, TaskNode};
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, String>;

/// Get the path to the contributor-tests examples directory
fn contributor_tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
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
    let test_dir = contributor_tests_dir();
    let test_path = test_dir.join("bun-workspace");
    load_manifest(&test_path).is_ok()
}

/// Skip test with message if FFI is unavailable
macro_rules! skip_if_ffi_unavailable {
    () => {
        if !ffi_available() {
            tracing::info!(
                "Skipping test: FFI/module evaluation unavailable (examples need cue.mod root)"
            );
            return Ok(());
        }
    };
}

fn project_path(example: &str) -> PathBuf {
    contributor_tests_dir().join(example)
}

fn load_example_project(example: &str) -> TestResult<(Project, PathBuf)> {
    let path = project_path(example);
    let project = load_manifest(&path)?;
    Ok((project, path))
}

/// Apply workspace contributors to a manifest and return the task index
fn apply_contributors(manifest: &mut Project, project_root: &Path) -> TestResult<TaskIndex> {
    let context = ContributorContext::detect(project_root).with_task_commands(&manifest.tasks);
    let contributors = builtin_workspace_contributors();
    let engine = ContributorEngine::new(&contributors, context);
    engine
        .apply(&mut manifest.tasks)
        .map_err(|e| format!("Failed to apply contributors: {e}"))?;
    TaskIndex::build(&manifest.tasks).map_err(|e| format!("Failed to build task index: {e}"))
}

/// Get the canonical (dotted) form of a contributor task name
fn canonical_contributor_task(suffix: &str) -> String {
    // CONTRIBUTOR_TASK_PREFIX uses colons, TaskIndex normalizes to dots
    format!("{}{}", CONTRIBUTOR_TASK_PREFIX.replace(':', "."), suffix)
}

fn task_names(index: &TaskIndex) -> Vec<&str> {
    index.list().iter().map(|task| task.name.as_str()).collect()
}

fn resolved_task<'a>(index: &'a TaskIndex, name: &str) -> TestResult<&'a Task> {
    let indexed = index
        .resolve(name)
        .map_err(|e| format!("{name} task should exist: {e}"))?;
    match &indexed.node {
        TaskNode::Task(task) => Ok(task.as_ref()),
        _ => Err(format!("expected single task definition for '{name}'")),
    }
}

fn apply_contributor_engine(
    manifest: &mut Project,
    project_root: &Path,
    contributors: &[Contributor],
) -> TestResult<()> {
    let context = ContributorContext::detect(project_root).with_task_commands(&manifest.tasks);
    let engine = ContributorEngine::new(contributors, context);
    engine
        .apply(&mut manifest.tasks)
        .map_err(|e| format!("Failed to apply contributors: {e}"))?;
    Ok(())
}

fn build_index(manifest: &Project) -> TestResult<TaskIndex> {
    TaskIndex::build(&manifest.tasks).map_err(|e| format!("Failed to build task index: {e}"))
}

// ============================================================================
// Bun Workspace Contributor Tests
// ============================================================================

#[test]
fn test_bun_contributor_injects_install_task() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("bun-workspace")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // Verify bun.workspace.install task was injected
    let install_task = canonical_contributor_task("bun.workspace.install");
    let task_names = task_names(&index);

    assert!(
        task_names.contains(&install_task.as_str()),
        "bun.workspace.install should be injected when bun.lock exists, got: {:?}",
        task_names
    );
    Ok(())
}

#[test]
fn test_bun_contributor_injects_setup_task() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("bun-workspace")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // Verify bun.workspace.setup task was injected
    let setup_task = canonical_contributor_task("bun.workspace.setup");
    let task_names = task_names(&index);

    assert!(
        task_names.contains(&setup_task.as_str()),
        "bun.workspace.setup should be injected when bun.lock exists, got: {:?}",
        task_names
    );
    Ok(())
}

// ============================================================================
// NPM Workspace Contributor Tests
// ============================================================================

#[test]
fn test_npm_contributor_injects_install_task() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("npm-workspace")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // Verify npm.workspace.install task was injected
    let install_task = canonical_contributor_task("npm.workspace.install");
    let task_names = task_names(&index);

    assert!(
        task_names.contains(&install_task.as_str()),
        "npm.workspace.install should be injected when package-lock.json exists, got: {:?}",
        task_names
    );
    Ok(())
}

// ============================================================================
// Auto-Association Tests
// ============================================================================

#[test]
fn test_auto_association_adds_depends_on_for_bun_command() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("auto-associate")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // The 'dev' task uses bun command, should depend on bun.workspace.setup
    let dev_task = resolved_task(&index, "dev")?;
    let expected_dep = canonical_contributor_task("bun.workspace.setup");

    assert!(
        dev_task
            .depends_on
            .iter()
            .any(|d| d.task_name() == expected_dep),
        "dev task should auto-depend on {}, got: {:?}",
        expected_dep,
        dev_task.depends_on
    );
    Ok(())
}

#[test]
fn test_auto_association_does_not_affect_non_bun_tasks() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("auto-associate")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // The 'lint' task uses echo command, should NOT depend on bun.workspace.setup
    let lint_task = resolved_task(&index, "lint")?;
    let bun_setup = canonical_contributor_task("bun.workspace.setup");

    assert!(
        !lint_task
            .depends_on
            .iter()
            .any(|d| d.task_name() == bun_setup),
        "lint task should NOT auto-depend on bun setup (it doesn't use bun), got: {:?}",
        lint_task.depends_on
    );
    Ok(())
}

// ============================================================================
// No Contributors Tests
// ============================================================================

#[test]
fn test_no_lockfile_no_injection() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("no-contributors")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // No lockfiles = no workspace contributor tasks should be injected
    let task_names = task_names(&index);

    // Should only have the original tasks (build, test)
    assert_eq!(
        task_names.len(),
        2,
        "without lockfiles, only original tasks should exist, got: {:?}",
        task_names
    );
    assert!(task_names.contains(&"build"));
    assert!(task_names.contains(&"test"));
    Ok(())
}

// ============================================================================
// Idempotency Tests
// ============================================================================

#[test]
fn test_idempotent_injection() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("bun-workspace")?;

    // Apply contributors once
    let contributors = builtin_workspace_contributors();
    apply_contributor_engine(&mut project, &project_path, &contributors)?;
    let first_index = build_index(&project)?;
    let first_tasks: Vec<_> = first_index.list().iter().map(|t| t.name.clone()).collect();

    // Apply contributors again (should be idempotent)
    apply_contributor_engine(&mut project, &project_path, &contributors)?;
    let second_index = build_index(&project)?;
    let second_tasks: Vec<_> = second_index.list().iter().map(|t| t.name.clone()).collect();

    // Task lists should be identical
    assert_eq!(
        first_tasks, second_tasks,
        "Applying contributors twice should produce the same task list"
    );
    Ok(())
}

// ============================================================================
// Task Naming Convention Tests
// ============================================================================

#[test]
fn test_contributor_task_naming_convention() -> TestResult {
    skip_if_ffi_unavailable!();

    let (mut project, project_path) = load_example_project("bun-workspace")?;

    let index = apply_contributors(&mut project, &project_path)?;

    // All injected contributor tasks should have the cuenv.contributor. prefix
    let canonical_prefix = CONTRIBUTOR_TASK_PREFIX.replace(':', ".");
    let task_names = task_names(&index);

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
    Ok(())
}
