//! Integration tests for IR contributors
//!
//! These tests load real examples via CUE evaluation and compile them to IR,
//! then verify that the expected phase tasks are contributed by each contributor.
//!
//! This prevents regressions where contributors fail to inject their setup tasks
//! into CI workflows.

use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, Task as IrTask};
use cuenv_core::manifest::Project;
use cuenv_core::module::{ModuleEvaluation, ModuleEvaluationOptions};
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
    let module_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|error| format!("Cannot resolve module root: {error}"))?;
    let directory = example_path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve example directory: {error}"))?;
    let relative = directory
        .strip_prefix(&module_root)
        .map_err(|error| format!("Example is outside module: {error}"))?;
    let relative_str = relative.to_str().ok_or("Example path is not UTF-8")?;
    let target = directory.to_str().ok_or("Example path is not UTF-8")?;
    let options = cuengine::ModuleEvalOptions {
        with_references: true,
        target_dir: Some(target.to_string()),
        instance_failures: cuengine::InstanceFailures::Fail,
        ..Default::default()
    };
    let raw = cuengine::evaluate_module(&module_root, "examples", Some(&options))
        .map_err(|error| format!("Failed to evaluate manifest: {error}"))?;
    let references = raw
        .meta
        .into_iter()
        .filter_map(|(key, value)| {
            value.reference.map(|reference| {
                (
                    cuenv_core::cue::discovery::adjust_meta_key_path(&key, relative_str),
                    reference,
                )
            })
        })
        .collect();
    let module = ModuleEvaluation::from_raw(
        module_root,
        raw.instances,
        ModuleEvaluationOptions {
            project_paths: raw.projects,
            references: Some(references),
        },
    );
    module
        .instances
        .get(relative)
        .ok_or_else(|| format!("No evaluated example at {relative_str}"))?
        .to_project()
        .map_err(|error| format!("Failed to load manifest: {error}"))
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

fn compile_with_registered_contributor(
    example: &str,
    pipeline_name: &str,
    contributor_id: &str,
) -> Result<IntermediateRepresentation, String> {
    let project = load_example(example)?;
    let registered = project.ci.as_ref().is_some_and(|ci| {
        ci.contributors
            .iter()
            .any(|contributor| contributor.id == contributor_id)
    });
    if !registered {
        return Err(format!("{example} does not register {contributor_id}"));
    }
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
    // ci-cachix registers Nix and uses a Nix runtime.
    let ir = compile_example("ci-cachix", "build")?;

    // Check that install-nix bootstrap task is present
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        bootstrap_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:nix.install"),
        "NixContributor should inject 'install-nix' task into bootstrap stage"
    );

    // Verify it has the right contributor
    let install_nix = phase_task(&bootstrap_tasks, "cuenv:contributor:nix.install")?;
    assert_eq!(install_nix.contributor.as_deref(), Some("nix"));
    assert_eq!(
        install_nix.priority,
        Some(2),
        "Nix install should leave room for Namespace cache preflight cleanup"
    );
    Ok(())
}

#[test]
fn test_nix_contributor_active_without_nix_runtime() -> Result<(), String> {
    let mut project = load_example("ci-gh-models")?;
    assert!(project.runtime.is_none());
    let source = load_example("ci-cachix")?;
    let nix = source
        .ci
        .ok_or("ci-cachix has no CI configuration")?
        .contributors
        .into_iter()
        .find(|contributor| contributor.id == "nix")
        .ok_or("ci-cachix does not register Nix")?;
    project
        .ci
        .as_mut()
        .ok_or("ci-gh-models has no CI configuration")?
        .contributors
        .push(nix);
    let ir = compile_with_pipeline(project, "eval")?;

    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let install_nix = phase_task(&bootstrap_tasks, "cuenv:contributor:nix.install")?;
    assert_eq!(install_nix.contributor.as_deref(), Some("nix"));
    Ok(())
}

#[test]
fn test_nix_contributor_absent_without_opt_in() -> Result<(), String> {
    let project = load_example("ci-pipeline")?;
    assert!(project.ci.as_ref().is_some_and(|ci| {
        ci.contributors
            .iter()
            .all(|contributor| contributor.id != "nix")
    }));
    let ir = compile_with_pipeline(project, "default")?;

    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        !bootstrap_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:nix.install"),
        "Nix should not be installed without opting into its contributor"
    );
    Ok(())
}

// ============================================================================
// Cuenv Contributor Tests
// ============================================================================

#[test]
fn test_cuenv_contributor_active_when_registered() -> Result<(), String> {
    let ir = compile_example("ci-cachix", "build")?;

    // CuenvContributor should inject setup-cuenv task
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:cuenv.setup"),
        "CuenvContributor should inject 'setup-cuenv' task into setup stage"
    );

    let setup_cuenv = phase_task(&setup_tasks, "cuenv:contributor:cuenv.setup")?;
    assert_eq!(setup_cuenv.contributor.as_deref(), Some("cuenv"));
    assert_eq!(setup_cuenv.priority, Some(10));
    Ok(())
}

// ============================================================================
// 1Password Contributor Tests
// ============================================================================

#[test]
fn test_onepassword_contributor_active_with_op_refs() -> Result<(), String> {
    // ci-onepassword has op:// refs in production environment
    let ir = compile_example("ci-onepassword", "deploy")?;

    // Check that setup-1password task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:1password.setup"),
        "OnePasswordContributor should inject 'setup-1password' task when op:// refs exist"
    );

    // Verify the command
    let setup_1password = phase_task(&setup_tasks, "cuenv:contributor:1password.setup")?;
    assert_eq!(
        setup_1password.command,
        ["cuenv", "secrets", "setup", "onepassword"]
    );
    assert!(
        setup_1password
            .depends_on
            .contains(&"cuenv:contributor:cuenv.setup".to_string())
    );
    Ok(())
}

#[test]
fn test_onepassword_contributor_inactive_without_op_refs() -> Result<(), String> {
    // ci-pipeline has no 1Password references
    let ir = compile_with_registered_contributor("ci-pipeline", "default", "1password")?;

    // Check that setup-1password task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:1password.setup"),
        "OnePasswordContributor should NOT inject task when no op:// refs"
    );
    Ok(())
}

// ============================================================================
// Cachix Contributor Tests
// ============================================================================

#[test]
fn test_cachix_contributor_active_with_config() -> Result<(), String> {
    // ci-cachix has cachix configuration
    let ir = compile_example("ci-cachix", "build")?;

    // Check that setup-cachix bootstrap task is present before cuenv itself is built
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:cachix.setup"),
        "CachixContributor should inject 'setup-cachix' task when cachix is configured"
    );

    // Verify it uses the configured cache name via the GitHub Action inputs
    let setup_cachix = phase_task(&setup_tasks, "cuenv:contributor:cachix.setup")?;
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
    // ci-pipeline has no cachix configuration
    let ir = compile_with_registered_contributor("ci-pipeline", "default", "cachix")?;

    // Check that setup-cachix task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    assert!(
        !setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:cachix.setup"),
        "CachixContributor should NOT inject task when no cachix config"
    );
    Ok(())
}

#[test]
fn test_namespace_cache_contributor_active_with_config() -> Result<(), String> {
    let ir = compile_example("ci-namespace-cache", "build")?;
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);

    assert!(
        bootstrap_tasks
            .iter()
            .filter(|task| task.contributor.as_deref() == Some("namespaceCache"))
            .all(|task| {
                task.provider_hints
                    .as_ref()
                    .and_then(|hints| hints.get("github_action"))
                    .and_then(|action| action.get("uses"))
                    .and_then(|uses| uses.as_str())
                    != Some("cachix/install-nix-action@v31")
            }),
        "Namespace cache contributor must not install Nix"
    );
    let install = phase_task(&bootstrap_tasks, "cuenv:contributor:nix.install")?;
    assert_eq!(
        github_action_uses(install)?,
        "cachix/install-nix-action@v31"
    );

    let setup_namespace_cache =
        phase_task(&bootstrap_tasks, "cuenv:contributor:namespaceCache.setup")?;
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

    let prepare_receipt = phase_task(
        &bootstrap_tasks,
        "cuenv:contributor:namespaceCache.prepareDeterminateReceipt",
    )?;
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

    let cleanup_receipt = phase_task(
        &bootstrap_tasks,
        "cuenv:contributor:namespaceCache.cleanupDeterminateReceipt",
    )?;
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

#[test]
fn test_hestia_contributor_active_with_config() -> Result<(), String> {
    let ir = compile_example("ci-hestia", "build")?;
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let setup_hestia = phase_task(&bootstrap_tasks, "cuenv:contributor:hestia.setup")?;
    let inputs = github_action_inputs(setup_hestia)?;

    assert_eq!(
        github_action_uses(setup_hestia)?,
        "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527"
    );
    assert_eq!(setup_hestia.priority, Some(4));
    assert!(
        setup_hestia
            .depends_on
            .contains(&"cuenv:contributor:nix.install".to_string()),
        "Hestia must run after Nix is installed"
    );
    assert_eq!(
        inputs.get("version").and_then(Value::as_str),
        Some("v2.0.0")
    );
    assert_eq!(
        inputs.get("upstream-cache-filter").and_then(Value::as_str),
        Some("true")
    );
    assert_eq!(
        inputs.get("drain-timeout").and_then(Value::as_str),
        Some("900")
    );
    Ok(())
}

#[test]
fn test_hestia_contributor_inactive_without_config() -> Result<(), String> {
    let ir = compile_with_registered_contributor("ci-pipeline", "default", "hestia")?;
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);

    assert!(
        bootstrap_tasks
            .iter()
            .all(|task| task.id != "cuenv:contributor:hestia.setup"),
        "Hestia should not be injected without github.hestia configuration"
    );
    Ok(())
}

// ============================================================================
// GH Models Contributor Tests
// ============================================================================

#[test]
fn test_gh_models_contributor_active_with_gh_models_task() -> Result<(), String> {
    // ci-gh-models has a task that uses gh models
    let ir = compile_example("ci-gh-models", "eval")?;

    // Check that setup-gh-models task is present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:gh-models.setup"),
        "GhModelsContributor should inject 'setup-gh-models' task when gh models is used"
    );

    // Verify the command installs the extension
    let setup_gh_models = phase_task(&setup_tasks, "cuenv:contributor:gh-models.setup")?;
    assert!(
        setup_gh_models
            .command
            .iter()
            .any(|arg| arg.contains("gh extension install github/gh-models")),
        "setup-gh-models should install the gh-models extension"
    );
    Ok(())
}

#[test]
fn test_gh_models_contributor_inactive_without_gh_models_task() -> Result<(), String> {
    // ci-pipeline has no gh models tasks
    let ir = compile_with_registered_contributor("ci-pipeline", "default", "gh-models")?;

    // Check that setup-gh-models task is NOT present
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        !setup_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:gh-models.setup"),
        "GhModelsContributor should NOT inject task when no gh models usage"
    );
    Ok(())
}

// ============================================================================
// Phase Priority Ordering Tests
// ============================================================================

#[test]
fn test_phase_tasks_are_sorted_by_priority() -> Result<(), String> {
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
    let ir = compile_example("ci-cachix", "build")?;

    // NixContributor should provide provider_hints for GitHub Actions
    let bootstrap_tasks = ir.sorted_phase_tasks(BuildStage::Bootstrap);
    let install_nix = phase_task(&bootstrap_tasks, "cuenv:contributor:nix.install")?;

    assert!(
        install_nix.provider_hints.is_some(),
        "install-nix should have provider_hints for GitHub Actions"
    );

    assert!(
        github_action_uses(install_nix)?.contains("cachix/install-nix-action"),
        "provider_hints.github_action.uses should contain cachix/install-nix-action"
    );
    assert_eq!(
        github_action_inputs(install_nix)?
            .get("install_url")
            .and_then(Value::as_str),
        Some("https://releases.nixos.org/nix/nix-2.31.2/install")
    );
    assert!(
        !install_nix.github_permissions.contains_key("id-token"),
        "upstream Nix installation does not require OIDC"
    );
    Ok(())
}

// ============================================================================
// Codecov Contributor Tests
// ============================================================================

#[test]
fn test_codecov_contributor_active_with_test_label() -> Result<(), String> {
    // ci-codecov has a task with "test" label, so Codecov contributor should be active
    let ir = compile_example("ci-codecov", "test")?;

    // Check that codecov-upload success task is present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        success_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:codecov-upload"),
        "CodecovContributor should inject 'codecov-upload' task into success phase"
    );

    // Verify it has the right contributor
    let codecov_upload = phase_task(&success_tasks, "cuenv:contributor:codecov-upload")?;
    assert_eq!(codecov_upload.contributor.as_deref(), Some("codecov"));
    Ok(())
}

#[test]
fn test_codecov_contributor_provides_github_action_hints() -> Result<(), String> {
    let ir = compile_example("ci-codecov", "test")?;

    // CodecovContributor should provide provider_hints for GitHub Actions
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = phase_task(&success_tasks, "cuenv:contributor:codecov-upload")?;

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
    let ir = compile_example("ci-codecov", "test")?;

    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    let codecov_upload = phase_task(&success_tasks, "cuenv:contributor:codecov-upload")?;

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
    // ci-pipeline has no test/coverage labels, so Codecov should be inactive
    let ir = compile_with_registered_contributor("ci-pipeline", "default", "codecov")?;

    // Check that codecov-upload task is NOT present
    let success_tasks = ir.sorted_phase_tasks(BuildStage::Success);
    assert!(
        !success_tasks
            .iter()
            .any(|t| t.id == "cuenv:contributor:codecov-upload"),
        "CodecovContributor should NOT inject task when no test/coverage labels"
    );
    Ok(())
}
