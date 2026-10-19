use super::*;

fn permissions(job: &Job) -> serde_yaml::Value {
    serde_yaml::to_value(job.permissions.as_ref().unwrap()).unwrap()
}

#[test]
fn plain_jobs_are_read_only_and_task_grants_stay_local() {
    let emitter = GitHubActionsEmitter::new();
    let lint = make_task("lint", &["lint"]);
    let mut publish = make_task("publish", &["publish"]);
    publish
        .github_permissions
        .insert("packages".into(), "write".into());
    let ir = make_ir(vec![lint.clone(), publish.clone()]);
    let options = SimpleJobOptions::orchestrated(None, None);
    assert_eq!(
        permissions(&emitter.build_simple_job(&lint, &ir, &options)),
        serde_yaml::from_str::<serde_yaml::Value>("contents: read").unwrap()
    );
    assert_eq!(
        permissions(&emitter.build_simple_job(&publish, &ir, &options))["packages"],
        "write"
    );
}

#[test]
fn contributor_grants_follow_emitted_steps() {
    let emitter = GitHubActionsEmitter::new();
    let lint = make_task("lint", &["lint"]);
    let publish = make_task("publish.cue", &["publish"]);
    let mut auth = make_phase_task("auth", &["login"], BuildStage::Setup, 25);
    auth.github_permissions
        .insert("id-token".into(), "write".into());
    auth.github_task_filter = vec![publish.id.clone()];
    let ir = make_ir(vec![lint.clone(), publish.clone(), auth]);
    let options = SimpleJobOptions::orchestrated(None, None);
    let lint_job = emitter.build_simple_job(&lint, &ir, &options);
    assert!(
        !lint_job
            .steps
            .iter()
            .any(|s| s.name.as_deref() == Some("auth"))
    );
    assert!(permissions(&lint_job)["id-token"].is_null());
    let publish_job = emitter.build_simple_job(&publish, &ir, &options);
    assert!(
        publish_job
            .steps
            .iter()
            .any(|s| s.name.as_deref() == Some("auth"))
    );
    assert_eq!(permissions(&publish_job)["id-token"], "write");
}

#[test]
fn skipped_setup_and_success_steps_do_not_widen_direct_jobs() {
    let emitter = GitHubActionsEmitter::new();
    let task = make_task("lint", &["lint"]);
    let mut setup = make_phase_task("setup", &["setup"], BuildStage::Setup, 10);
    setup.contributor = Some("cuenv".into());
    setup
        .github_permissions
        .insert("packages".into(), "write".into());
    let mut success = make_phase_task("comment", &["comment"], BuildStage::Success, 50);
    success
        .github_permissions
        .insert("pull-requests".into(), "write".into());
    let ir = make_ir(vec![task.clone(), setup, success]);
    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::direct(None));
    assert!(permissions(&job)["packages"].is_null());
    assert!(permissions(&job)["pull-requests"].is_null());
    let yaml: serde_yaml::Value = serde_yaml::from_str(&emitter.emit_thin(&ir).unwrap()).unwrap();
    let jobs = yaml["jobs"].as_mapping().unwrap();
    let thin = jobs.values().next().unwrap();
    assert_eq!(thin["permissions"]["packages"], "write");
    assert_eq!(thin["permissions"]["pull-requests"], "write");
}

#[test]
fn pipeline_ceilings_clamp_never_grant_and_none_forbids() {
    let mut emitter = GitHubActionsEmitter::new();
    emitter.configured_permissions = HashMap::from([
        ("contents".into(), "read".into()),
        ("packages".into(), "none".into()),
        ("id-token".into(), "write".into()),
    ]);
    let mut task = make_task("publish", &["publish"]);
    task.deployment = true;
    task.github_permissions
        .insert("packages".into(), "write".into());
    let ir = make_ir(vec![task.clone()]);
    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));
    assert_eq!(permissions(&job)["contents"], "read");
    assert_eq!(permissions(&job)["packages"], "none");
    assert!(permissions(&job)["id-token"].is_null());
}

#[test]
fn bootstrap_ignores_pipeline_map_and_artifact_upload_needs_no_write() {
    let mut emitter = GitHubActionsEmitter::new();
    emitter
        .configured_permissions
        .insert("packages".into(), "write".into());
    emitter
        .configured_permissions
        .insert("contents".into(), "none".into());
    let mut setup = make_phase_task("cuenv.setup", &["build"], BuildStage::Setup, 10);
    setup.contributor = Some("cuenv".into());
    let ir = make_ir(vec![setup]);
    let job = emitter
        .build_cuenv_bootstrap_job(
            &ir,
            CuenvBootstrapJobOptions {
                runs_on: RunsOn::Label("ubuntu-latest".into()),
                name: "Build cuenv",
                artifact_name: "cuenv",
            },
        )
        .unwrap();
    assert_eq!(
        permissions(&job),
        serde_yaml::from_str::<serde_yaml::Value>("contents: read").unwrap()
    );
}

#[test]
fn bootstrap_preserves_requirements_of_injected_cache_steps() {
    let emitter = GitHubActionsEmitter::new();
    let mut setup = make_phase_task("cuenv.setup", &["build"], BuildStage::Setup, 10);
    setup.contributor = Some("cuenv".into());
    let mut cache = make_phase_task("cache", &["cache"], BuildStage::Bootstrap, 1);
    cache
        .github_permissions
        .insert("id-token".into(), "write".into());
    let ir = make_ir(vec![setup, cache]);
    let job = emitter
        .build_cuenv_bootstrap_job(
            &ir,
            CuenvBootstrapJobOptions {
                runs_on: RunsOn::Label("ubuntu-latest".into()),
                name: "Build cuenv",
                artifact_name: "cuenv",
            },
        )
        .unwrap();
    assert_eq!(permissions(&job)["id-token"], "write");
}

#[test]
fn matrix_jobs_keep_own_task_grants_and_drop_unrelated_phase_grants() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = make_task("publish", &["publish"]);
    task.matrix = Some(cuenv_ci::ir::MatrixConfig {
        dimensions: BTreeMap::from([(
            "arch".into(),
            vec!["linux-x64".into(), "darwin-arm64".into()],
        )]),
        ..Default::default()
    });
    task.github_permissions
        .insert("packages".into(), "write".into());
    let mut auth = make_phase_task("auth", &["login"], BuildStage::Setup, 25);
    auth.github_permissions
        .insert("id-token".into(), "write".into());
    auth.github_task_filter = vec!["other".into()];
    let ir = make_ir(vec![task.clone(), auth]);
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
    assert_eq!(jobs.len(), 2);
    for job in jobs.values() {
        assert_eq!(permissions(job)["packages"], "write");
        assert!(permissions(job)["id-token"].is_null());
    }
}

#[test]
fn thin_reporter_grants_are_attributed_and_capped() {
    let mut emitter = GitHubActionsEmitter::new();
    let mut ir = make_ir(vec![make_task("lint", &["lint"])]);
    let grants = serde_yaml::to_value(emitter.build_permissions(&ir)).unwrap();
    assert_eq!(grants["checks"], "write");
    assert!(grants["pull-requests"].is_null());
    ir.pipeline.trigger = Some(TriggerCondition {
        pull_request: Some(true),
        ..Default::default()
    });
    let grants = serde_yaml::to_value(emitter.build_permissions(&ir)).unwrap();
    assert_eq!(grants["pull-requests"], "write");
    emitter
        .configured_permissions
        .insert("checks".into(), "none".into());
    emitter
        .configured_permissions
        .insert("pull-requests".into(), "none".into());
    let grants = serde_yaml::to_value(emitter.build_permissions(&ir)).unwrap();
    assert_eq!(grants["checks"], "none");
    assert_eq!(grants["pull-requests"], "none");
}
