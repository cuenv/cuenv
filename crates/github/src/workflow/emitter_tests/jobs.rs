use super::{
    BuildStage, CuenvBootstrapJobOptions, CuenvSetup, GitHubActionsEmitter, RunsOn,
    SimpleJobOptions, TaskPhase,
};
#[test]
fn test_build_simple_job() {
    let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
    let task = super::make_task("build", &["cargo", "build"]);
    let ir = super::make_ir(vec![task.clone()]);

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
    super::assert_github_context_env(task_step);
}

#[test]
fn test_build_simple_job_preserves_explicit_task_env() {
    let emitter = GitHubActionsEmitter::new();
    let mut task = super::make_task("publish", &["./publish.sh"]);
    task.env.insert(
        "GITHUB_ACTOR".to_string(),
        "cuenv:passthrough:GITHUB_ACTOR".to_string(),
    );
    let ir = super::make_ir(vec![task.clone()]);

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
    let task = super::make_task("deploy", &["./deploy.sh"]);
    let ir = super::make_ir(vec![task.clone()]);
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
    let task = super::make_task("build", &["cargo", "build"]);
    let ir = super::make_ir(vec![task.clone()]);

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
    let task = super::make_task(
        "checks.clippy",
        &[
            "nix",
            "build",
            ".#checks.x86_64-linux.cuenv-clippy",
            "-L",
            "--accept-flake-config",
        ],
    );
    let ir = super::make_ir(vec![task.clone()]);

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
    let mut task = super::make_task("security.audit", &["echo first && echo second"]);
    task.shell = true;
    let ir = super::make_ir(vec![task.clone()]);

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

    let mut bootstrap_task = super::make_phase_task(
        "cuenv:contributor:nix.install",
        &["install nix"],
        TaskPhase::new(BuildStage::Bootstrap, 0),
    );
    bootstrap_task.label = Some("Install Nix".to_string());
    bootstrap_task.contributor = Some("nix".to_string());

    let mut cuenv_setup = super::make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    cuenv_setup.label = Some("Setup cuenv".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let mut onepassword_setup = super::make_phase_task(
        "cuenv:contributor:1password.setup",
        &["cuenv secrets setup onepassword"],
        TaskPhase::new(BuildStage::Setup, 20),
    );
    onepassword_setup.label = Some("Setup 1Password".to_string());
    onepassword_setup.contributor = Some("1password".to_string());
    onepassword_setup.depends_on = vec!["cuenv:contributor:cuenv.setup".to_string()];

    let task = super::make_task(
        "checks.nextest",
        &[
            "nix",
            "build",
            ".#checks.x86_64-linux.cuenv-nextest",
            "-L",
            "--accept-flake-config",
        ],
    );
    let ir = super::make_ir(vec![
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
        TaskPhase::new(BuildStage::Setup, 5),
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

    let ir = super::make_ir(vec![
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
            },
        )
        .expect("expected cuenv bootstrap job");
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert!(step_names.contains(&"Checkout"));
    assert!(step_names.contains(&"Install Nix"));
    assert!(step_names.contains(&"Setup Cachix"));
    assert!(step_names.contains(&"Build cuenv (nix)"));
    assert!(!step_names.contains(&"Setup 1Password"));
    assert!(
        job.steps
            .iter()
            .all(|step| step.uses.as_deref() != Some("actions/upload-artifact@v4"))
    );
}

#[test]
fn test_build_cuenv_bootstrap_job_respects_disabled_cuenv_build() {
    let emitter = GitHubActionsEmitter::new().without_cuenv_build();

    let mut cuenv_setup = super::make_phase_task(
        "cuenv:contributor:cuenv.setup",
        &["nix build .#cuenv"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    cuenv_setup.label = Some("Build cuenv (nix)".to_string());
    cuenv_setup.contributor = Some("cuenv".to_string());

    let ir = super::make_ir(vec![cuenv_setup]);

    assert!(
        emitter
            .build_cuenv_bootstrap_job(
                &ir,
                CuenvBootstrapJobOptions {
                    runs_on: RunsOn::Label("ubuntu-latest".to_string()),
                    name: "build.cuenv",
                }
            )
            .is_none()
    );
}

#[test]
fn test_build_simple_job_nix_package_setup_skips_only_cuenv_setup() {
    let emitter = GitHubActionsEmitter::new();

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

    let task = super::make_task("publish.github", &["gh", "release", "upload"]);
    let ir = super::make_ir(vec![
        bootstrap_task,
        cachix_setup,
        cuenv_setup,
        onepassword_setup,
        task.clone(),
    ]);

    let job = emitter.build_simple_job(
        &task,
        &ir,
        &SimpleJobOptions::orchestrated(None, None).with_cuenv_setup(CuenvSetup::RealizeNixPackage),
    );
    let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_deref()).collect();

    assert_eq!(
        step_names,
        [
            "Checkout",
            "Install Nix",
            "Setup Cachix",
            "Add cuenv to PATH",
            "Setup 1Password",
            "publish.github",
        ]
    );

    let path_step = job
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("Add cuenv to PATH"))
        .expect("missing cuenv path step");
    let script = path_step.run.as_deref().expect("missing setup script");
    assert!(script.contains("nix build .#cuenv --no-link --print-out-paths"));
    assert!(script.contains("echo \"$cuenv_package/bin\" >> \"$GITHUB_PATH\""));
    assert!(!script.contains("cmp "));
}

#[test]
fn nix_package_setup_precedes_early_cuenv_dependent() {
    let mut setup = super::make_phase_task(
        "cuenv.setup",
        &["nix build"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    setup.contributor = Some("cuenv".to_string());

    let mut dependent = super::make_phase_task(
        "needs.cuenv",
        &["cuenv sync"],
        TaskPhase::new(BuildStage::Setup, 5),
    );
    dependent.label = Some("Needs cuenv".to_string());
    dependent.depends_on.push("cuenv.setup".to_string());

    let task = super::make_task("test", &["true"]);
    let ir = super::make_ir(vec![setup, dependent, task.clone()]);
    let job = GitHubActionsEmitter::new().build_simple_job(
        &task,
        &ir,
        &SimpleJobOptions::orchestrated(None, None).with_cuenv_setup(CuenvSetup::RealizeNixPackage),
    );
    let names: Vec<_> = job
        .steps
        .iter()
        .filter_map(|step| step.name.as_deref())
        .collect();
    assert_eq!(
        names,
        ["Checkout", "Add cuenv to PATH", "Needs cuenv", "test"]
    );
}

#[cfg(unix)]
#[test]
fn nix_cuenv_setup_accepts_byte_different_builds_of_the_store_path()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let fake_bin = temp.path().join("fake tools");
    let package = temp.path().join("realized package");
    let runner_temp = temp.path().join("runner temp");
    // The `.#cuenv` derivation is not bit-reproducible, so the bootstrap job's
    // build can differ from the copy this runner realizes for the same store path.
    let bootstrap_build = runner_temp.join("cuenv-bootstrap");
    let github_path = temp.path().join("github-path");
    std::fs::create_dir_all(&fake_bin)?;
    std::fs::create_dir_all(package.join("bin"))?;
    std::fs::create_dir_all(&bootstrap_build)?;
    std::fs::write(package.join("bin/cuenv"), b"build from the binary cache")?;
    std::fs::write(
        bootstrap_build.join("cuenv"),
        b"build from the bootstrap job",
    )?;

    let fake_nix = fake_bin.join("nix");
    std::fs::write(
        &fake_nix,
        "#!/bin/sh\n\
         [ \"$*\" = 'build .#cuenv --no-link --print-out-paths -L --accept-flake-config' ] || exit 97\n\
         [ \"$TEST_NIX_MODE\" != fail ] || exit 42\n\
         printf '%s\\n' \"$TEST_CUENV_PACKAGE\"\n",
    )?;
    std::fs::set_permissions(&fake_nix, std::fs::Permissions::from_mode(0o755))?;

    let mut setup = super::make_phase_task(
        "cuenv.setup",
        &["nix build"],
        TaskPhase::new(BuildStage::Setup, 10),
    );
    setup.contributor = Some("cuenv".to_string());
    let task = super::make_task("test", &["true"]);
    let ir = super::make_ir(vec![setup, task.clone()]);
    let job = GitHubActionsEmitter::new().build_simple_job(
        &task,
        &ir,
        &SimpleJobOptions::orchestrated(None, None).with_cuenv_setup(CuenvSetup::RealizeNixPackage),
    );
    let script = job
        .steps
        .iter()
        .find(|step| step.name.as_deref() == Some("Add cuenv to PATH"))
        .and_then(|step| step.run.as_deref())
        .ok_or_else(|| std::io::Error::other("missing cuenv setup script"))?;

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin).chain(std::env::split_paths(&existing_path)),
    )?;
    let run = |mode: &str| {
        std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("PATH", &path)
            .env("RUNNER_TEMP", &runner_temp)
            .env("GITHUB_PATH", &github_path)
            .env("TEST_CUENV_PACKAGE", &package)
            .env("TEST_NIX_MODE", mode)
            .output()
    };

    let success = run("ok")?;
    assert!(
        success.status.success(),
        "byte-different builds of one store path must not fail the job: {}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&github_path)?,
        format!("{}\n", package.join("bin").display())
    );

    std::fs::write(&github_path, "")?;
    assert!(
        !run("fail")?.status.success(),
        "failed realization must fail the job"
    );
    assert_eq!(std::fs::read_to_string(&github_path)?, "");
    Ok(())
}
