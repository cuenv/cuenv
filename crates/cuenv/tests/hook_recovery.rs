//! CLI recovery requires an explicit, exact drained-run identity.

use cuenv_hooks::{ExecutionStatus, HookExecutionIdentity, HookExecutionState, StateManager};
use std::error::Error;
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn recover_command(directory: &TempDir, arguments: &[&str]) -> TestResult<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env("XDG_STATE_HOME", directory.path().join("xdg-state"))
        .env("CUENV_STATE_DIR", directory.path().join("hook-state"))
        .args(["env", "recover"])
        .args(arguments)
        .output()?)
}

#[test]
fn listing_does_not_require_a_cue_module() -> TestResult {
    let directory = TempDir::new()?;
    let output = recover_command(&directory, &["--list"])?;
    assert!(
        output.status.success(),
        "listing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("No hook reservations"));
    Ok(())
}

#[test]
fn recovery_requires_an_explicit_drain_attestation() -> TestResult {
    let directory = TempDir::new()?;
    let output = recover_command(
        &directory,
        &[
            "--directory-key",
            "directory",
            "--instance-hash",
            "instance",
            "--run-id",
            "00000000-0000-0000-0000-000000000000",
        ],
    )?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--drained"));
    assert!(!directory.path().join("hook-state").exists());
    Ok(())
}

#[test]
fn listing_cannot_be_combined_with_recovery() -> TestResult {
    let directory = TempDir::new()?;
    let output = recover_command(&directory, &["--list", "--drained"])?;
    assert!(!output.status.success());
    assert!(!directory.path().join("hook-state").exists());
    Ok(())
}

#[tokio::test]
async fn recovery_requires_the_current_run_identity_even_after_directory_removal() -> TestResult {
    let directory = TempDir::new()?;
    let project = directory.path().join("project");
    std::fs::create_dir(&project)?;
    let manager = StateManager::new(directory.path().join("hook-state"));
    let mut state = HookExecutionState::new(
        HookExecutionIdentity {
            directory_path: project.clone(),
            instance_hash: String::new(),
            config_hash: "config".to_string(),
            session_id: uuid::Uuid::new_v4(),
        },
        Vec::new(),
    );
    let identity = manager.begin_run(&mut state).await?;
    std::fs::remove_dir(&project)?;

    let listing = recover_command(&directory, &["--list"])?;
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.contains(&identity.directory_key));
    assert!(listing.contains(&identity.instance_hash));
    assert!(listing.contains(&identity.run_id.to_string()));

    let stale_run = uuid::Uuid::new_v4().to_string();
    let arguments = [
        "--directory-key",
        identity.directory_key.as_str(),
        "--instance-hash",
        identity.instance_hash.as_str(),
        "--run-id",
        stale_run.as_str(),
        "--drained",
    ];
    let stale = recover_command(&directory, &arguments)?;
    assert!(!stale.status.success());
    assert_eq!(manager.reservations().await?.len(), 1);

    let run_id = identity.run_id.to_string();
    let arguments = [
        "--directory-key",
        identity.directory_key.as_str(),
        "--instance-hash",
        identity.instance_hash.as_str(),
        "--run-id",
        run_id.as_str(),
        "--drained",
    ];
    let recovered = recover_command(&directory, &arguments)?;
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(manager.reservations().await?.is_empty());
    let state = manager
        .load_state(&identity.instance_hash)
        .await?
        .expect("recovered state");
    assert_eq!(state.status, ExecutionStatus::Cancelled);
    Ok(())
}

#[cfg(unix)]
#[test]
fn background_supervisor_accepts_a_non_utf8_state_directory() -> TestResult {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = TempDir::new()?;
    let project = directory.path().join("project");
    std::fs::create_dir_all(project.join("cue.mod"))?;
    std::fs::write(
        project.join("cue.mod/module.cue"),
        "module: \"test.example/native-state\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    std::fs::write(
        project.join("env.cue"),
        "package cuenv\nname: \"native-state\"\nhooks: onEnter: example: {command: \"sh\", args: [\"-c\", \"printf done\"]}\n",
    )?;
    let state_dir = directory
        .path()
        .join(OsString::from_vec(b"state-\xff".to_vec()));
    let session = uuid::Uuid::new_v4().to_string();
    let run = |args: &[&str]| -> TestResult<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(&project)
            .env("HOME", directory.path())
            .env("XDG_STATE_HOME", directory.path().join("xdg-state"))
            .env("CUENV_STATE_DIR", &state_dir)
            .env("CUENV_HOOK_SESSION", &session)
            .env(
                "CUENV_APPROVAL_FILE",
                directory.path().join("approvals.json"),
            )
            .env("CUENV_EXECUTABLE", env!("CARGO_BIN_EXE_cuenv"))
            .args(args)
            .output()?)
    };

    for args in [
        &["allow", "--yes"][..],
        &["env", "load"][..],
        &[
            "env",
            "status",
            "--wait",
            "--timeout",
            "10",
            "--output",
            "short",
        ][..],
        &["env", "inspect"][..],
        &["export", "--shell", "bash"][..],
    ] {
        let output = run(args)?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args.get(1) == Some(&"status") {
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[OK]");
        } else if args.get(1) == Some(&"inspect") {
            assert!(String::from_utf8_lossy(&output.stdout).contains("State file:"));
        }
    }
    Ok(())
}
