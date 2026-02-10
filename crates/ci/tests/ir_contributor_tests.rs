//! Integration tests for IR contributors
//!
//! These tests load real examples via CUE evaluation and compile them to IR,
//! then verify that the expected phase tasks are contributed by each contributor.
//!
//! This prevents regressions where contributors fail to inject their setup tasks
//! into CI workflows.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::expect_used)]

use cuengine::evaluate_cue_package_typed;
use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation};
use cuenv_core::manifest::Project;
use std::path::{Path, PathBuf};

/// Get the path to the examples directory
fn getexamples_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .join("examples")
}

/// Load a Project manifest from an example directory.
fn load_example_manifest(example_path: &Path) -> Result<Project, String> {
    evaluate_cue_package_typed::<Project>(example_path, "examples")
        .map_err(|e| format!("Failed to load manifest: {e}"))
}

/// Check if the FFI/module evaluation is available for these tests.
fn ffi_available() -> bool {
    let examples_dir = getexamples_dir();
    let test_path = examples_dir.join("env-basic");
    load_example_manifest(&test_path).is_ok()
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

/// Compile a project to IR with a specific pipeline
fn compile_with_pipeline(
    project: Project,
    pipeline_name: &str,
) -> Result<IntermediateRepresentation, String> {
    // Find the pipeline by name
    let pipeline = project
        .ci
        .as_ref()
        .and_then(|ci| ci.pipelines.get(pipeline_name))
        .ok_or_else(|| format!("Pipeline '{pipeline_name}' not found"))?
        .clone();

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline),
        ..Default::default()
    };

    Compiler::with_options(project, options)
        .compile()
        .map_err(|e| format!("Compilation failed: {e}"))
}

/// Compile a project to IR without a specific pipeline context
#[allow(dead_code)] // Test helper for non-pipeline compilation
fn compile_without_pipeline(project: Project) -> Result<IntermediateRepresentation, String> {
    Compiler::new(project)
        .compile()
        .map_err(|e| format!("Compilation failed: {e}"))
}

// ============================================================================
// Nix Contributor Tests
// ============================================================================

#[test]
fn test_nix_contributor_active_with_nix_runtime() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    // ci-cachix has a Nix runtime, so NixContributor should be active
    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Check that install-nix bootstrap task is present
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        bootstrap_tasks.iter().any(|t| t.id == "install-nix"),
        "NixContributor should inject 'install-nix' task into bootstrap stage"
    );

    // Verify it has the right contributor
    let install_nix = bootstrap_tasks
        .iter()
        .find(|t| t.id == "install-nix")
        .unwrap();
    assert_eq!(install_nix.contributor.as_deref(), Some("nix"));
}

#[test]
fn test_nix_contributor_inactive_without_nix_runtime() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-gh-models");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-gh-models");

    // ci-gh-models has no Nix runtime, so NixContributor should be inactive
    let ir = compile_with_pipeline(project, "eval").expect("Failed to compile");

    // Check that install-nix bootstrap task is NOT present
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        !bootstrap_tasks.iter().any(|t| t.id == "install-nix"),
        "NixContributor should NOT inject 'install-nix' task when no Nix runtime"
    );
}

// ============================================================================
// Cuenv Contributor Tests
// ============================================================================

#[test]
fn test_cuenv_contributor_active_with_nix_runtime() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // CuenvContributor should inject setup-cuenv task
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-cuenv"),
        "CuenvContributor should inject 'setup-cuenv' task into setup stage"
    );

    // Verify it depends on install-nix
    let setup_cuenv = setup_tasks.iter().find(|t| t.id == "setup-cuenv").unwrap();
    assert!(
        setup_cuenv.depends_on.contains(&"install-nix".to_string()),
        "setup-cuenv should depend on install-nix"
    );
}

// ============================================================================
// 1Password Contributor Tests
// ============================================================================

#[test]
fn test_onepassword_contributor_active_with_op_refs() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-onepassword");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-onepassword");

    // ci-onepassword has op:// refs in production environment
    let ir = compile_with_pipeline(project, "deploy").expect("Failed to compile");

    // Check that setup-1password task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should inject 'setup-1password' task when op:// refs exist"
    );

    // Verify the command
    let setup_1password = setup_tasks
        .iter()
        .find(|t| t.id == "setup-1password")
        .unwrap();
    assert!(
        setup_1password.command[0].contains("cuenv secrets setup onepassword"),
        "setup-1password should run 'cuenv secrets setup onepassword'"
    );
}

#[test]
fn test_onepassword_contributor_inactive_without_op_refs() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no 1Password references
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-1password task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should NOT inject task when no op:// refs"
    );
}

// ============================================================================
// Cachix Contributor Tests
// ============================================================================

#[test]
fn test_cachix_contributor_active_with_config() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    // ci-cachix has cachix configuration
    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Check that setup-cachix task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should inject 'setup-cachix' task when cachix is configured"
    );

    // Verify it uses the configured cache name
    let setup_cachix = setup_tasks.iter().find(|t| t.id == "setup-cachix").unwrap();
    assert!(
        setup_cachix.command[0].contains("my-project-cache"),
        "setup-cachix should use the configured cache name"
    );
}

#[test]
fn test_cachix_contributor_inactive_without_config() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no cachix configuration
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-cachix task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should NOT inject task when no cachix config"
    );
}

// ============================================================================
// GH Models Contributor Tests
// ============================================================================

#[test]
fn test_gh_models_contributor_active_with_gh_models_task() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-gh-models");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-gh-models");

    // ci-gh-models has a task that uses gh models
    let ir = compile_with_pipeline(project, "eval").expect("Failed to compile");

    // Check that setup-gh-models task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should inject 'setup-gh-models' task when gh models is used"
    );

    // Verify the command installs the extension
    let setup_gh_models = setup_tasks
        .iter()
        .find(|t| t.id == "setup-gh-models")
        .unwrap();
    assert!(
        setup_gh_models.command[0].contains("gh extension install github/gh-models"),
        "setup-gh-models should install the gh-models extension"
    );
}

#[test]
fn test_gh_models_contributor_inactive_without_gh_models_task() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no gh models tasks
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-gh-models task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should NOT inject task when no gh models usage"
    );
}

// ============================================================================
// Phase Priority Ordering Tests
// ============================================================================

#[test]
fn test_phase_tasks_are_sorted_by_priority() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Bootstrap phase tasks should be sorted by priority (sorted_phase_tasks returns them sorted)
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let bootstrap_priorities: Vec<i32> = bootstrap_tasks
        .iter()
        .map(|t| t.priority.unwrap_or(0))
        .collect();
    let mut sorted_bootstrap = bootstrap_priorities.clone();
    sorted_bootstrap.sort_unstable();
    assert_eq!(
        bootstrap_priorities, sorted_bootstrap,
        "Bootstrap tasks should be sorted by priority"
    );

    // Setup phase tasks should be sorted by priority
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    let setup_priorities: Vec<i32> = setup_tasks
        .iter()
        .map(|t| t.priority.unwrap_or(0))
        .collect();
    let mut sorted_setup = setup_priorities.clone();
    sorted_setup.sort_unstable();
    assert_eq!(
        setup_priorities, sorted_setup,
        "Setup tasks should be sorted by priority"
    );
}

// ============================================================================
// Provider Hints Tests (provider_hints replaces ActionSpec)
// ============================================================================

#[test]
fn test_nix_contributor_provides_github_action_hints() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // NixContributor should provide provider_hints for GitHub Actions
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let install_nix = bootstrap_tasks
        .iter()
        .find(|t| t.id == "install-nix")
        .expect("install-nix task should exist");

    assert!(
        install_nix.provider_hints.is_some(),
        "install-nix should have provider_hints for GitHub Actions"
    );

    let hints = install_nix.provider_hints.as_ref().unwrap();
    let github_action = hints
        .get("github_action")
        .expect("Should have github_action key");
    let uses = github_action.get("uses").expect("Should have uses field");
    assert!(
        uses.as_str()
            .unwrap()
            .contains("DeterminateSystems/nix-installer-action"),
        "provider_hints.github_action.uses should contain DeterminateSystems/nix-installer-action"
    );
}

// ============================================================================
// Codecov Contributor Tests
// ============================================================================

#[test]
fn test_codecov_contributor_active_with_test_label() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-codecov");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-codecov");

    // ci-codecov has a task with "test" label, so Codecov contributor should be active
    let ir = compile_with_pipeline(project, "test").expect("Failed to compile");

    // Check that codecov-upload success task is present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        success_tasks.iter().any(|t| t.id == "codecov-upload"),
        "CodecovContributor should inject 'codecov-upload' task into success phase"
    );

    // Verify it has the right contributor
    let codecov_upload = success_tasks
        .iter()
        .find(|t| t.id == "codecov-upload")
        .unwrap();
    assert_eq!(codecov_upload.contributor.as_deref(), Some("codecov"));
}

#[test]
fn test_codecov_contributor_provides_github_action_hints() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-codecov");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-codecov");

    let ir = compile_with_pipeline(project, "test").expect("Failed to compile");

    // CodecovContributor should provide provider_hints for GitHub Actions
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = success_tasks
        .iter()
        .find(|t| t.id == "codecov-upload")
        .expect("codecov-upload task should exist");

    assert!(
        codecov_upload.provider_hints.is_some(),
        "codecov-upload should have provider_hints for GitHub Actions"
    );

    let hints = codecov_upload.provider_hints.as_ref().unwrap();
    let github_action = hints
        .get("github_action")
        .expect("Should have github_action key");
    let uses = github_action.get("uses").expect("Should have uses field");
    assert!(
        uses.as_str().unwrap().contains("codecov/codecov-action"),
        "provider_hints.github_action.uses should contain codecov/codecov-action"
    );
}

#[test]
fn test_codecov_contributor_command_structure() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-codecov");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-codecov");

    let ir = compile_with_pipeline(project, "test").expect("Failed to compile");

    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = success_tasks
        .iter()
        .find(|t| t.id == "codecov-upload")
        .expect("codecov-upload task should exist");

    // Verify the command structure
    assert_eq!(codecov_upload.command[0], "codecov");
    assert!(
        codecov_upload
            .command
            .contains(&"upload-process".to_string()),
        "codecov-upload should use upload-process subcommand"
    );
    assert!(
        codecov_upload
            .command
            .contains(&"--auto-detect".to_string()),
        "codecov-upload should use --auto-detect flag"
    );
}

#[test]
fn test_codecov_contributor_inactive_without_labels() {
    skip_if_ffi_unavailable!();

    let examples_dir = getexamples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no test/coverage labels, so Codecov should be inactive
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that codecov-upload task is NOT present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        !success_tasks.iter().any(|t| t.id == "codecov-upload"),
        "CodecovContributor should NOT inject task when no test/coverage labels"
    );
}
