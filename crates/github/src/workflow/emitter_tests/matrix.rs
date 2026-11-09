use super::{
    ArtifactAggregationJobOptions, BTreeMap, BuildStage, CuenvSetup, GitHubActionsEmitter,
    MatrixJobOptions, OutputType, RunsOn, TaskExecution, TaskPhase,
};
use std::collections::HashMap;

#[test]
fn direct_matrix_retains_cache_setup_environment_and_outputs() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = super::make_task("check", &["nix", "build", ".#check"]);
    task.matrix = Some(cuenv_ci::ir::MatrixConfig {
        dimensions: BTreeMap::from([("arch".into(), vec!["darwin-arm64".into()])]),
        ..Default::default()
    });
    task.env.insert("TOKEN".into(), "${CACHE_TOKEN}".into());
    task.outputs.push(cuenv_ci::ir::OutputDeclaration {
        path: "reports/check.json".into(),
        output_type: OutputType::Orchestrator,
    });
    let mut cache = super::make_phase_task(
        "cache",
        &["configure cache"],
        TaskPhase::new(BuildStage::Bootstrap, 9),
    );
    cache.contributor = Some("cachix".into());
    let mut cuenv = super::make_phase_task(
        "cuenv.setup",
        &["build cuenv"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    cuenv.contributor = Some("cuenv".into());
    let mut secret_setup = super::make_phase_task(
        "secrets",
        &["cuenv secrets setup"],
        TaskPhase::new(BuildStage::Setup, 20),
    );
    secret_setup.depends_on = vec![cuenv.id.clone()];
    let ir = super::make_ir(vec![task.clone(), cache, cuenv, secret_setup]);

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            execution: TaskExecution::Direct,
            cuenv_setup: CuenvSetup::RealizeNixPackage,
            ..Default::default()
        },
    );
    let job = &jobs["check-darwin-arm64"];
    assert!(
        job.steps
            .iter()
            .any(|step| step.run.as_deref() == Some("configure cache"))
    );
    assert!(job.steps.iter().all(|step| {
        !matches!(
            step.run.as_deref(),
            Some("build cuenv" | "cuenv secrets setup")
        ) && step.uses.as_deref() != Some("actions/download-artifact@v4")
    }));
    let command = job
        .steps
        .iter()
        .find(|step| step.run.as_deref() == Some("nix build .#check"))
        .expect("direct Nix command");
    assert_eq!(command.env["TOKEN"], "${{ secrets.CACHE_TOKEN }}");
    let upload = job
        .steps
        .iter()
        .find(|step| step.name.as_deref() == Some("Upload artifacts"))
        .expect("declared output upload");
    assert_eq!(
        upload.with_inputs["path"].as_str(),
        Some("reports/check.json")
    );
}

#[test]
fn matrix_architecture_overrides_task_passthrough() {
    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let mut task = super::make_task("cargo.build", &["nix", "develop"]);
    task.matrix = Some(cuenv_ci::ir::MatrixConfig {
        dimensions: BTreeMap::from([("arch".into(), vec!["linux-x64".into()])]),
        ..Default::default()
    });
    task.env
        .insert("CUENV_ARCH".into(), "cuenv:passthrough:CUENV_ARCH".into());
    let ir = super::make_ir(vec![task.clone()]);

    for execution in [TaskExecution::Orchestrated, TaskExecution::Direct] {
        let jobs = emitter.build_matrix_jobs(
            &task,
            &ir,
            &MatrixJobOptions {
                execution,
                ..Default::default()
            },
        );
        let step = jobs["cargo-build-linux-x64"]
            .steps
            .iter()
            .find(|step| step.name.as_deref() == Some("cargo.build (linux-x64)"))
            .expect("task step");
        assert_eq!(step.env["CUENV_ARCH"], "linux-x64");
    }
}

#[test]
fn test_build_matrix_jobs() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let mut task = super::make_task("release.build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let ir = super::make_ir(vec![task.clone()]);

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: None,
            previous_jobs: &[],
            project_path: None,
            execution: TaskExecution::Orchestrated,
            cuenv_setup: CuenvSetup::BuildInJob,
        },
    );

    // Should create 2 jobs, one per arch
    assert_eq!(jobs.len(), 2);
    for job in jobs.values() {
        let document = serde_yaml::to_value(job).unwrap();
        assert_eq!(document["permissions"]["contents"].as_str(), Some("read"));
        assert_eq!(document["permissions"]["checks"].as_str(), None);
    }
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
    let mut task = super::make_task("build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let ir = super::make_ir(vec![task.clone()]);
    let arch_runners: HashMap<String, String> = [
        ("linux-x64".to_string(), "ubuntu-24.04".to_string()),
        ("darwin-arm64".to_string(), "macos-14".to_string()),
    ]
    .into_iter()
    .collect();

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: Some(&arch_runners),
            previous_jobs: &[],
            project_path: None,
            execution: TaskExecution::Orchestrated,
            cuenv_setup: CuenvSetup::BuildInJob,
        },
    );

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
    let mut task = super::make_task("release.publish", &["./publish.sh"]);
    task.artifact_downloads = vec![ArtifactDownload {
        name: "release-build".to_string(),
        path: "./artifacts".to_string(),
        filter: String::new(),
    }];
    task.params = [("version".to_string(), "1.0.0".to_string())]
        .into_iter()
        .collect();
    let ir = super::make_ir(vec![task.clone()]);
    let previous_jobs = vec![
        "release-build-linux-x64".to_string(),
        "release-build-darwin-arm64".to_string(),
    ];

    let job = emitter.build_artifact_aggregation_job(
        &task,
        &ir,
        &ArtifactAggregationJobOptions {
            environment: None,
            previous_jobs: &previous_jobs,
            project_path: None,
            cuenv_setup: CuenvSetup::BuildInJob,
        },
    );

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
    super::assert_github_context_env(task_step.unwrap());
}

#[test]
fn test_build_matrix_jobs_realize_nix_cuenv_package_per_runner() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let mut bootstrap_task = super::make_phase_task(
        "cuenv:contributor:nix.install",
        &["install nix"],
        TaskPhase::new(BuildStage::Bootstrap, 0),
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());

    let mut cachix_setup = super::make_phase_task(
        "cuenv:contributor:cachix.setup",
        &["setup cachix"],
        TaskPhase::new(BuildStage::Setup, 9),
    );
    cachix_setup.label = Some("Setup Cachix".to_string());
    cachix_setup.contributor = Some("cachix".to_string());

    let mut cuenv_setup = super::make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    cuenv_setup.label = Some("Build cuenv (nix)".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let mut onepassword_setup = super::make_phase_task(
        "cuenv:contributor:1password.setup",
        &["cuenv secrets setup onepassword"],
        TaskPhase::new(BuildStage::Setup, 20),
    );
    onepassword_setup.label = Some("Setup 1Password".to_string());
    onepassword_setup.contributor = Some("1password".to_string());
    onepassword_setup.depends_on = vec!["cuenv:contributor:cuenv.setup".to_string()];

    let mut task = super::make_task("release.build", &["cargo", "build"]);
    task.matrix = Some(MatrixConfig {
        dimensions: [(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let ir = super::make_ir(vec![
        bootstrap_task,
        cachix_setup,
        cuenv_setup,
        onepassword_setup,
        task.clone(),
    ]);
    let arch_runners: HashMap<String, String> = [
        ("linux-x64".to_string(), "ubuntu-24.04".to_string()),
        ("darwin-arm64".to_string(), "macos-14".to_string()),
    ]
    .into_iter()
    .collect();

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: Some(&arch_runners),
            previous_jobs: &[],
            project_path: None,
            execution: TaskExecution::Orchestrated,
            cuenv_setup: CuenvSetup::RealizeNixPackage,
        },
    );
    let linux_job = jobs
        .get("release-build-linux-x64")
        .expect("missing linux matrix job");
    let step_names: Vec<_> = linux_job
        .steps
        .iter()
        .filter_map(|s| s.name.as_deref())
        .collect();

    assert_eq!(
        step_names,
        [
            "Checkout",
            "Install Nix",
            "Setup Cachix",
            "Add cuenv to PATH",
            "Setup 1Password",
            "release.build (linux-x64)",
            "Upload artifacts",
        ]
    );

    let path_step = linux_job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("Add cuenv to PATH"))
        .expect("missing cuenv path step");
    assert!(path_step.run.as_deref().is_some_and(|script| {
        script.contains("nix build .#cuenv --no-link --print-out-paths")
            && script.contains("echo \"$cuenv_package/bin\" >> \"$GITHUB_PATH\"")
            && !script.contains("cmp ")
    }));

    let darwin_job = jobs
        .get("release-build-darwin-arm64")
        .expect("missing darwin matrix job");
    let darwin_step_names: Vec<_> = darwin_job
        .steps
        .iter()
        .filter_map(|step| step.name.as_deref())
        .collect();
    assert_eq!(
        darwin_step_names,
        [
            "Checkout",
            "Install Nix",
            "Setup Cachix",
            "Add cuenv to PATH",
            "Setup 1Password",
            "release.build (darwin-arm64)",
            "Upload artifacts",
        ]
    );
}

#[test]
fn test_task_has_matrix() {
    use cuenv_ci::ir::MatrixConfig;

    let task_without = super::make_task("build", &["cargo", "build"]);
    assert!(!GitHubActionsEmitter::task_has_matrix(&task_without));

    let mut task_with_empty = super::make_task("build", &["cargo", "build"]);
    task_with_empty.matrix = Some(MatrixConfig::default());
    assert!(!GitHubActionsEmitter::task_has_matrix(&task_with_empty));

    let mut task_with_matrix = super::make_task("build", &["cargo", "build"]);
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

    let task_without = super::make_task("build", &["cargo", "build"]);
    assert!(!GitHubActionsEmitter::task_has_artifact_downloads(
        &task_without
    ));

    let mut task_with = super::make_task("publish", &["./publish.sh"]);
    task_with.artifact_downloads = vec![ArtifactDownload {
        name: "build".to_string(),
        path: "./out".to_string(),
        filter: String::new(),
    }];
    assert!(GitHubActionsEmitter::task_has_artifact_downloads(
        &task_with
    ));
}
