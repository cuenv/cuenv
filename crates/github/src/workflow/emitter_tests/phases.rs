use super::*;

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
