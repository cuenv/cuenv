use super::*;

#[test]
fn test_build_simple_job() {
    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let task = make_task("build", &["cargo", "build"]);
    let ir = make_ir(vec![task.clone()]);

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));

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

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::orchestrated(None, None));
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

    let job = emitter.build_simple_job(
        &task,
        &ir,
        &SimpleJobOptions::orchestrated(Some(&env), None),
    );

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
        &SimpleJobOptions::orchestrated(None, Some("platform/my-project")),
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

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::direct(None));

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

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::direct(None));

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

    let job = emitter.build_simple_job(&task, &ir, &SimpleJobOptions::direct(None));
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
            CuenvBootstrapJobOptions {
                runs_on: RunsOn::Label("ubuntu-latest".to_string()),
                name: "build.cuenv",
                artifact_name: "cuenv-bootstrap-ubuntu-latest",
            },
        )
        .expect("expected cuenv bootstrap job");
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert!(step_names.contains(&"Checkout"));
    assert!(step_names.contains(&"Install Nix"));
    assert!(step_names.contains(&"Setup Cachix"));
    assert!(step_names.contains(&"Build cuenv (nix)"));
    assert!(step_names.contains(&"Upload cuenv"));
    assert!(!step_names.contains(&"Setup 1Password"));

    let upload_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("Upload cuenv"))
        .expect("missing cuenv upload step");
    assert_eq!(
        upload_step.uses.as_deref(),
        Some("actions/upload-artifact@v4")
    );
    assert_eq!(
        upload_step.with_inputs.get("name"),
        Some(&serde_yaml::Value::String(
            "cuenv-bootstrap-ubuntu-latest".to_string()
        ))
    );
    assert_eq!(
        upload_step.with_inputs.get("path"),
        Some(&serde_yaml::Value::String("result/bin/cuenv".to_string()))
    );
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
                CuenvBootstrapJobOptions {
                    runs_on: RunsOn::Label("ubuntu-latest".to_string()),
                    name: "build.cuenv",
                    artifact_name: "cuenv-bootstrap-ubuntu-latest",
                }
            )
            .is_none()
    );
}

#[test]
fn test_build_simple_job_artifact_setup_skips_only_cuenv_setup() {
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

    let task = make_task("publish.github", &["gh", "release", "upload"]);
    let ir = make_ir(vec![
        bootstrap_task,
        cuenv_setup,
        onepassword_setup,
        task.clone(),
    ]);

    let job = emitter.build_simple_job(
        &task,
        &ir,
        &SimpleJobOptions::orchestrated_with_cuenv_artifact(
            None,
            None,
            "cuenv-bootstrap-ubuntu-latest".to_string(),
        ),
    );
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert!(step_names.contains(&"Install Nix"));
    assert!(step_names.contains(&"Download cuenv"));
    assert!(step_names.contains(&"Add cuenv to PATH"));
    assert!(step_names.contains(&"Setup 1Password"));
    assert!(step_names.contains(&"publish.github"));
    assert!(!step_names.contains(&"Build cuenv (nix)"));

    let download_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("Download cuenv"))
        .expect("missing cuenv download step");
    assert_eq!(
        download_step.with_inputs.get("name"),
        Some(&serde_yaml::Value::String(
            "cuenv-bootstrap-ubuntu-latest".to_string()
        ))
    );
}
