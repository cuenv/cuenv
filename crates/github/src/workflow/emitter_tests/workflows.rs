use super::*;

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
