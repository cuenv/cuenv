use super::*;

#[test]
fn test_build_simple_job_without_working_directory() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("build", &["cargo", "build"]);
    let ir = make_ir(vec![task.clone()]);

    // project_path = None means root project, no working-directory
    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));

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
        &SimpleJobOptions::orchestrated(
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

    let jobs = emitter.build_matrix_jobs(
        &task,
        &ir,
        &MatrixJobOptions {
            environment: None,
            arch_runners: None,
            previous_jobs: &[],
            project_path: Some("apps/my-service"),
            cuenv_artifacts_by_runner: None,
        },
    );

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

    let previous_jobs = vec!["build-linux-x64".to_string()];
    let job = emitter.build_artifact_aggregation_job(
        &task,
        &ir,
        &ArtifactAggregationJobOptions {
            environment: None,
            previous_jobs: &previous_jobs,
            project_path: Some("services/api"),
            cuenv_setup: CuenvSetup::BuildInJob,
        },
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

    let previous_jobs = vec!["build-linux-x64".to_string()];
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
        &SimpleJobOptions::orchestrated(None, Some("my-project")),
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

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));

    // Serialize job to YAML and verify working-directory does NOT appear
    let yaml = serde_yaml::to_string(&job).expect("Failed to serialize job");
    assert!(
        !yaml.contains("working-directory"),
        "YAML should NOT contain working-directory field. Got:\n{yaml}"
    );
}
