use super::{
    BTreeMap, BuildStage, CuenvBootstrapJobOptions, CuenvSetup, Emitter, GitHubActionsEmitter,
    HashMap, Job, MatrixJobOptions, RunsOn, SimpleJobOptions, TaskExecution, TaskPhase,
    TriggerCondition,
};
fn permissions(job: &Job) -> serde_yaml::Value {
    serde_yaml::to_value(job.permissions.as_ref().unwrap()).unwrap()
}

#[test]
fn plain_jobs_are_read_only_and_task_grants_stay_local() {
    let emitter = GitHubActionsEmitter::new();
    let lint = super::make_task("lint", &["lint"]);
    let mut publish = super::make_task("publish", &["publish"]);
    publish
        .github_permissions
        .insert("packages".into(), "write".into());
    let ir = super::make_ir(vec![lint.clone(), publish.clone()]);
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
    let lint = super::make_task("lint", &["lint"]);
    let publish = super::make_task("publish.cue", &["publish"]);
    let mut auth =
        super::make_phase_task("auth", &["login"], TaskPhase::new(BuildStage::Setup, 25));
    auth.github_permissions
        .insert("id-token".into(), "write".into());
    auth.github_task_filter = vec![publish.id.clone()];
    let ir = super::make_ir(vec![lint.clone(), publish.clone(), auth]);
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
    let task = super::make_task("lint", &["lint"]);
    let mut setup =
        super::make_phase_task("setup", &["setup"], TaskPhase::new(BuildStage::Setup, 10));
    setup.contributor = Some("cuenv".into());
    setup
        .github_permissions
        .insert("packages".into(), "write".into());
    let mut success = super::make_phase_task(
        "comment",
        &["comment"],
        TaskPhase::new(BuildStage::Success, 50),
    );
    success
        .github_permissions
        .insert("pull-requests".into(), "write".into());
    let ir = super::make_ir(vec![task.clone(), setup, success]);
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
    let mut task = super::make_task("publish", &["publish"]);
    task.deployment = true;
    task.github_permissions
        .insert("packages".into(), "write".into());
    let ir = super::make_ir(vec![task.clone()]);
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
    let mut setup = super::make_phase_task(
        "cuenv.setup",
        &["build"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    setup.contributor = Some("cuenv".into());
    let ir = super::make_ir(vec![setup]);
    let job = emitter
        .build_cuenv_bootstrap_job(
            &ir,
            CuenvBootstrapJobOptions {
                runs_on: RunsOn::Label("ubuntu-latest".into()),
                name: "Build cuenv",
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
    let mut setup = super::make_phase_task(
        "cuenv.setup",
        &["build"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    setup.contributor = Some("cuenv".into());
    let mut cache = super::make_phase_task(
        "cache",
        &["cache"],
        TaskPhase::new(BuildStage::Bootstrap, 1),
    );
    cache
        .github_permissions
        .insert("id-token".into(), "write".into());
    let ir = super::make_ir(vec![setup, cache]);
    let job = emitter
        .build_cuenv_bootstrap_job(
            &ir,
            CuenvBootstrapJobOptions {
                runs_on: RunsOn::Label("ubuntu-latest".into()),
                name: "Build cuenv",
            },
        )
        .unwrap();
    assert_eq!(permissions(&job)["id-token"], "write");
}

#[test]
fn matrix_jobs_keep_own_task_grants_and_drop_unrelated_phase_grants() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = super::make_task("publish", &["publish"]);
    task.matrix = Some(cuenv_ci::ir::MatrixConfig {
        dimensions: BTreeMap::from([(
            "arch".into(),
            vec!["linux-x64".into(), "darwin-arm64".into()],
        )]),
        ..Default::default()
    });
    task.github_permissions
        .insert("packages".into(), "write".into());
    let mut auth =
        super::make_phase_task("auth", &["login"], TaskPhase::new(BuildStage::Setup, 25));
    auth.github_permissions
        .insert("id-token".into(), "write".into());
    auth.github_task_filter = vec!["other".into()];
    let ir = super::make_ir(vec![task.clone(), auth]);
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
    assert_eq!(jobs.len(), 2);
    for job in jobs.values() {
        assert_eq!(permissions(job)["packages"], "write");
        assert!(permissions(job)["id-token"].is_null());
    }
}

#[test]
fn thin_reporter_grants_are_attributed_and_capped() {
    let mut emitter = GitHubActionsEmitter::new();
    let mut ir = super::make_ir(vec![super::make_task("lint", &["lint"])]);
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

#[test]
fn all_github_permission_scopes_are_emitted_and_clamped() {
    let mut emitter = GitHubActionsEmitter::new();
    emitter
        .configured_permissions
        .insert("attestations".into(), "read".into());
    let mut task = super::make_task("attest", &["attest"]);
    for (scope, level) in [
        ("actions", "read"),
        ("artifact-metadata", "write"),
        ("attestations", "write"),
        ("checks", "write"),
        ("contents", "write"),
        ("deployments", "write"),
        ("discussions", "read"),
        ("id-token", "write"),
        ("issues", "write"),
        ("models", "read"),
        ("packages", "write"),
        ("pages", "write"),
        ("pull-requests", "write"),
        ("repository-projects", "read"),
        ("security-events", "write"),
        ("statuses", "write"),
    ] {
        task.github_permissions
            .insert(scope.to_string(), level.to_string());
    }
    let ir = super::make_ir(vec![task.clone()]);
    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));
    let perms = permissions(&job);
    assert_eq!(perms["artifact-metadata"], "write");
    assert_eq!(perms["attestations"], "read"); // clamped by pipeline ceiling
    assert_eq!(perms["deployments"], "write");
    assert_eq!(perms["discussions"], "read");
    assert_eq!(perms["models"], "read");
    assert_eq!(perms["pages"], "write");
    assert_eq!(perms["repository-projects"], "read");
    assert_eq!(perms["security-events"], "write");
    assert_eq!(perms["statuses"], "write");
}

#[test]
fn explicit_empty_workflow_permissions_emits_empty_block_without_inference() {
    let config = crate::config::GitHubConfig {
        permissions: Some(HashMap::new()),
        ..Default::default()
    };
    let emitter = GitHubActionsEmitter::from_config(&config)
        .without_nix()
        .without_cuenv_build();
    let ir = super::make_ir(vec![super::make_task("check", &["cargo", "check"])]);

    for yaml in [
        emitter.emit_thin(&ir).unwrap(),
        emitter.emit_expanded(&ir).unwrap(),
    ] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["permissions"],
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        );
        for job in doc["jobs"].as_mapping().unwrap().values() {
            assert!(job.get("permissions").is_none());
        }
    }
}

#[test]
fn explicit_empty_job_permissions_emits_verbatim_and_rejects_required_scopes() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut gate = super::make_task("ci-ok", &["true"]);
    gate.github_permissions_mode = cuenv_ci::ir::GitHubPermissionsMode::ExplicitEmpty;
    let check = super::make_task("check", &["cargo", "check"]);
    let ir = super::make_ir(vec![check, gate]);

    let yaml = emitter.emit_expanded(&ir).unwrap();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert!(doc.get("permissions").is_none());
    assert_eq!(
        doc["jobs"]["ci-ok"]["permissions"],
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    );
    assert_eq!(doc["jobs"]["check"]["permissions"]["contents"], "read");

    // A task requiring a scope under explicit empty workflow permissions fails.
    let empty_config = crate::config::GitHubConfig {
        permissions: Some(HashMap::new()),
        ..Default::default()
    };
    let empty_emitter = GitHubActionsEmitter::from_config(&empty_config)
        .without_nix()
        .without_cuenv_build();
    let mut publish = super::make_task("publish", &["publish"]);
    publish
        .github_permissions
        .insert("attestations".into(), "write".into());
    let conflict_ir = super::make_ir(vec![publish]);
    assert!(empty_emitter.emit_expanded(&conflict_ir).is_err());
    assert!(empty_emitter.emit_thin(&conflict_ir).is_err());

    // A job with explicit empty permissions fails if a matching phase step needs a scope.
    let mut restricted = super::make_task("restricted", &["true"]);
    restricted.github_permissions_mode = cuenv_ci::ir::GitHubPermissionsMode::ExplicitEmpty;
    let mut auth =
        super::make_phase_task("auth", &["login"], TaskPhase::new(BuildStage::Setup, 25));
    auth.github_permissions
        .insert("id-token".into(), "write".into());
    let job_conflict_ir = super::make_ir(vec![restricted, auth]);
    assert!(emitter.emit_expanded(&job_conflict_ir).is_err());
}
