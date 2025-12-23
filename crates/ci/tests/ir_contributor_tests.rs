//! Integration tests for IR contributors
//!
//! These tests load real examples via CUE evaluation and compile them to IR,
//! then verify that the expected stage tasks are contributed by each contributor.
//!
//! This prevents regressions where contributors fail to inject their setup tasks
//! into CI workflows.

use cuengine::evaluate_cue_package_typed;
use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::ir::IntermediateRepresentation;
use cuenv_core::manifest::Project;
use std::path::{Path, PathBuf};

/// Get the path to the examples directory
fn get_examples_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .join("examples")
}

/// Load a Project manifest from an example directory.
fn load_example_manifest(example_path: &Path) -> Result<Project, String> {
    evaluate_cue_package_typed::<Project>(example_path, "_examples")
        .map_err(|e| format!("Failed to load manifest: {e}"))
}

/// Check if the FFI/module evaluation is available for these tests.
fn ffi_available() -> bool {
    let examples_dir = get_examples_dir();
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
        .and_then(|ci| ci.pipelines.iter().find(|p| p.name == pipeline_name))
        .ok_or_else(|| format!("Pipeline '{pipeline_name}' not found"))?
        .clone();

    let options = CompilerOptions {
        pipeline: Some(pipeline),
        ..Default::default()
    };

    Compiler::with_options(project, options)
        .compile()
        .map_err(|e| format!("Compilation failed: {e}"))
}

/// Compile a project to IR without a specific pipeline context
#[allow(dead_code)]
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

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    // ci-cachix has a Nix runtime, so NixContributor should be active
    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Check that install-nix bootstrap task is present
    assert!(
        ir.stages.bootstrap.iter().any(|t| t.id == "install-nix"),
        "NixContributor should inject 'install-nix' task into bootstrap stage"
    );

    // Verify it has the right provider
    let install_nix = ir
        .stages
        .bootstrap
        .iter()
        .find(|t| t.id == "install-nix")
        .unwrap();
    assert_eq!(install_nix.provider, "nix");
}

#[test]
fn test_nix_contributor_inactive_without_nix_runtime() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-gh-models");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-gh-models");

    // ci-gh-models has no Nix runtime, so NixContributor should be inactive
    let ir = compile_with_pipeline(project, "eval").expect("Failed to compile");

    // Check that install-nix bootstrap task is NOT present
    assert!(
        !ir.stages.bootstrap.iter().any(|t| t.id == "install-nix"),
        "NixContributor should NOT inject 'install-nix' task when no Nix runtime"
    );
}

// ============================================================================
// Cuenv Contributor Tests
// ============================================================================

#[test]
fn test_cuenv_contributor_active_with_nix_runtime() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // CuenvContributor should inject setup-cuenv task
    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-cuenv"),
        "CuenvContributor should inject 'setup-cuenv' task into setup stage"
    );

    // Verify it depends on install-nix
    let setup_cuenv = ir
        .stages
        .setup
        .iter()
        .find(|t| t.id == "setup-cuenv")
        .unwrap();
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

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-onepassword");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-onepassword");

    // ci-onepassword has op:// refs in production environment
    let ir = compile_with_pipeline(project, "deploy").expect("Failed to compile");

    // Check that setup-1password task is present
    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should inject 'setup-1password' task when op:// refs exist"
    );

    // Verify the command
    let setup_1password = ir
        .stages
        .setup
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

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no 1Password references
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-1password task is NOT present
    assert!(
        !ir.stages.setup.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should NOT inject task when no op:// refs"
    );
}

// ============================================================================
// Cachix Contributor Tests
// ============================================================================

#[test]
fn test_cachix_contributor_active_with_config() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    // ci-cachix has cachix configuration
    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Check that setup-cachix task is present
    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should inject 'setup-cachix' task when cachix is configured"
    );

    // Verify it uses the configured cache name
    let setup_cachix = ir
        .stages
        .setup
        .iter()
        .find(|t| t.id == "setup-cachix")
        .unwrap();
    assert!(
        setup_cachix.command[0].contains("my-project-cache"),
        "setup-cachix should use the configured cache name"
    );
}

#[test]
fn test_cachix_contributor_inactive_without_config() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no cachix configuration
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-cachix task is NOT present
    assert!(
        !ir.stages.setup.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should NOT inject task when no cachix config"
    );
}

// ============================================================================
// GH Models Contributor Tests
// ============================================================================

#[test]
fn test_gh_models_contributor_active_with_gh_models_task() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-gh-models");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-gh-models");

    // ci-gh-models has a task that uses gh models
    let ir = compile_with_pipeline(project, "eval").expect("Failed to compile");

    // Check that setup-gh-models task is present
    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should inject 'setup-gh-models' task when gh models is used"
    );

    // Verify the command installs the extension
    let setup_gh_models = ir
        .stages
        .setup
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

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-pipeline");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-pipeline");

    // ci-pipeline has no gh models tasks
    let ir = compile_with_pipeline(project, "default").expect("Failed to compile");

    // Check that setup-gh-models task is NOT present
    assert!(
        !ir.stages.setup.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should NOT inject task when no gh models usage"
    );
}

// ============================================================================
// Stage Priority Ordering Tests
// ============================================================================

#[test]
fn test_stages_are_sorted_by_priority() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // Bootstrap stage tasks should be sorted by priority
    let bootstrap_priorities: Vec<i32> = ir.stages.bootstrap.iter().map(|t| t.priority).collect();
    let mut sorted_bootstrap = bootstrap_priorities.clone();
    sorted_bootstrap.sort_unstable();
    assert_eq!(
        bootstrap_priorities, sorted_bootstrap,
        "Bootstrap tasks should be sorted by priority"
    );

    // Setup stage tasks should be sorted by priority
    let setup_priorities: Vec<i32> = ir.stages.setup.iter().map(|t| t.priority).collect();
    let mut sorted_setup = setup_priorities.clone();
    sorted_setup.sort_unstable();
    assert_eq!(
        setup_priorities, sorted_setup,
        "Setup tasks should be sorted by priority"
    );
}

// ============================================================================
// ActionSpec Tests
// ============================================================================

#[test]
fn test_nix_contributor_provides_action_spec() {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("ci-cachix");
    let project = load_example_manifest(&example_path).expect("Failed to load ci-cachix");

    let ir = compile_with_pipeline(project, "build").expect("Failed to compile");

    // NixContributor should provide ActionSpec for GitHub Actions
    let install_nix = ir
        .stages
        .bootstrap
        .iter()
        .find(|t| t.id == "install-nix")
        .expect("install-nix task should exist");

    assert!(
        install_nix.action.is_some(),
        "install-nix should have ActionSpec for GitHub Actions"
    );

    let action = install_nix.action.as_ref().unwrap();
    assert!(
        action
            .uses
            .contains("DeterminateSystems/nix-installer-action"),
        "ActionSpec should use DeterminateSystems/nix-installer-action"
    );
}
