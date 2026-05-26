use super::*;
use cuenv_ci::ir::{CachePolicy, PipelineMetadata, ResourceRequirements, Task, TriggerCondition};
use cuenv_core::ci::PipelineMode;
use std::collections::BTreeMap;

/// Create an IR for testing expanded mode behavior.
/// Uses PipelineMode::Expanded explicitly since tests check multi-job output.
fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
    IntermediateRepresentation {
        version: "1.4".to_string(),
        pipeline: PipelineMetadata {
            name: "test-pipeline".to_string(),
            mode: PipelineMode::Expanded,
            environment: None,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: vec![],
        tasks,
    }
}

/// Helper to create a phase task for testing
fn make_phase_task(id: &str, command: &[&str], phase: BuildStage, priority: i32) -> Task {
    Task {
        id: id.to_string(),
        runtime: None,
        command: command.iter().map(|s| (*s).to_string()).collect(),
        shell: command.len() == 1,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Disabled,
        deployment: false,
        manual_approval: false,
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: Some(phase),
        label: None,
        priority: Some(priority),
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

fn make_task(id: &str, command: &[&str]) -> Task {
    Task {
        id: id.to_string(),
        runtime: None,
        command: command.iter().map(|s| (*s).to_string()).collect(),
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Normal,
        deployment: false,
        manual_approval: false,
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

fn assert_github_context_env(step: &Step) {
    assert_eq!(
        step.env.get("GITHUB_TOKEN"),
        Some(&"${{ secrets.GITHUB_TOKEN }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_ACTOR"),
        Some(&"${{ github.actor }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_REF_TYPE"),
        Some(&"${{ github.ref_type }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_REF_NAME"),
        Some(&"${{ github.ref_name }}".to_string())
    );
}

#[test]
fn test_simple_workflow() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("name: test-pipeline"));
    assert!(yaml.contains("jobs:"));
    assert!(yaml.contains("build:"));
    assert!(yaml.contains("cuenv task build"));
}

#[test]
fn test_workflow_with_nix() {
    let emitter = GitHubActionsEmitter::new().with_nix();

    // Build provider_hints for GitHub Actions (matching NixContributor)
    let provider_hints = serde_json::json!({
        "github_action": {
            "uses": "DeterminateSystems/determinate-nix-action@v3",
            "inputs": {
                "extra-conf": "accept-flake-config = true"
            }
        }
    });

    // Create phase tasks that would be contributed by NixContributor
    let mut bootstrap_task =
        make_phase_task("install-nix", &["curl ... | sh"], BuildStage::Bootstrap, 0);
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());
    bootstrap_task.provider_hints = Some(provider_hints);

    let mut setup_task =
        make_phase_task("setup-cuenv", &["nix build .#cuenv"], BuildStage::Setup, 10);
    setup_task.label = Some("Setup cuenv".to_string());
    setup_task.contributor = Some("cuenv".to_string());
    setup_task.depends_on = vec!["install-nix".to_string()];

    let ir = make_ir(vec![
        bootstrap_task,
        setup_task,
        make_task("build", &["cargo", "build"]),
    ]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("DeterminateSystems/determinate-nix-action"));
    assert!(yaml.contains("nix build .#cuenv"));
}

#[test]
fn test_workflow_with_cachix() {
    let emitter = GitHubActionsEmitter::new()
        .with_nix()
        .with_cachix("my-cache");

    // Build provider_hints for GitHub Actions (matching NixContributor)
    let nix_provider_hints = serde_json::json!({
        "github_action": {
            "uses": "DeterminateSystems/determinate-nix-action@v3",
            "inputs": {
                "extra-conf": "accept-flake-config = true"
            }
        }
    });

    // Create phase tasks for Cachix
    let mut bootstrap_task =
        make_phase_task("install-nix", &["curl ... | sh"], BuildStage::Bootstrap, 0);
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());
    bootstrap_task.provider_hints = Some(nix_provider_hints);

    let mut cachix_task = make_phase_task("setup-cachix", &[], BuildStage::Setup, 5);
    cachix_task.label = Some("Setup Cachix (my-cache)".to_string());
    cachix_task.contributor = Some("cachix".to_string());
    cachix_task.depends_on = vec!["install-nix".to_string()];
    cachix_task.provider_hints = Some(serde_json::json!({
        "github_action": {
            "uses": "cachix/cachix-action@v17",
            "inputs": {
                "name": "my-cache",
                "authToken": "${CACHIX_AUTH_TOKEN}"
            }
        }
    }));

    let ir = make_ir(vec![
        bootstrap_task,
        cachix_task,
        make_task("build", &["cargo", "build"]),
    ]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("cachix/cachix-action@v17"));
    assert!(yaml.contains("name: my-cache"));
    assert!(yaml.contains("${{ secrets.CACHIX_AUTH_TOKEN }}"));
    assert!(yaml.contains("Setup Cachix (my-cache)"));
}

#[test]
fn test_workflow_with_dependencies() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut test_task = make_task("test", &["cargo", "test"]);
    test_task.depends_on = vec!["build".to_string()];

    let ir = make_ir(vec![make_task("build", &["cargo", "build"]), test_task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("needs:"));
    assert!(yaml.contains("- build"));
}

#[test]
fn test_workflow_with_manual_approval() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build()
        .with_approval_environment("staging");
    let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
    deploy_task.manual_approval = true;

    let ir = make_ir(vec![deploy_task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("environment: staging"));
}

#[test]
fn test_workflow_with_concurrency_group() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
    deploy_task.concurrency_group = Some("production".to_string());

    let ir = make_ir(vec![deploy_task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("concurrency:"));
    assert!(yaml.contains("group: production"));
}

#[test]
fn test_workflow_with_custom_runner() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build()
        .with_runner("self-hosted");
    let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("runs-on: self-hosted"));
}

#[test]
fn test_workflow_with_resource_tags() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut task = make_task("build", &["cargo", "build"]);
    task.resources = Some(ResourceRequirements {
        cpu: None,
        memory: None,
        tags: vec!["blacksmith-8vcpu-ubuntu-2404".to_string()],
    });

    let ir = make_ir(vec![task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("runs-on: blacksmith-8vcpu-ubuntu-2404"));
}

#[test]
fn test_emit_workflows() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

    let workflows = emitter.emit_workflows(&ir).unwrap();

    assert_eq!(workflows.len(), 1);
    assert!(workflows.contains_key("test-pipeline.yml"));
}

#[test]
fn test_sanitize_filename() {
    assert_eq!(sanitize_filename("CI Pipeline"), "ci-pipeline");
    assert_eq!(sanitize_filename("release/v1"), "releasev1");
    assert_eq!(sanitize_filename("test_workflow"), "test_workflow");
}

#[test]
fn test_sanitize_job_id() {
    assert_eq!(sanitize_job_id("build.test"), "build-test");
    assert_eq!(sanitize_job_id("deploy prod"), "deploy-prod");
}

#[test]
fn test_validation_invalid_id() {
    let emitter = GitHubActionsEmitter::new();
    let ir = make_ir(vec![make_task("invalid task", &["echo"])]);

    let result = emitter.validate(&ir);
    assert!(result.is_err());
}

#[test]
fn test_validation_missing_dependency() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("test", &["cargo", "test"]);
    task.depends_on = vec!["nonexistent".to_string()];

    let ir = make_ir(vec![task]);

    let result = emitter.validate(&ir);
    assert!(result.is_err());
}

#[test]
fn test_format_name() {
    let emitter = GitHubActionsEmitter::new();
    assert_eq!(emitter.format_name(), "github");
    assert_eq!(emitter.file_extension(), "yml");
}

#[test]
fn test_generation_header() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.starts_with("# Generated by cuenv"));
    assert!(yaml.contains("cuenv sync ci"));
}

// =========================================================================
// Tests for new matrix/artifact job building methods
// =========================================================================

#[test]
fn test_build_simple_job() {
    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let task = make_task("build", &["cargo", "build"]);
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::orchestrated(None, None));

    assert_eq!(job.name, Some("build".to_string()));
    assert!(matches!(job.runs_on, RunsOn::Label(ref l) if l == "ubuntu-latest"));
    assert!(job.needs.is_empty()); // Caller sets needs
    assert!(!job.steps.is_empty());

    // Should have checkout and task run steps
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_ref()).collect();
    assert!(step_names.contains(&&"Checkout".to_string()));
    assert!(step_names.contains(&&"build".to_string()));

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("build"))
        .unwrap();
    assert_github_context_env(task_step);
}

#[test]
fn test_build_simple_job_preserves_explicit_task_env() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("publish", &["./publish.sh"]);
    task.env.insert(
        "GITHUB_ACTOR".to_string(),
        "cuenv:passthrough:GITHUB_ACTOR".to_string(),
    );
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::orchestrated(None, None));
    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("publish"))
        .unwrap();

    assert_eq!(
        task_step.env.get("GITHUB_ACTOR"),
        Some(&"cuenv:passthrough:GITHUB_ACTOR".to_string())
    );
    assert_eq!(
        task_step.env.get("GITHUB_TOKEN"),
        Some(&"${{ secrets.GITHUB_TOKEN }}".to_string())
    );
}

#[test]
fn test_build_simple_job_with_environment() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("deploy", &["./deploy.sh"]);
    let ir = make_ir(vec![task.clone()]);
    let env = "production".to_string();

    let job =
        emitter.build_simple_job(&task, &ir, SimpleJobOptions::orchestrated(Some(&env), None));

    // Find the task step and check command includes environment
    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("deploy"));
    assert!(task_step.is_some());
    let run_cmd = task_step.unwrap().run.as_ref().unwrap();
    assert!(run_cmd.contains("-e production"));
    assert!(run_cmd.contains("--skip-dependencies"));
}

#[test]
fn test_build_simple_job_with_working_directory() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("build", &["cargo", "build"]);
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(
        &task,
        &ir,
        SimpleJobOptions::orchestrated(None, Some("platform/my-project")),
    );

    // Find the task step and check working-directory is set
    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("build"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        Some("platform/my-project".to_string())
    );
}

#[test]
fn test_build_simple_job_direct_execution_runs_ir_command() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task(
        "checks.clippy",
        &[
            "nix",
            "build",
            ".#checks.x86_64-linux.cuenv-clippy",
            "-L",
            "--accept-flake-config",
        ],
    );
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::direct(None));

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("checks.clippy"))
        .expect("missing direct task step");

    assert_eq!(
        task_step.run.as_deref(),
        Some("nix build .#checks.x86_64-linux.cuenv-clippy -L --accept-flake-config")
    );
    assert!(
        !task_step
            .run
            .as_deref()
            .unwrap_or_default()
            .contains("cuenv task")
    );
}

#[test]
fn test_build_simple_job_direct_execution_preserves_shell_command() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("security.audit", &["echo first && echo second"]);
    task.shell = true;
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::direct(None));

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("security.audit"))
        .expect("missing shell task step");

    assert_eq!(task_step.run.as_deref(), Some("echo first && echo second"));
}

#[test]
fn test_build_simple_job_direct_execution_skips_cuenv_setup_and_dependents() {
    let emitter = GitHubActionsEmitter::new();

    let mut bootstrap_task = make_phase_task(
        "cuenv:contributor:nix.install",
        &["install nix"],
        BuildStage::Bootstrap,
        0,
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());

    let mut cuenv_setup = make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        BuildStage::Setup,
        10,
    );
    cuenv_setup.label = Some("Setup cuenv".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let mut onepassword_setup = make_phase_task(
        "cuenv:contributor:1password.setup",
        &["cuenv secrets setup onepassword"],
        BuildStage::Setup,
        20,
    );
    onepassword_setup.label = Some("Setup 1Password".to_string());
    onepassword_setup.contributor = Some("1password".to_string());
    onepassword_setup.depends_on = vec!["cuenv:contributor:cuenv.setup".to_string()];

    let task = make_task(
        "checks.nextest",
        &[
            "nix",
            "build",
            ".#checks.x86_64-linux.cuenv-nextest",
            "-L",
            "--accept-flake-config",
        ],
    );
    let ir = make_ir(vec![
        bootstrap_task,
        cuenv_setup,
        onepassword_setup,
        task.clone(),
    ]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::direct(None));
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert!(step_names.contains(&"Checkout"));
    assert!(step_names.contains(&"Install Nix"));
    assert!(step_names.contains(&"checks.nextest"));
    assert!(!step_names.contains(&"Setup cuenv"));
    assert!(!step_names.contains(&"Setup 1Password"));
}

#[test]
fn test_build_cuenv_bootstrap_job_includes_cuenv_and_prereqs_but_not_dependents() {
    let emitter = GitHubActionsEmitter::new();

    let mut bootstrap_task = make_phase_task(
        "cuenv:contributor:nix.install",
        &["install nix"],
        BuildStage::Bootstrap,
        0,
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());

    let mut cachix_setup = make_phase_task(
        "cuenv:contributor:cachix.setup",
        &["setup cachix"],
        BuildStage::Setup,
        5,
    );
    cachix_setup.label = Some("Setup Cachix".to_string());
    cachix_setup.contributor = Some("cachix".to_string());

    let mut cuenv_setup = make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        BuildStage::Setup,
        10,
    );
    cuenv_setup.label = Some("Build cuenv (nix)".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let mut onepassword_setup = make_phase_task(
        "cuenv:contributor:1password.setup",
        &["cuenv secrets setup onepassword"],
        BuildStage::Setup,
        20,
    );
    onepassword_setup.label = Some("Setup 1Password".to_string());
    onepassword_setup.contributor = Some("1password".to_string());
    onepassword_setup.depends_on = vec!["cuenv:contributor:cuenv.setup".to_string()];

    let ir = make_ir(vec![
        bootstrap_task,
        cachix_setup,
        cuenv_setup,
        onepassword_setup,
    ]);

    let job = emitter
        .build_cuenv_bootstrap_job(
            &ir,
            RunsOn::Label("ubuntu-latest".to_string()),
            "build.cuenv",
        )
        .expect("expected cuenv bootstrap job");
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert!(step_names.contains(&"Checkout"));
    assert!(step_names.contains(&"Install Nix"));
    assert!(step_names.contains(&"Setup Cachix"));
    assert!(step_names.contains(&"Build cuenv (nix)"));
    assert!(!step_names.contains(&"Setup 1Password"));
}

#[test]
fn test_build_cuenv_bootstrap_job_respects_disabled_cuenv_build() {
    let emitter = GitHubActionsEmitter::new().without_cuenv_build();

    let mut cuenv_setup = make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        BuildStage::Setup,
        10,
    );
    cuenv_setup.label = Some("Build cuenv (nix)".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let ir = make_ir(vec![cuenv_setup]);

    assert!(
        emitter
            .build_cuenv_bootstrap_job(
                &ir,
                RunsOn::Label("ubuntu-latest".to_string()),
                "build.cuenv"
            )
            .is_none()
    );
}

#[test]
fn test_build_matrix_jobs() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let mut task = make_task("release.build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let ir = make_ir(vec![task.clone()]);

    let jobs = emitter.build_matrix_jobs(&task, &ir, None, None, &[], None);

    // Should create 2 jobs, one per arch
    assert_eq!(jobs.len(), 2);
    assert!(jobs.contains_key("release-build-linux-x64"));
    assert!(jobs.contains_key("release-build-darwin-arm64"));

    // Each job should have the arch in its name
    let linux_job = jobs.get("release-build-linux-x64").unwrap();
    assert_eq!(
        linux_job.name,
        Some("release.build (linux-x64)".to_string())
    );

    // Should have CUENV_ARCH env var
    let task_step = linux_job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("release.build (linux-x64)"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().env.get("CUENV_ARCH"),
        Some(&"linux-x64".to_string())
    );
}

#[test]
fn test_build_matrix_jobs_with_arch_runners() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let mut task = make_task("build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let ir = make_ir(vec![task.clone()]);
    let arch_runners: HashMap<String, String> = [
        ("linux-x64".to_string(), "ubuntu-24.04".to_string()),
        ("darwin-arm64".to_string(), "macos-14".to_string()),
    ]
    .into_iter()
    .collect();

    let jobs = emitter.build_matrix_jobs(&task, &ir, None, Some(&arch_runners), &[], None);

    // Check runners are correctly mapped
    let linux_job = jobs.get("build-linux-x64").unwrap();
    assert!(matches!(linux_job.runs_on, RunsOn::Label(ref l) if l == "ubuntu-24.04"));

    let darwin_job = jobs.get("build-darwin-arm64").unwrap();
    assert!(matches!(darwin_job.runs_on, RunsOn::Label(ref l) if l == "macos-14"));
}

#[test]
fn test_build_artifact_aggregation_job() {
    use cuenv_ci::ir::ArtifactDownload;

    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("release.publish", &["./publish.sh"]);
    task.artifact_downloads = vec![ArtifactDownload {
        name: "release-build".to_string(),
        path: "./artifacts".to_string(),
        filter: String::new(),
    }];
    task.params = [("version".to_string(), "1.0.0".to_string())]
        .into_iter()
        .collect();
    let ir = make_ir(vec![task.clone()]);
    let previous_jobs = vec![
        "release-build-linux-x64".to_string(),
        "release-build-darwin-arm64".to_string(),
    ];

    let job = emitter.build_artifact_aggregation_job(&task, &ir, None, &previous_jobs, None);

    assert_eq!(job.name, Some("release.publish".to_string()));
    assert_eq!(job.needs, previous_jobs);
    assert_eq!(job.timeout_minutes, Some(30));

    // Should have download artifact steps
    let download_steps: Vec<_> = job
        .steps
        .iter()
        .filter(|s| s.uses.as_deref() == Some("actions/download-artifact@v4"))
        .collect();
    assert_eq!(download_steps.len(), 2);

    // Task step should have params as env vars
    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("release.publish"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().env.get("VERSION"),
        Some(&"1.0.0".to_string())
    );
    assert_github_context_env(task_step.unwrap());
}

#[test]
fn test_task_has_matrix() {
    use cuenv_ci::ir::MatrixConfig;

    let task_without = make_task("build", &["cargo", "build"]);
    assert!(!GitHubActionsEmitter::task_has_matrix(&task_without));

    let mut task_with_empty = make_task("build", &["cargo", "build"]);
    task_with_empty.matrix = Some(MatrixConfig::default());
    assert!(!GitHubActionsEmitter::task_has_matrix(&task_with_empty));

    let mut task_with_matrix = make_task("build", &["cargo", "build"]);
    task_with_matrix.matrix = Some(MatrixConfig {
        dimensions: [("arch".to_string(), vec!["x64".to_string()])]
            .into_iter()
            .collect(),
        ..Default::default()
    });
    assert!(GitHubActionsEmitter::task_has_matrix(&task_with_matrix));
}

#[test]
fn test_task_has_artifact_downloads() {
    use cuenv_ci::ir::ArtifactDownload;

    let task_without = make_task("build", &["cargo", "build"]);
    assert!(!GitHubActionsEmitter::task_has_artifact_downloads(
        &task_without
    ));

    let mut task_with = make_task("publish", &["./publish.sh"]);
    task_with.artifact_downloads = vec![ArtifactDownload {
        name: "build".to_string(),
        path: "./out".to_string(),
        filter: String::new(),
    }];
    assert!(GitHubActionsEmitter::task_has_artifact_downloads(
        &task_with
    ));
}

#[test]
fn test_render_phase_steps() {
    let emitter = GitHubActionsEmitter::new();

    let mut bootstrap_task =
        make_phase_task("install-nix", &["curl ... | sh"], BuildStage::Bootstrap, 0);
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());

    let mut setup_task =
        make_phase_task("setup-cuenv", &["nix build .#cuenv"], BuildStage::Setup, 10);
    setup_task.label = Some("Setup cuenv".to_string());
    setup_task.contributor = Some("cuenv".to_string());
    setup_task
        .env
        .insert("MY_VAR".to_string(), "${MY_SECRET}".to_string());

    let ir = make_ir(vec![bootstrap_task, setup_task]);

    let (steps, secret_env_vars) = emitter.render_phase_steps(&ir, TaskExecution::Orchestrated);

    assert_eq!(steps.len(), 2);
    assert!(steps[0].name.as_deref() == Some("Install Nix"));
    assert!(steps[1].name.as_deref() == Some("Setup cuenv"));

    // Secret env vars should be collected
    assert_eq!(
        secret_env_vars.get("MY_VAR"),
        Some(&"${MY_SECRET}".to_string())
    );
}

// =========================================================================
// Working Directory Tests - Comprehensive coverage for monorepo support
// =========================================================================

#[test]
fn test_build_simple_job_without_working_directory() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("build", &["cargo", "build"]);
    let ir = make_ir(vec![task.clone()]);

    // project_path = None means root project, no working-directory
    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::orchestrated(None, None));

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("build"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        None,
        "Root project should NOT have working-directory"
    );
}

#[test]
fn test_build_simple_job_with_nested_working_directory() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("deploy", &["./deploy.sh"]);
    let ir = make_ir(vec![task.clone()]);

    // Deeply nested project path
    let job = emitter.build_simple_job(
        &task,
        &ir,
        SimpleJobOptions::orchestrated(
            None,
            Some("projects/rawkode.academy/platform/email-preferences"),
        ),
    );

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("deploy"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        Some("projects/rawkode.academy/platform/email-preferences".to_string()),
        "Nested project should have correct working-directory"
    );
}

#[test]
fn test_build_matrix_jobs_with_working_directory() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("release.build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [("arch".to_string(), vec!["linux-x64".to_string()])]
            .into_iter()
            .collect(),
        ..Default::default()
    });
    let ir = make_ir(vec![task.clone()]);

    let jobs = emitter.build_matrix_jobs(&task, &ir, None, None, &[], Some("apps/my-service"));

    assert_eq!(jobs.len(), 1);
    let job = jobs.get("release-build-linux-x64").unwrap();

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("release.build (linux-x64)"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        Some("apps/my-service".to_string()),
        "Matrix job should have working-directory"
    );
}

#[test]
fn test_build_matrix_jobs_without_working_directory() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [("arch".to_string(), vec!["linux-x64".to_string()])]
            .into_iter()
            .collect(),
        ..Default::default()
    });
    let ir = make_ir(vec![task.clone()]);

    // project_path = None
    let jobs = emitter.build_matrix_jobs(&task, &ir, None, None, &[], None);

    let job = jobs.get("build-linux-x64").unwrap();
    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("build (linux-x64)"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        None,
        "Root project matrix job should NOT have working-directory"
    );
}

#[test]
fn test_build_artifact_aggregation_job_with_working_directory() {
    use cuenv_ci::ir::ArtifactDownload;

    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("publish", &["./publish.sh"]);
    task.artifact_downloads = vec![ArtifactDownload {
        name: "build".to_string(),
        path: "./out".to_string(),
        filter: String::new(),
    }];
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_artifact_aggregation_job(
        &task,
        &ir,
        None,
        &["build-linux-x64".to_string()],
        Some("services/api"),
    );

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("publish"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        Some("services/api".to_string()),
        "Artifact aggregation job should have working-directory"
    );
}

#[test]
fn test_build_artifact_aggregation_job_without_working_directory() {
    use cuenv_ci::ir::ArtifactDownload;

    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("publish", &["./publish.sh"]);
    task.artifact_downloads = vec![ArtifactDownload {
        name: "build".to_string(),
        path: "./out".to_string(),
        filter: String::new(),
    }];
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_artifact_aggregation_job(
        &task,
        &ir,
        None,
        &["build-linux-x64".to_string()],
        None,
    );

    let task_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("publish"));
    assert!(task_step.is_some());
    assert_eq!(
        task_step.unwrap().working_directory,
        None,
        "Root project aggregation job should NOT have working-directory"
    );
}

#[test]
fn test_working_directory_yaml_serialization() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("test", &["cargo", "test"]);
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(
        &task,
        &ir,
        SimpleJobOptions::orchestrated(None, Some("my-project")),
    );

    // Serialize job to YAML and verify working-directory appears
    let yaml = serde_yaml::to_string(&job).expect("Failed to serialize job");
    assert!(
        yaml.contains("working-directory: my-project"),
        "YAML should contain working-directory field. Got:\n{yaml}"
    );
}

#[test]
fn test_working_directory_not_in_yaml_when_none() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("test", &["cargo", "test"]);
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, SimpleJobOptions::orchestrated(None, None));

    // Serialize job to YAML and verify working-directory does NOT appear
    let yaml = serde_yaml::to_string(&job).expect("Failed to serialize job");
    assert!(
        !yaml.contains("working-directory"),
        "YAML should NOT contain working-directory field. Got:\n{yaml}"
    );
}

// =========================================================================
// Workflow Self-Path Trigger Tests
// =========================================================================

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
