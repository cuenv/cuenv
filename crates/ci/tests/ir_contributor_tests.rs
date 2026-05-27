//! Integration tests for IR contributors
//!
//! These tests load real examples via CUE evaluation and compile them to IR,
//! then verify that the expected phase tasks are contributed by each contributor.
//!
//! This prevents regressions where contributors fail to inject their setup tasks
//! into CI workflows.

use cuengine::evaluate_cue_package_typed;
use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, Task as IrTask};
use cuenv_core::manifest::Project;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Get the path to the examples directory
fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples")
}

/// Load a Project manifest from an example directory.
fn load_example_manifest(example_path: &Path) -> Result<Project, String> {
    evaluate_cue_package_typed::<Project>(example_path, "examples")
        .map_err(|e| format!("Failed to load manifest: {e}"))
}

/// Check if the FFI/module evaluation is available for these tests.
fn ffi_available() -> bool {
    let examples_dir = examples_dir();
    let test_path = examples_dir.join("env-basic");
    load_example_manifest(&test_path).is_ok()
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

fn load_example(example: &str) -> Result<Project, String> {
    load_example_manifest(&examples_dir().join(example))
        .map_err(|e| format!("Failed to load {example}: {e}"))
}

fn compile_example(
    example: &str,
    pipeline_name: &str,
) -> Result<IntermediateRepresentation, String> {
    let project = load_example(example)?;
    compile_with_pipeline(project, pipeline_name)
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

fn phase_task<'a>(tasks: &[&'a IrTask], task_id: &str) -> Result<&'a IrTask, String> {
    tasks
        .iter()
        .copied()
        .find(|task| task.id == task_id)
        .ok_or_else(|| format!("Expected contributed task '{task_id}'"))
}

fn github_action_hint(task: &IrTask) -> Result<&Value, String> {
    task.provider_hints
        .as_ref()
        .and_then(|hints| hints.get("github_action"))
        .ok_or_else(|| format!("{} should have a GitHub Action provider hint", task.id))
}

fn github_action_inputs(task: &IrTask) -> Result<&Map<String, Value>, String> {
    github_action_hint(task)?
        .get("inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} should define GitHub Action inputs", task.id))
}

fn github_action_uses(task: &IrTask) -> Result<&str, String> {
    github_action_hint(task)?
        .get("uses")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} should define a GitHub Action uses field", task.id))
}

// ============================================================================
// Nix Contributor Tests
// ============================================================================

#[test]
fn test_nix_contributor_active_with_nix_runtime() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-cachix has a Nix runtime, so NixContributor should be active
    let ir = compile_example("ci-cachix", "build")?;

    // Check that install-nix bootstrap task is present
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        bootstrap_tasks.iter().any(|t| t.id == "install-nix"),
        "NixContributor should inject 'install-nix' task into bootstrap stage"
    );

    // Verify it has the right contributor
    let install_nix = phase_task(&bootstrap_tasks, "install-nix")?;
    assert_eq!(install_nix.contributor.as_deref(), Some("nix"));
    assert_eq!(
        install_nix.priority,
        Some(2),
        "Nix install should leave room for Namespace cache preflight cleanup"
    );
    Ok(())
}

#[test]
fn test_nix_contributor_inactive_without_nix_runtime() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-gh-models has no Nix runtime, so NixContributor should be inactive
    let ir = compile_example("ci-gh-models", "eval")?;

    // Check that install-nix bootstrap task is NOT present
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        !bootstrap_tasks.iter().any(|t| t.id == "install-nix"),
        "NixContributor should NOT inject 'install-nix' task when no Nix runtime"
    );
    Ok(())
}

// ============================================================================
// Cuenv Contributor Tests
// ============================================================================

#[test]
fn test_cuenv_contributor_active_with_nix_runtime() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-cachix", "build")?;

    // CuenvContributor should inject setup-cuenv task
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-cuenv"),
        "CuenvContributor should inject 'setup-cuenv' task into setup stage"
    );

    // Verify it depends on install-nix
    let setup_cuenv = phase_task(&setup_tasks, "setup-cuenv")?;
    assert!(
        setup_cuenv.depends_on.contains(&"install-nix".to_string()),
        "setup-cuenv should depend on install-nix"
    );
    Ok(())
}

// ============================================================================
// 1Password Contributor Tests
// ============================================================================

#[test]
fn test_onepassword_contributor_active_with_op_refs() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-onepassword has op:// refs in production environment
    let ir = compile_example("ci-onepassword", "deploy")?;

    // Check that setup-1password task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should inject 'setup-1password' task when op:// refs exist"
    );

    // Verify the command
    let setup_1password = phase_task(&setup_tasks, "setup-1password")?;
    assert!(
        setup_1password.command[0].contains("cuenv secrets setup onepassword"),
        "setup-1password should run 'cuenv secrets setup onepassword'"
    );
    Ok(())
}

#[test]
fn test_onepassword_contributor_inactive_without_op_refs() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-pipeline has no 1Password references
    let ir = compile_example("ci-pipeline", "default")?;

    // Check that setup-1password task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should NOT inject task when no op:// refs"
    );
    Ok(())
}

// ============================================================================
// Cachix Contributor Tests
// ============================================================================

#[test]
fn test_cachix_contributor_active_with_config() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-cachix has cachix configuration
    let ir = compile_example("ci-cachix", "build")?;

    // Check that setup-cachix bootstrap task is present before cuenv itself is built
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should inject 'setup-cachix' task when cachix is configured"
    );

    // Verify it uses the configured cache name via the GitHub Action inputs
    let setup_cachix = phase_task(&setup_tasks, "setup-cachix")?;
    let inputs = github_action_inputs(setup_cachix)?;

    assert_eq!(
        github_action_uses(setup_cachix)?,
        "cachix/cachix-action@v17"
    );
    assert!(
        inputs.get("name").and_then(|value| value.as_str()) == Some("my-project-cache"),
        "setup-cachix should use the configured cache name"
    );
    assert!(
        inputs.get("authToken").and_then(|value| value.as_str()) == Some("${CACHIX_AUTH_TOKEN}"),
        "setup-cachix should use the default Cachix auth token secret"
    );
    Ok(())
}

#[test]
fn test_cachix_contributor_inactive_without_config() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-pipeline has no cachix configuration
    let ir = compile_example("ci-pipeline", "default")?;

    // Check that setup-cachix task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-cachix"),
        "CachixContributor should NOT inject task when no cachix config"
    );
    Ok(())
}

#[test]
fn test_namespace_cache_contributor_active_with_config() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-namespace-cache", "build")?;
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);

    assert!(
        bootstrap_tasks.iter().all(|task| {
            task.provider_hints
                .as_ref()
                .and_then(|hints| hints.get("github_action"))
                .and_then(|action| action.get("uses"))
                .and_then(|uses| uses.as_str())
                != Some("DeterminateSystems/determinate-nix-action@v3")
        }),
        "Namespace cache contributor must not inject Determinate Nix"
    );

    let setup_namespace_cache = phase_task(&bootstrap_tasks, "namespaceCache.setup")?;
    let inputs = github_action_inputs(setup_namespace_cache)?;
    let github_action = github_action_hint(setup_namespace_cache)?;

    assert_eq!(
        github_action_uses(setup_namespace_cache)?,
        "namespacelabs/nscloud-cache-action@v1"
    );
    assert!(
        inputs.get("cache").and_then(|value| value.as_str()) == Some("nix"),
        "Namespace cache should enable Nix cache mode"
    );
    assert_eq!(
        github_action.get("if").and_then(|value| value.as_str()),
        Some("runner.os == 'Linux'"),
        "Namespace Nix cache mode should run only on Linux runners"
    );

    let prepare_receipt = phase_task(&bootstrap_tasks, "namespaceCache.prepareDeterminateReceipt")?;
    assert_eq!(
        prepare_receipt.priority,
        Some(1),
        "Namespace cache prepare step must run before Determinate Nix install"
    );
    assert!(
        prepare_receipt
            .command
            .iter()
            .any(|command| command.contains("/nix/receipt.json")),
        "Namespace cache prepare should remove stale Determinate Nix receipt metadata"
    );
    assert!(
        prepare_receipt.provider_hints.is_none(),
        "Namespace cache prepare should render as a shell step"
    );

    let cleanup_receipt = phase_task(&bootstrap_tasks, "namespaceCache.cleanupDeterminateReceipt")?;
    assert_eq!(
        cleanup_receipt.priority,
        Some(3),
        "Namespace cache cleanup step must run after Determinate Nix install"
    );
    assert!(
        cleanup_receipt
            .command
            .iter()
            .any(|command| command.contains("/nix/receipt.json")),
        "Namespace cache cleanup should remove Determinate Nix receipt metadata"
    );
    assert!(
        cleanup_receipt.provider_hints.is_none(),
        "Namespace cache cleanup should render as a shell step"
    );
    Ok(())
}

// ============================================================================
// GH Models Contributor Tests
// ============================================================================

#[test]
fn test_gh_models_contributor_active_with_gh_models_task() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-gh-models has a task that uses gh models
    let ir = compile_example("ci-gh-models", "eval")?;

    // Check that setup-gh-models task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should inject 'setup-gh-models' task when gh models is used"
    );

    // Verify the command installs the extension
    let setup_gh_models = phase_task(&setup_tasks, "setup-gh-models")?;
    assert!(
        setup_gh_models.command[0].contains("gh extension install github/gh-models"),
        "setup-gh-models should install the gh-models extension"
    );
    Ok(())
}

#[test]
fn test_gh_models_contributor_inactive_without_gh_models_task() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-pipeline has no gh models tasks
    let ir = compile_example("ci-pipeline", "default")?;

    // Check that setup-gh-models task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks.iter().any(|t| t.id == "setup-gh-models"),
        "GhModelsContributor should NOT inject task when no gh models usage"
    );
    Ok(())
}

// ============================================================================
// Phase Priority Ordering Tests
// ============================================================================

#[test]
fn test_phase_tasks_are_sorted_by_priority() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-cachix", "build")?;

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
    Ok(())
}

// ============================================================================
// Provider Hints Tests (provider_hints replaces ActionSpec)
// ============================================================================

#[test]
fn test_nix_contributor_provides_github_action_hints() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-cachix", "build")?;

    // NixContributor should provide provider_hints for GitHub Actions
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let install_nix = phase_task(&bootstrap_tasks, "install-nix")?;

    assert!(
        install_nix.provider_hints.is_some(),
        "install-nix should have provider_hints for GitHub Actions"
    );

    assert!(
        github_action_uses(install_nix)?.contains("DeterminateSystems/determinate-nix-action"),
        "provider_hints.github_action.uses should contain DeterminateSystems/determinate-nix-action"
    );
    Ok(())
}

// ============================================================================
// Codecov Contributor Tests
// ============================================================================

#[test]
fn test_codecov_contributor_active_with_test_label() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-codecov has a task with "test" label, so Codecov contributor should be active
    let ir = compile_example("ci-codecov", "test")?;

    // Check that codecov-upload success task is present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        success_tasks.iter().any(|t| t.id == "codecov-upload"),
        "CodecovContributor should inject 'codecov-upload' task into success phase"
    );

    // Verify it has the right contributor
    let codecov_upload = phase_task(&success_tasks, "codecov-upload")?;
    assert_eq!(codecov_upload.contributor.as_deref(), Some("codecov"));
    Ok(())
}

#[test]
fn test_codecov_contributor_provides_github_action_hints() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-codecov", "test")?;

    // CodecovContributor should provide provider_hints for GitHub Actions
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = phase_task(&success_tasks, "codecov-upload")?;

    assert!(
        codecov_upload.provider_hints.is_some(),
        "codecov-upload should have provider_hints for GitHub Actions"
    );

    assert!(
        github_action_uses(codecov_upload)?.contains("codecov/codecov-action"),
        "provider_hints.github_action.uses should contain codecov/codecov-action"
    );
    Ok(())
}

#[test]
fn test_codecov_contributor_command_structure() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    let ir = compile_example("ci-codecov", "test")?;

    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = phase_task(&success_tasks, "codecov-upload")?;

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
    Ok(())
}

#[test]
fn test_codecov_contributor_inactive_without_labels() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-pipeline has no test/coverage labels, so Codecov should be inactive
    let ir = compile_example("ci-pipeline", "default")?;

    // Check that codecov-upload task is NOT present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        !success_tasks.iter().any(|t| t.id == "codecov-upload"),
        "CodecovContributor should NOT inject task when no test/coverage labels"
    );
    Ok(())
}
