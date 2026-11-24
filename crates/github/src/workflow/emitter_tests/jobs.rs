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
    assert_eq!(job.needs, [] as [String; 0]); // Caller sets needs
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
            .all(|step| step.uses.as_deref() != Some("actions/upload-artifact@v7"))
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

#[test]
fn task_github_job_settings_are_emitted_in_expanded_jobs() {
    use cuenv_ci::emitter::Emitter;
    use cuenv_core::ci::{
        CancelInProgressConfig, PipelineConcurrency, StringOrVec, TaskGitHubContainer,
        TaskGitHubContainerConfig, TaskGitHubDefaults, TaskGitHubEnvironment, TaskGitHubJobConfig,
        TaskGitHubRunDefaults,
    };
    use std::collections::BTreeMap;

    let config = crate::config::GitHubConfig {
        timeout_minutes: Some(30),
        ..Default::default()
    };
    let emitter = GitHubActionsEmitter::from_config(&config)
        .without_nix()
        .without_cuenv_build();

    let mut deploy = super::make_task("deploy.docs", &["deploy"]);
    deploy.github_job = Some(TaskGitHubJobConfig {
        runner: Some(StringOrVec::Vec(vec![
            "namespace-profile-builder".to_string(),
        ])),
        concurrency: Some(PipelineConcurrency {
            group: Some("deploy-prod".to_string()),
            cancel_in_progress: Some(CancelInProgressConfig::Bool(false)),
        }),
        timeout_minutes: Some(45),
        environment: Some(TaskGitHubEnvironment::Detailed {
            name: "production".to_string(),
            url: Some("https://docs.intar.dev".to_string()),
        }),
        container: Some(TaskGitHubContainer::Detailed(TaskGitHubContainerConfig {
            image: "mcr.microsoft.com/playwright:v1.64.1-noble@sha256:deadbeef".to_string(),
            options: Some("--ipc=host".to_string()),
            env: BTreeMap::from([("CI".to_string(), "1".to_string())]),
            volumes: vec!["/tmp:/tmp".to_string()],
        })),
        defaults: Some(TaskGitHubDefaults {
            run: Some(TaskGitHubRunDefaults {
                working_directory: Some("docs".to_string()),
                shell: Some("bash".to_string()),
            }),
        }),
        if_condition: Some("always()".to_string()),
        ..TaskGitHubJobConfig::default()
    });

    // A second task with image-only container & string environment inherits pipeline timeout.
    let mut smoke = super::make_task("smoke", &["test"]);
    smoke.github_job = Some(TaskGitHubJobConfig {
        runner: Some(StringOrVec::String("${{ matrix.runner }}".to_string())),
        environment: Some(TaskGitHubEnvironment::Name("staging".to_string())),
        container: Some(TaskGitHubContainer::Detailed(TaskGitHubContainerConfig {
            image: "node:22@sha256:cafe".to_string(),
            ..Default::default()
        })),
        ..Default::default()
    });

    let ir = super::make_ir(vec![deploy, smoke]);
    let yaml = emitter.emit_expanded(&ir).unwrap();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();

    let deploy_job = &doc["jobs"]["deploy-docs"];
    assert_eq!(
        deploy_job["runs-on"],
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(
            "namespace-profile-builder".to_string()
        )])
    );
    assert_eq!(
        deploy_job["concurrency"]["group"].as_str(),
        Some("deploy-prod")
    );
    assert_eq!(
        deploy_job["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false)
    );
    assert_eq!(deploy_job["timeout-minutes"].as_u64(), Some(45));
    assert_eq!(
        deploy_job["environment"]["name"].as_str(),
        Some("production")
    );
    assert_eq!(
        deploy_job["environment"]["url"].as_str(),
        Some("https://docs.intar.dev")
    );
    assert_eq!(
        deploy_job["container"]["image"].as_str(),
        Some("mcr.microsoft.com/playwright:v1.64.1-noble@sha256:deadbeef")
    );
    assert_eq!(
        deploy_job["container"]["options"].as_str(),
        Some("--ipc=host")
    );
    assert_eq!(
        deploy_job["defaults"]["run"]["working-directory"].as_str(),
        Some("docs")
    );
    assert_eq!(
        deploy_job["defaults"]["run"]["shell"].as_str(),
        Some("bash")
    );
    assert_eq!(deploy_job["if"].as_str(), Some("always()"));

    let smoke_job = &doc["jobs"]["smoke"];
    assert_eq!(smoke_job["runs-on"].as_str(), Some("${{ matrix.runner }}"));
    assert_eq!(smoke_job["timeout-minutes"].as_u64(), Some(30));
    assert_eq!(smoke_job["environment"].as_str(), Some("staging"));
    // Image-only container serializes as string shorthand.
    assert_eq!(smoke_job["container"].as_str(), Some("node:22@sha256:cafe"));
}

#[test]
fn checkout_custom_steps_and_action_pins_compose_end_to_end() {
    use cuenv_ci::emitter::Emitter;
    use cuenv_core::ci::{
        CheckoutSubmodules, GitHubActionPin, GitHubCheckoutConfig, TaskGitHubJobConfig,
        TaskGitHubStep, TaskGitHubSteps,
    };
    use std::collections::BTreeMap;

    let checkout_sha = "3d3c42e5aac5ba805825da76410c181273ba90b1";
    let upload_sha = "cf430e030ddbb5b0abf93d22962f4752f3646cd9";
    let config = crate::config::GitHubConfig {
        checkout: Some(GitHubCheckoutConfig {
            uses: Some("actions/checkout".to_string()),
            persist_credentials: Some(false),
            fetch_depth: Some(0),
            git_ref: None,
            submodules: Some(CheckoutSubmodules::Mode("recursive".to_string())),
        }),
        pins: Some(BTreeMap::from([
            (
                "actions/checkout".to_string(),
                GitHubActionPin {
                    git_ref: checkout_sha.to_string(),
                    tag: Some("v7".to_string()),
                },
            ),
            (
                "actions/upload-artifact".to_string(),
                GitHubActionPin {
                    git_ref: upload_sha.to_string(),
                    tag: Some("v7".to_string()),
                },
            ),
        ])),
        require_pinned_actions: Some(true),
        ..Default::default()
    };
    let emitter = GitHubActionsEmitter::from_config(&config)
        .without_nix()
        .without_cuenv_build();

    let mut task = super::make_task("web.build", &["bun", "run", "build"]);
    task.github_job = Some(TaskGitHubJobConfig {
        checkout: Some(GitHubCheckoutConfig {
            git_ref: Some("${{ github.event.workflow_run.head_sha }}".to_string()),
            ..Default::default()
        }),
        steps: Some(TaskGitHubSteps {
            before: vec![TaskGitHubStep {
                name: Some("Prepare".to_string()),
                id: Some("prep".to_string()),
                run: Some("echo prep".to_string()),
                working_directory: Some("apps/web".to_string()),
                ..Default::default()
            }],
            after: vec![TaskGitHubStep {
                name: Some("Upload build".to_string()),
                if_condition: Some("always()".to_string()),
                uses: Some("actions/upload-artifact@v7".to_string()),
                with_inputs: BTreeMap::from([(
                    "path".to_string(),
                    serde_json::json!("apps/web/dist"),
                )]),
                ..Default::default()
            }],
        }),
        ..Default::default()
    });

    let ir = super::make_ir(vec![task]);
    let yaml = emitter.emit_expanded(&ir).unwrap();

    // Pinned actions keep their trailing `# <tag>` comment verbatim.
    assert!(yaml.contains(&format!("uses: actions/checkout@{checkout_sha} # v7")));
    assert!(yaml.contains(&format!("uses: actions/upload-artifact@{upload_sha} # v7")));

    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let steps = doc["jobs"]["web-build"]["steps"].as_sequence().unwrap();
    // Checkout merged pipeline + task fields.
    assert_eq!(
        steps[0]["with"]["persist-credentials"].as_bool(),
        Some(false)
    );
    assert_eq!(steps[0]["with"]["fetch-depth"].as_u64(), Some(0));
    assert_eq!(steps[0]["with"]["submodules"].as_str(), Some("recursive"));
    assert_eq!(
        steps[0]["with"]["ref"].as_str(),
        Some("${{ github.event.workflow_run.head_sha }}")
    );
    // Custom before -> task -> custom after ordering.
    assert_eq!(steps[1]["id"].as_str(), Some("prep"));
    assert_eq!(steps[1]["working-directory"].as_str(), Some("apps/web"));
    assert_eq!(steps[2]["name"].as_str(), Some("web.build"));
    assert_eq!(steps[3]["name"].as_str(), Some("Upload build"));
    assert_eq!(steps[3]["if"].as_str(), Some("always()"));

    // requirePinnedActions fails when an unpinned action is emitted.
    let strict_emitter = GitHubActionsEmitter::from_config(&crate::config::GitHubConfig {
        require_pinned_actions: Some(true),
        ..Default::default()
    })
    .without_nix()
    .without_cuenv_build();
    let unpinned_ir = super::make_ir(vec![super::make_task("check", &["cargo", "check"])]);
    assert!(strict_emitter.emit_expanded(&unpinned_ir).is_err());

    // Custom step with both uses and run is rejected.
    let mut invalid_task = super::make_task("bad", &["true"]);
    invalid_task.github_job = Some(TaskGitHubJobConfig {
        steps: Some(TaskGitHubSteps {
            before: vec![TaskGitHubStep {
                uses: Some("actions/checkout@v7".to_string()),
                run: Some("echo both".to_string()),
                ..Default::default()
            }],
            after: vec![],
        }),
        ..Default::default()
    });
    let invalid_ir = super::make_ir(vec![invalid_task]);
    assert!(emitter.emit_expanded(&invalid_ir).is_err());
}

#[test]
fn job_outputs_cross_job_references_and_dynamic_matrix() {
    use cuenv_ci::emitter::Emitter;
    use cuenv_core::ci::{
        StringOrVec, TaskGitHubDynamicMatrix, TaskGitHubJobConfig, TaskGitHubMatrixInclude,
    };
    use std::collections::BTreeMap;

    let emitter = GitHubActionsEmitter::new()
        .with_runner("ubuntu-24.04")
        .without_nix()
        .without_cuenv_build();

    let mut plan = super::make_task("release-plan", &["scripts/plan.sh"]);
    plan.github_job = Some(TaskGitHubJobConfig {
        outputs: vec!["matrix".to_string(), "web_digest".to_string()],
        step_id: Some("plan".to_string()),
        ..Default::default()
    });

    let mut build = super::make_task("release.build", &["scripts/build.sh"]);
    build.depends_on = vec!["release-plan".to_string()];
    build.env.insert(
        "TESTED_DIGEST".to_string(),
        "${{ needs.release-plan.outputs.web_digest }}".to_string(),
    );
    build.github_job = Some(TaskGitHubJobConfig {
        if_condition: Some("needs.release-plan.outputs.matrix != '[]'".to_string()),
        matrix: Some(TaskGitHubDynamicMatrix {
            include: Some(TaskGitHubMatrixInclude::Expression(
                "${{ needs.release-plan.outputs.matrix }}".to_string(),
            )),
            fail_fast: Some(false),
            max_parallel: Some(4),
            runs_on: Some(StringOrVec::String("${{ matrix.runner }}".to_string())),
        }),
        ..Default::default()
    });

    let mut static_matrix = super::make_task("smoke", &["scripts/smoke.sh"]);
    static_matrix.github_job = Some(TaskGitHubJobConfig {
        matrix: Some(TaskGitHubDynamicMatrix {
            include: Some(TaskGitHubMatrixInclude::Static(vec![BTreeMap::from([
                ("target".to_string(), serde_json::json!("x86_64-linux")),
                ("runner".to_string(), serde_json::json!("ubuntu-24.04")),
            ])])),
            fail_fast: Some(true),
            max_parallel: Some(2),
            runs_on: Some(StringOrVec::String("${{ matrix.runner }}".to_string())),
        }),
        ..Default::default()
    });

    let ir = super::make_ir(vec![plan, build, static_matrix]);
    let yaml = emitter.emit_expanded(&ir).expect("emit expanded workflow");
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse workflow yaml");

    let plan_job = &doc["jobs"]["release-plan"];
    assert_eq!(
        plan_job["outputs"]["matrix"].as_str(),
        Some("${{ steps.plan.outputs.matrix }}")
    );
    assert_eq!(
        plan_job["outputs"]["web_digest"].as_str(),
        Some("${{ steps.plan.outputs.web_digest }}")
    );
    assert_eq!(plan_job["steps"][1]["id"].as_str(), Some("plan"));

    let build_job = &doc["jobs"]["release-build"];
    assert_eq!(build_job["runs-on"].as_str(), Some("${{ matrix.runner }}"));
    assert_eq!(
        build_job["if"].as_str(),
        Some("needs.release-plan.outputs.matrix != '[]'")
    );
    assert_eq!(
        build_job["strategy"]["matrix"]["include"].as_str(),
        Some("${{ fromJSON(needs.release-plan.outputs.matrix) }}")
    );
    assert_eq!(build_job["strategy"]["fail-fast"].as_bool(), Some(false));
    assert_eq!(build_job["strategy"]["max-parallel"].as_u64(), Some(4));

    let smoke_job = &doc["jobs"]["smoke"];
    assert_eq!(
        smoke_job["strategy"]["matrix"]["include"][0]["target"].as_str(),
        Some("x86_64-linux")
    );
    assert_eq!(smoke_job["strategy"]["fail-fast"].as_bool(), Some(true));
    assert_eq!(smoke_job["strategy"]["max-parallel"].as_u64(), Some(2));

    // Referencing an undeclared output from an upstream job fails validation.
    let mut bad_consumer = super::make_task("deploy", &["scripts/deploy.sh"]);
    bad_consumer.depends_on = vec!["release-plan".to_string()];
    bad_consumer.env.insert(
        "BAD".to_string(),
        "${{ needs.release-plan.outputs.undeclared }}".to_string(),
    );
    let mut plan_only = super::make_task("release-plan", &["scripts/plan.sh"]);
    plan_only.github_job = Some(TaskGitHubJobConfig {
        outputs: vec!["matrix".to_string()],
        ..Default::default()
    });
    let bad_ir = super::make_ir(vec![plan_only, bad_consumer]);
    let err = emitter
        .emit_expanded(&bad_ir)
        .expect_err("undeclared job output reference must fail");
    assert!(
        err.to_string().contains("undeclared output 'undeclared'"),
        "unexpected error: {err}"
    );
}
