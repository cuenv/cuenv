use super::{BuildStage, Emitter, GitHubActionsEmitter, ResourceRequirements, TaskPhase};
#[test]
fn test_simple_workflow() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);

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
            "uses": "cachix/install-nix-action@v31",
            "inputs": {
                "extra_nix_config": "accept-flake-config = true"
            }
        }
    });

    // Create phase tasks that would be contributed by NixContributor
    let mut bootstrap_task = super::make_phase_task(
        "install-nix",
        &["curl ... | sh"],
        TaskPhase::new(BuildStage::Bootstrap, 0),
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());
    bootstrap_task.provider_hints = Some(provider_hints);

    let mut setup_task = super::make_phase_task(
        "setup-cuenv",
        &["nix build .#cuenv"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    setup_task.label = Some("Setup cuenv".to_string());
    setup_task.contributor = Some("cuenv".to_string());
    setup_task.depends_on = vec!["install-nix".to_string()];

    let ir = super::make_ir(vec![
        bootstrap_task,
        setup_task,
        super::make_task("build", &["cargo", "build"]),
    ]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("cachix/install-nix-action"));
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
            "uses": "cachix/install-nix-action@v31",
            "inputs": {
                "extra_nix_config": "accept-flake-config = true"
            }
        }
    });

    // Create phase tasks for Cachix
    let mut bootstrap_task = super::make_phase_task(
        "install-nix",
        &["curl ... | sh"],
        TaskPhase::new(BuildStage::Bootstrap, 0),
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());
    bootstrap_task.provider_hints = Some(nix_provider_hints);

    let mut cachix_task =
        super::make_phase_task("setup-cachix", &[], TaskPhase::new(BuildStage::Setup, 5));
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

    let ir = super::make_ir(vec![
        bootstrap_task,
        cachix_task,
        super::make_task("build", &["cargo", "build"]),
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
    let mut test_task = super::make_task("test", &["cargo", "test"]);
    test_task.depends_on = vec!["build".to_string()];

    let ir = super::make_ir(vec![
        super::make_task("build", &["cargo", "build"]),
        test_task,
    ]);

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
    let mut deploy_task = super::make_task("deploy", &["./deploy.sh"]);
    deploy_task.manual_approval = true;

    let ir = super::make_ir(vec![deploy_task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("environment: staging"));
}

#[test]
fn test_workflow_with_concurrency_group() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut deploy_task = super::make_task("deploy", &["./deploy.sh"]);
    deploy_task.concurrency_group = Some("production".to_string());

    let ir = super::make_ir(vec![deploy_task]);

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
    let ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("runs-on: self-hosted"));
}

#[test]
fn test_workflow_with_resource_tags() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let mut task = super::make_task("build", &["cargo", "build"]);
    task.resources = Some(ResourceRequirements {
        cpu: None,
        memory: None,
        tags: vec!["blacksmith-8vcpu-ubuntu-2404".to_string()],
    });

    let ir = super::make_ir(vec![task]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.contains("runs-on: blacksmith-8vcpu-ubuntu-2404"));
}

#[test]
fn test_emit_workflows() {
    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);

    let workflows = emitter.emit_workflows(&ir).unwrap();

    assert_eq!(workflows.len(), 1);
    assert!(workflows.contains_key("test-pipeline.yml"));
}

#[test]
fn test_sanitize_filename() {
    assert_eq!(
        super::super::sanitize_filename("CI Pipeline"),
        "ci-pipeline"
    );
    assert_eq!(super::super::sanitize_filename("release/v1"), "releasev1");
    assert_eq!(
        super::super::sanitize_filename("test_workflow"),
        "test_workflow"
    );
}

#[test]
fn test_sanitize_job_id() {
    assert_eq!(super::super::sanitize_job_id("build.test"), "build-test");
    assert_eq!(super::super::sanitize_job_id("deploy prod"), "deploy-prod");
}

#[test]
fn test_validation_invalid_id() {
    let emitter = GitHubActionsEmitter::new();
    let ir = super::make_ir(vec![super::make_task("invalid task", &["echo"])]);

    let result = emitter.validate(&ir);
    assert!(result.is_err());
}

#[test]
fn test_validation_missing_dependency() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = super::make_task("test", &["cargo", "test"]);
    task.depends_on = vec!["nonexistent".to_string()];

    let ir = super::make_ir(vec![task]);

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
    let ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);

    let yaml = emitter.emit(&ir).unwrap();

    assert!(yaml.starts_with("# Generated by cuenv"));
    assert!(yaml.contains("cuenv sync ci"));
}

#[test]
fn permissions_are_scoped_to_jobs_in_both_modes() {
    let mut emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    emitter
        .configured_permissions
        .insert("id-token".into(), "write".into());
    emitter
        .configured_permissions
        .insert("checks".into(), "none".into());
    let mut deploy = super::make_task("deploy", &["deploy"]);
    deploy.deployment = true;
    let ir = super::make_ir(vec![super::make_task("check", &["cargo", "check"]), deploy]);
    for (yaml, expected_checks) in [
        (emitter.emit_expanded(&ir).unwrap(), None),
        (emitter.emit_thin(&ir).unwrap(), Some("none")),
    ] {
        let workflow: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert!(workflow.get("permissions").is_none());
        for job in workflow["jobs"].as_mapping().unwrap().values() {
            assert_eq!(job["permissions"]["id-token"].as_str(), None);
            assert_eq!(job["permissions"]["checks"].as_str(), expected_checks);
        }
        if let Some(check) = workflow["jobs"].get("check") {
            assert_eq!(check["permissions"]["contents"].as_str(), Some("read"));
            assert_eq!(
                workflow["jobs"]["deploy"]["permissions"]["contents"].as_str(),
                Some("write")
            );
        }
    }
}

#[test]
fn test_remote_cache_separates_ro_pr_and_protected_rw_credentials() {
    use cuenv_core::manifest::{CacheAuth, CacheProtocol, RemoteCache, RemoteCacheCi};

    let remote_cache = RemoteCache {
        endpoint: "https://cache.example.com".to_string(),
        protocol: CacheProtocol::Http,
        instance: "ci".to_string(),
        upload: true,
        auth: Some(CacheAuth {
            bearer_token_env: Some("CUENV_CACHE_TOKEN".to_string()),
            header: None,
            credential_helper: None,
        }),
        tls: None,
        ci: Some(RemoteCacheCi {
            read_secret: Some("CACHE_RO_TOKEN".to_string()),
            write_secret: Some("CACHE_RW_TOKEN".to_string()),
            write_environment: Some("cache-write".to_string()),
            write_queue: None,
            isolated_cache_dir: true,
            ..Default::default()
        }),
    };

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build()
        .with_remote_cache(remote_cache);

    // 1. Protected push-only workflow gets write environment + RW secret + trusted cache dir.
    let mut protected_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    protected_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(false),
        ..Default::default()
    });
    let protected_yaml = emitter.emit_thin(&protected_ir).unwrap();
    assert!(protected_yaml.contains("environment: cache-write"));
    assert!(protected_yaml.contains("secrets.CACHE_RW_TOKEN"));
    assert!(!protected_yaml.contains("secrets.CACHE_RO_TOKEN"));
    assert!(protected_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'true'"));
    assert!(protected_yaml.contains("cuenv-cache/trusted"));

    protected_ir.pipeline.environment = Some("deploy".to_string());
    let conflict_yaml = emitter.emit_thin(&protected_ir).unwrap();
    assert!(conflict_yaml.contains("environment: deploy"));
    assert!(!conflict_yaml.contains("secrets.CACHE_RW_TOKEN"));
    assert!(conflict_yaml.contains("secrets.CACHE_RO_TOKEN"));
    assert!(conflict_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));

    protected_ir.pipeline.environment = None;
    if let Some(trigger) = &mut protected_ir.pipeline.trigger {
        trigger.manual = Some(cuenv_ci::ir::ManualTriggerConfig {
            enabled: true,
            ..Default::default()
        });
    }
    let manual_yaml = emitter.emit_thin(&protected_ir).unwrap();
    assert!(!manual_yaml.contains("secrets.CACHE_RW_TOKEN"));
    assert!(manual_yaml.contains("secrets.CACHE_RO_TOKEN"));

    // 2. Pull-request workflow gets only RO secret, no write environment, and untrusted cache dir.
    let mut pr_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    pr_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(true),
        ..Default::default()
    });
    let pr_yaml = emitter.emit_thin(&pr_ir).unwrap();
    assert!(!pr_yaml.contains("environment: cache-write"));
    assert!(!pr_yaml.contains("secrets.CACHE_RW_TOKEN"));
    assert!(pr_yaml.contains("secrets.CACHE_RO_TOKEN"));
    assert!(pr_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
    assert!(pr_yaml.contains("cuenv-cache/untrusted"));
}

#[test]
fn test_remote_cache_ambient_mode_preserves_env_and_gates_uploads() {
    use cuenv_core::manifest::{
        CacheAuth, CacheProtocol, RemoteCache, RemoteCacheCi, RemoteCacheCiAuthMode,
    };

    let remote_cache = RemoteCache {
        endpoint: "grpcs://cache.namespace.so:443".to_string(),
        protocol: CacheProtocol::Grpc,
        instance: String::new(),
        upload: true,
        auth: Some(CacheAuth {
            bearer_token_env: None,
            header: None,
            credential_helper: Some("bazel-credential-nsc".to_string()),
        }),
        tls: None,
        ci: Some(RemoteCacheCi {
            auth_mode: RemoteCacheCiAuthMode::Ambient,
            isolated_cache_dir: true,
            ..Default::default()
        }),
    };

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build()
        .with_remote_cache(remote_cache);

    // Protected push workflow with an existing `environment: production` keeps
    // its environment, does NOT overwrite CUENV_REMOTE_CACHE or inject static
    // secrets, enables upload, and isolates CUENV_CACHE_DIR to trusted.
    let mut protected_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    protected_ir.pipeline.environment = Some("production".to_string());
    protected_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(false),
        ..Default::default()
    });
    let protected_yaml = emitter.emit_thin(&protected_ir).unwrap();
    assert!(protected_yaml.contains("environment: production"));
    assert!(!protected_yaml.contains("CUENV_REMOTE_CACHE:"));
    assert!(!protected_yaml.contains("secrets.CACHE_"));
    assert!(protected_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'true'"));
    assert!(protected_yaml.contains("cuenv-cache/trusted"));

    // Pull-request workflow disables upload and isolates CUENV_CACHE_DIR to untrusted.
    let mut pr_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    pr_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(true),
        ..Default::default()
    });
    let pr_yaml = emitter.emit_thin(&pr_ir).unwrap();
    assert!(!pr_yaml.contains("CUENV_REMOTE_CACHE:"));
    assert!(pr_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
    assert!(pr_yaml.contains("cuenv-cache/untrusted"));
}

#[test]
fn test_remote_cache_ambient_mode_gates_manual_and_scheduled_triggers_dynamically() {
    use cuenv_core::manifest::{
        CacheAuth, CacheProtocol, RemoteCache, RemoteCacheCi, RemoteCacheCiAuthMode,
    };

    let make_emitter = |upload| {
        GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build()
            .with_remote_cache(RemoteCache {
                endpoint: "grpcs://cache.namespace.so:443".to_string(),
                protocol: CacheProtocol::Grpc,
                instance: String::new(),
                upload,
                auth: Some(CacheAuth {
                    bearer_token_env: None,
                    header: None,
                    credential_helper: Some("bazel-credential-nsc".to_string()),
                }),
                tls: None,
                ci: Some(RemoteCacheCi {
                    auth_mode: RemoteCacheCiAuthMode::Ambient,
                    isolated_cache_dir: true,
                    ..Default::default()
                }),
            })
    };

    let emitter = make_emitter(true);

    // Default-branch push + workflow_dispatch gates upload & cache dir dynamically.
    let mut main_manual_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    main_manual_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string()],
        pull_request: Some(false),
        manual: Some(cuenv_ci::ir::ManualTriggerConfig {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let main_manual_yaml = emitter.emit_thin(&main_manual_ir).unwrap();
    assert!(main_manual_yaml.contains(
        "CUENV_REMOTE_CACHE_UPLOAD: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
    ));
    assert!(main_manual_yaml.contains(
        "CUENV_CACHE_DIR: ${{ runner.temp }}/cuenv-cache/${{ (github.event_name == 'push' && github.ref == 'refs/heads/main') && 'trusted' || 'untrusted' }}"
    ));

    // Multiple literal branches join with || inside parentheses.
    let mut multi_branch_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    multi_branch_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main".to_string(), "release".to_string()],
        manual: Some(cuenv_ci::ir::ManualTriggerConfig {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let multi_branch_yaml = emitter.emit_thin(&multi_branch_ir).unwrap();
    assert!(multi_branch_yaml.contains(
        "CUENV_REMOTE_CACHE_UPLOAD: ${{ github.event_name == 'push' && (github.ref == 'refs/heads/main' || github.ref == 'refs/heads/release') }}"
    ));

    // Glob branch patterns fall back to ref_type matching.
    let mut glob_branch_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    glob_branch_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["release/*".to_string()],
        scheduled: vec!["0 2 * * *".to_string()],
        ..Default::default()
    });
    let glob_branch_yaml = emitter.emit_thin(&glob_branch_ir).unwrap();
    assert!(glob_branch_yaml.contains(
        "CUENV_REMOTE_CACHE_UPLOAD: ${{ github.event_name == 'push' && github.ref_type == 'branch' }}"
    ));

    // Combined tag + release triggers with manual dispatch emit both event clauses.
    let mut release_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    release_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        tags: vec!["v*".to_string()],
        release: vec!["published".to_string()],
        manual: Some(cuenv_ci::ir::ManualTriggerConfig {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let release_yaml = emitter.emit_thin(&release_ir).unwrap();
    assert!(release_yaml.contains(
        "CUENV_REMOTE_CACHE_UPLOAD: ${{ (github.event_name == 'push' && github.ref_type == 'tag') || github.event_name == 'release' }}"
    ));

    // Patterns with single quotes fail closed to disabled.
    let mut unsafe_ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);
    unsafe_ir.pipeline.trigger = Some(cuenv_ci::ir::TriggerCondition {
        branches: vec!["main' || true".to_string()],
        manual: Some(cuenv_ci::ir::ManualTriggerConfig {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let unsafe_yaml = emitter.emit_thin(&unsafe_ir).unwrap();
    assert!(unsafe_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
    assert!(unsafe_yaml.contains("cuenv-cache/untrusted"));

    // upload: false stays statically disabled even on protected + manual workflows.
    let readonly_emitter = make_emitter(false);
    let readonly_yaml = readonly_emitter.emit_thin(&main_manual_ir).unwrap();
    assert!(readonly_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
    assert!(readonly_yaml.contains("cuenv-cache/untrusted"));
}

#[test]
fn workflow_run_name_is_emitted_when_configured_and_omitted_by_default() {
    let ir = super::make_ir(vec![super::make_task("build", &["cargo", "build"])]);

    let default_emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();
    let default_doc: serde_yaml::Value =
        serde_yaml::from_str(&default_emitter.emit_expanded(&ir).unwrap()).unwrap();
    assert!(default_doc.get("run-name").is_none());

    let config = crate::config::GitHubConfig {
        run_name: Some("Deploy main @ ${{ github.sha }}".to_string()),
        ..Default::default()
    };
    let emitter = GitHubActionsEmitter::from_config(&config)
        .without_nix()
        .without_cuenv_build();
    for yaml in [
        emitter.emit_thin(&ir).unwrap(),
        emitter.emit_expanded(&ir).unwrap(),
    ] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["run-name"].as_str(),
            Some("Deploy main @ ${{ github.sha }}")
        );
    }
}

#[test]
fn workflow_concurrency_supports_bool_expression_and_per_event_map() {
    use cuenv_core::ci::{CancelInProgressByEvent, CancelInProgressConfig, PipelineConcurrency};

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    // 1. Static false with default group
    let mut ir_false = super::make_ir(vec![super::make_task("deploy", &["deploy"])]);
    ir_false.pipeline.concurrency = Some(PipelineConcurrency {
        group: None,
        cancel_in_progress: Some(CancelInProgressConfig::Bool(false)),
    });
    for yaml in [
        emitter.emit_thin(&ir_false).unwrap(),
        emitter.emit_expanded(&ir_false).unwrap(),
    ] {
        let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["concurrency"]["group"].as_str(),
            Some(GitHubActionsEmitter::DEFAULT_CONCURRENCY_GROUP)
        );
        assert_eq!(
            doc["concurrency"]["cancel-in-progress"].as_bool(),
            Some(false)
        );
    }

    // 2. Expression strings for both group and cancel-in-progress
    let mut ir_expr = super::make_ir(vec![super::make_task("check", &["check"])]);
    ir_expr.pipeline.concurrency = Some(PipelineConcurrency {
        group: Some(
            "pr-title-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || github.run_id }}"
                .to_string(),
        ),
        cancel_in_progress: Some(CancelInProgressConfig::Expression(
            "${{ github.event_name == 'pull_request' }}".to_string(),
        )),
    });
    let expr_doc: serde_yaml::Value =
        serde_yaml::from_str(&emitter.emit_expanded(&ir_expr).unwrap()).unwrap();
    assert_eq!(
        expr_doc["concurrency"]["cancel-in-progress"].as_str(),
        Some("${{ github.event_name == 'pull_request' }}")
    );
    assert!(
        expr_doc["concurrency"]["group"]
            .as_str()
            .unwrap()
            .starts_with("pr-title-")
    );

    // 3. Per-event map rendered as an expression
    let mut ir_events = super::make_ir(vec![super::make_task("check", &["check"])]);
    ir_events.pipeline.concurrency = Some(PipelineConcurrency {
        group: None,
        cancel_in_progress: Some(CancelInProgressConfig::ByEvent(CancelInProgressByEvent {
            pull_request: Some(true),
            push: Some(false),
            ..Default::default()
        })),
    });
    let events_doc: serde_yaml::Value =
        serde_yaml::from_str(&emitter.emit_expanded(&ir_events).unwrap()).unwrap();
    assert_eq!(
        events_doc["concurrency"]["cancel-in-progress"].as_str(),
        Some("${{ github.event_name == 'pull_request' }}")
    );

    // 4. Per-event map with all false simplifies to boolean false
    let mut ir_none = super::make_ir(vec![super::make_task("release", &["release"])]);
    ir_none.pipeline.concurrency = Some(PipelineConcurrency {
        group: Some("release-${{ github.ref }}".to_string()),
        cancel_in_progress: Some(CancelInProgressConfig::ByEvent(CancelInProgressByEvent {
            pull_request: Some(false),
            push: Some(false),
            ..Default::default()
        })),
    });
    let none_doc: serde_yaml::Value =
        serde_yaml::from_str(&emitter.emit_expanded(&ir_none).unwrap()).unwrap();
    assert_eq!(
        none_doc["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false)
    );
}

#[test]
fn pipeline_gate_and_changes_emit_and_verify_truth_table() {
    use cuenv_core::ci::{PipelineChangeLane, PipelineChanges, PipelineGate, TaskGitHubJobConfig};
    use std::collections::BTreeMap;

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let rust_task = super::make_task("check.rust", &["cargo", "check"]);
    let web_task = super::make_task("check.web", &["bun", "test"]);
    let mut always_task = super::make_task("pr-title", &["scripts/pr-title.sh"]);
    always_task.github_job = Some(TaskGitHubJobConfig {
        if_condition: Some("github.event_name == 'pull_request'".to_string()),
        ..Default::default()
    });

    let mut ir = super::make_ir(vec![rust_task, web_task, always_task]);
    ir.pipeline.changes = Some(PipelineChanges {
        job: Some("changes".to_string()),
        lanes: BTreeMap::from([
            (
                "rust".to_string(),
                PipelineChangeLane {
                    tasks: vec!["check.rust".to_string()],
                },
            ),
            (
                "web".to_string(),
                PipelineChangeLane {
                    tasks: vec!["check.web".to_string(), "pr-title".to_string()],
                },
            ),
        ]),
        script: None,
        runner: None,
    });
    ir.pipeline.gate = Some(PipelineGate {
        name: "ci-ok".to_string(),
        required: vec!["changes".to_string()],
        runner: None,
        timeout_minutes: Some(5),
    });

    // 1. Expanded workflow: changes job + lane ifs + custom if preserved + gate job.
    let expanded_yaml = emitter.emit_expanded(&ir).expect("emit expanded workflow");
    let doc: serde_yaml::Value = serde_yaml::from_str(&expanded_yaml).expect("parse expanded yaml");

    let changes_job = &doc["jobs"]["changes"];
    assert_eq!(
        changes_job["outputs"]["rust"].as_str(),
        Some("${{ steps.detect.outputs.rust }}")
    );
    assert_eq!(
        changes_job["outputs"]["web"].as_str(),
        Some("${{ steps.detect.outputs.web }}")
    );
    let detect_step = &changes_job["steps"][1];
    assert_eq!(detect_step["id"].as_str(), Some("detect"));
    let detect_run = detect_step["run"].as_str().expect("detect run script");
    assert!(detect_run.contains("merge_group)"));
    assert!(detect_run.contains("pull_request|pull_request_target)"));
    assert!(detect_run.contains("push)"));
    assert_eq!(
        detect_step["env"]["MERGE_GROUP_BASE_SHA"].as_str(),
        Some("${{ github.event.merge_group.base_sha }}")
    );
    assert_eq!(
        detect_step["env"]["PR_BASE_SHA"].as_str(),
        Some("${{ github.event.pull_request.base.sha }}")
    );
    assert_eq!(
        detect_step["env"]["PUSH_BEFORE_SHA"].as_str(),
        Some("${{ github.event.before }}")
    );

    assert_eq!(
        doc["jobs"]["check-rust"]["if"].as_str(),
        Some("needs.changes.outputs.rust == 'true'")
    );
    assert_eq!(
        doc["jobs"]["check-web"]["if"].as_str(),
        Some("needs.changes.outputs.web == 'true'")
    );
    // Explicit task `if` overrides automatic lane `if`.
    assert_eq!(
        doc["jobs"]["pr-title"]["if"].as_str(),
        Some("github.event_name == 'pull_request'")
    );

    let gate_job = &doc["jobs"]["ci-ok"];
    assert_eq!(gate_job["name"].as_str(), Some("ci-ok"));
    assert_eq!(gate_job["if"].as_str(), Some("always()"));
    assert_eq!(
        gate_job["permissions"]
            .as_mapping()
            .map(serde_yaml::Mapping::len),
        Some(0)
    );
    let gate_needs: Vec<&str> = gate_job["needs"]
        .as_sequence()
        .expect("gate needs")
        .iter()
        .filter_map(serde_yaml::Value::as_str)
        .collect();
    assert_eq!(
        gate_needs,
        vec!["changes", "check-rust", "check-web", "pr-title"]
    );

    // Execute the generated dependency-free POSIX sh gate script against the pass/skip/fail/cancel truth table.
    let gate_script = gate_job["steps"][0]["run"]
        .as_str()
        .expect("gate run script");
    let cases = [
        // (changes[req], check-rust, check-web, pr-title, expected_pass)
        ("success", "success", "success", "success", true),
        ("success", "skipped", "skipped", "skipped", true),
        ("skipped", "success", "success", "success", false),
        ("failure", "success", "success", "success", false),
        ("cancelled", "success", "success", "success", false),
        ("success", "failure", "skipped", "success", false),
        ("success", "skipped", "cancelled", "success", false),
    ];
    for (r0, r1, r2, r3, expected_pass) in cases {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(gate_script)
            .env("NEED_RESULT_0", r0)
            .env("NEED_RESULT_1", r1)
            .env("NEED_RESULT_2", r2)
            .env("NEED_RESULT_3", r3)
            .status()
            .expect("run POSIX sh gate script");
        assert_eq!(
            status.success(),
            expected_pass,
            "gate truth table mismatch for ({r0}, {r1}, {r2}, {r3})"
        );
    }

    // 2. Thin workflow also emits the gate job needing the main thin job.
    let thin_yaml = emitter.emit_thin(&ir).expect("emit thin workflow");
    let thin_doc: serde_yaml::Value = serde_yaml::from_str(&thin_yaml).expect("parse thin yaml");
    assert_eq!(thin_doc["jobs"]["ci-ok"]["if"].as_str(), Some("always()"));
    assert_eq!(
        thin_doc["jobs"]["ci-ok"]["needs"][0].as_str(),
        Some("test-pipeline")
    );

    // 3. Custom detection script hook replaces the default detection step.
    let mut custom_ir = ir;
    if let Some(changes) = custom_ir.pipeline.changes.as_mut() {
        changes.script = Some("./scripts/detect-lanes.sh".to_string());
    }
    let custom_yaml = emitter
        .emit_expanded(&custom_ir)
        .expect("emit custom changes script");
    let custom_doc: serde_yaml::Value =
        serde_yaml::from_str(&custom_yaml).expect("parse custom yaml");
    assert_eq!(
        custom_doc["jobs"]["changes"]["steps"][1]["run"].as_str(),
        Some("./scripts/detect-lanes.sh")
    );
}

#[test]
fn change_lane_does_not_capture_other_tasks_sharing_its_job_id_prefix() {
    use cuenv_core::ci::{PipelineChangeLane, PipelineChanges};
    use std::collections::BTreeMap;

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut ir = super::make_ir(vec![
        super::make_task("check", &["cargo", "check"]),
        super::make_task("check-web", &["bun", "test"]),
    ]);
    ir.pipeline.changes = Some(PipelineChanges {
        job: None,
        lanes: BTreeMap::from([(
            "rust".to_string(),
            PipelineChangeLane {
                tasks: vec!["check".to_string()],
            },
        )]),
        script: None,
        runner: None,
    });

    let yaml = emitter.emit_expanded(&ir).expect("emit expanded workflow");
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse yaml");

    assert_eq!(
        doc["jobs"]["check"]["if"].as_str(),
        Some("needs.changes.outputs.rust == 'true'")
    );
    assert!(doc["jobs"]["check-web"]["if"].is_null());
    assert!(doc["jobs"]["check-web"]["needs"].is_null());
}

#[test]
fn job_concurrency_default_group_is_namespaced_by_workflow() {
    use cuenv_core::ci::{CancelInProgressConfig, PipelineConcurrency, TaskGitHubJobConfig};

    let emitter = GitHubActionsEmitter::new()
        .without_nix()
        .without_cuenv_build();

    let mut task = super::make_task("deploy", &["deploy"]);
    task.github_job = Some(TaskGitHubJobConfig {
        concurrency: Some(PipelineConcurrency {
            group: None,
            cancel_in_progress: Some(CancelInProgressConfig::Bool(true)),
        }),
        ..Default::default()
    });

    let yaml = emitter
        .emit_expanded(&super::make_ir(vec![task]))
        .expect("emit expanded workflow");
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse yaml");

    assert_eq!(
        doc["jobs"]["deploy"]["concurrency"]["group"].as_str(),
        Some("${{ github.workflow }}-deploy-${{ github.ref }}")
    );
}
