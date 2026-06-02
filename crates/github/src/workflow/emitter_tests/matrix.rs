use super::*;
use std::collections::HashMap;

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

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: None,
            previous_jobs: &[],
            project_path: None,
            cuenv_artifacts_by_runner: None,
        },
    );

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

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: Some(&arch_runners),
            previous_jobs: &[],
            project_path: None,
            cuenv_artifacts_by_runner: None,
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
    assert_github_context_env(task_step.unwrap());
}

#[test]
fn test_build_matrix_jobs_download_cuenv_artifact_per_runner() {
    use cuenv_ci::ir::MatrixConfig;

    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
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
    let ir = make_ir(vec![cuenv_setup, onepassword_setup, task.clone()]);
    let arch_runners: HashMap<String, String> = [
        ("linux-x64".to_string(), "ubuntu-24.04".to_string()),
        ("darwin-arm64".to_string(), "macos-14".to_string()),
    ]
    .into_iter()
    .collect();
    let cuenv_artifacts_by_runner: HashMap<String, String> = [
        (
            "ubuntu-24.04".to_string(),
            "cuenv-bootstrap-ubuntu-24-04".to_string(),
        ),
        (
            "macos-14".to_string(),
            "cuenv-bootstrap-macos-14".to_string(),
        ),
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
            cuenv_artifacts_by_runner: Some(&cuenv_artifacts_by_runner),
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

    assert!(step_names.contains(&"Download cuenv"));
    assert!(step_names.contains(&"Add cuenv to PATH"));
    assert!(step_names.contains(&"Setup 1Password"));
    assert!(!step_names.contains(&"Build cuenv (nix)"));

    let download_step = linux_job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("Download cuenv"))
        .expect("missing cuenv download step");
    assert_eq!(
        download_step.with_inputs.get("name"),
        Some(&serde_yaml::Value::String(
            "cuenv-bootstrap-ubuntu-24-04".to_string()
        ))
    );
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
