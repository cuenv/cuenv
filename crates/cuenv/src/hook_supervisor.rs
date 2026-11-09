use cuenv::cli::CliError;
use cuenv_hooks::{
    ExecutionStatus, HookExecutionState, HookRunIdentity, HookRunJoin, HookRunUpdate,
    HookWorkerStart, StateManager,
};
use std::ffi::OsString;
use std::path::PathBuf;

struct HookSupervisorArgs {
    state_dir: PathBuf,
    identity: HookRunIdentity,
}

struct SupervisorFailure<'failure> {
    manager: &'failure StateManager,
    identity: &'failure HookRunIdentity,
    state: &'failure mut HookExecutionState,
    message: &'failure str,
}

/// Join and execute one reserved hook run.
pub async fn run_hook_supervisor(args: Vec<OsString>) -> Result<(), CliError> {
    let args = parse_hook_supervisor_args(&args)?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();

    let manager = StateManager::new(args.state_dir);
    let start = match manager
        .join_supervisor_run(&args.identity)
        .await
        .map_err(|error| CliError::other(format!("Failed to join hook run: {error}")))?
    {
        HookRunJoin::Joined(start) => start,
        HookRunJoin::CancelRequested => {
            return acknowledge_cancellation(&manager, &args.identity).await;
        }
        HookRunJoin::Superseded => return Ok(()),
    };

    let HookWorkerStart {
        mut state,
        hooks,
        config,
    } = *start;
    if let Err(error) = std::env::set_current_dir(&state.directory_path) {
        return record_failure(SupervisorFailure {
            manager: &manager,
            identity: &args.identity,
            state: &mut state,
            message: &format!("Failed to enter hook directory: {error}"),
        })
        .await;
    }

    if let Err(error) = cuenv_hooks::execute_hooks(
        hooks,
        &config,
        cuenv_hooks::HookRunState {
            manager: &manager,
            state: &mut state,
            identity: &args.identity,
        },
    )
    .await
    {
        return record_failure(SupervisorFailure {
            manager: &manager,
            identity: &args.identity,
            state: &mut state,
            message: &error.to_string(),
        })
        .await;
    }

    if state.status == ExecutionStatus::Completed {
        return match manager.publish_and_serve(&args.identity, &state).await {
            Ok(HookRunUpdate::Applied | HookRunUpdate::Superseded) => Ok(()),
            Ok(HookRunUpdate::CancelRequested) => {
                acknowledge_cancellation(&manager, &args.identity).await
            }
            Err(error) => {
                record_failure(SupervisorFailure {
                    manager: &manager,
                    identity: &args.identity,
                    state: &mut state,
                    message: &error.to_string(),
                })
                .await
            }
        };
    }

    Ok(())
}

fn parse_hook_supervisor_args(args: &[OsString]) -> Result<HookSupervisorArgs, CliError> {
    let mut state_dir = None;
    let mut directory_key = None;
    let mut instance_hash = None;
    let mut run_id = None;
    let mut arguments = args.iter().skip(2);
    while let Some(flag) = arguments.next() {
        let flag = flag
            .to_str()
            .ok_or_else(|| CliError::other("Supervisor option is not valid UTF-8"))?;
        let value = arguments
            .next()
            .ok_or_else(|| CliError::other(format!("Missing value for {flag}")))?;
        let destination = match flag {
            "--state-dir" => &mut state_dir,
            "--directory-key" => &mut directory_key,
            "--instance-hash" => &mut instance_hash,
            "--run-id" => &mut run_id,
            _ => {
                return Err(CliError::other(format!(
                    "Unknown supervisor argument: {flag}"
                )));
            }
        };
        if destination.replace(value.clone()).is_some() {
            return Err(CliError::other(format!(
                "Repeated supervisor argument: {flag}"
            )));
        }
    }

    let state_dir = PathBuf::from(state_dir.ok_or_else(|| CliError::other("Missing --state-dir"))?);
    let directory_key = utf8_argument(directory_key, "--directory-key")?;
    let instance_hash = utf8_argument(instance_hash, "--instance-hash")?;
    let run_id = utf8_argument(run_id, "--run-id")?;
    let run_id = uuid::Uuid::parse_str(&run_id)
        .map_err(|error| CliError::other(format!("Invalid supervisor run UUID: {error}")))?;
    if state_dir.as_os_str().is_empty()
        || directory_key.is_empty()
        || instance_hash.is_empty()
        || run_id.is_nil()
    {
        return Err(CliError::other("Invalid hook supervisor identity"));
    }

    Ok(HookSupervisorArgs {
        state_dir,
        identity: HookRunIdentity {
            directory_key,
            instance_hash,
            run_id,
        },
    })
}

fn utf8_argument(value: Option<OsString>, flag: &str) -> Result<String, CliError> {
    value
        .ok_or_else(|| CliError::other(format!("Missing {flag}")))?
        .into_string()
        .map_err(|_| CliError::other(format!("{flag} must be valid UTF-8")))
}

async fn acknowledge_cancellation(
    manager: &StateManager,
    identity: &HookRunIdentity,
) -> Result<(), CliError> {
    match manager
        .acknowledge_cancellation(identity)
        .await
        .map_err(|error| CliError::other(format!("Failed to acknowledge cancellation: {error}")))?
    {
        HookRunUpdate::Applied | HookRunUpdate::Superseded => Ok(()),
        HookRunUpdate::CancelRequested => Err(CliError::other(
            "Cannot acknowledge cancellation while a hook child remains active",
        )),
    }
}

async fn record_failure(failure: SupervisorFailure<'_>) -> Result<(), CliError> {
    failure.state.status = ExecutionStatus::Failed;
    failure.state.error_message = Some(failure.message.to_string());
    failure.state.finished_at = Some(chrono::Utc::now());
    match failure
        .manager
        .update_run(failure.identity, failure.state)
        .await
        .map_err(|error| CliError::other(format!("Failed to record hook failure: {error}")))?
    {
        HookRunUpdate::Applied => Err(CliError::other("Hook execution failed")),
        HookRunUpdate::CancelRequested => {
            acknowledge_cancellation(failure.manager, failure.identity).await
        }
        HookRunUpdate::Superseded => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    fn args(values: &[&str]) -> Vec<std::ffi::OsString> {
        std::iter::once("cuenv")
            .chain(std::iter::once("__hook-supervisor"))
            .chain(values.iter().copied())
            .map(std::ffi::OsString::from)
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_accepts_a_non_utf8_state_directory() {
        use std::os::unix::ffi::OsStringExt;

        let state_dir = std::ffi::OsString::from_vec(b"/private/state-\xff".to_vec());
        let mut arguments = args(&[
            "--state-dir",
            "/private/state",
            "--directory-key",
            "directory",
            "--instance-hash",
            "instance",
            "--run-id",
            "43ff7692-a719-4f57-9aad-6808b062a22e",
        ]);
        arguments[3] = state_dir.clone();
        let parsed = super::parse_hook_supervisor_args(&arguments).unwrap();
        assert_eq!(parsed.state_dir, std::path::PathBuf::from(state_dir));
    }

    #[test]
    fn supervisor_requires_a_complete_unique_run_identity() {
        let run_id = "43ff7692-a719-4f57-9aad-6808b062a22e";
        let parsed = super::parse_hook_supervisor_args(&args(&[
            "--state-dir",
            "/private/state",
            "--directory-key",
            "directory",
            "--instance-hash",
            "instance",
            "--run-id",
            run_id,
        ]))
        .unwrap();
        assert_eq!(parsed.identity.directory_key, "directory");
        assert_eq!(parsed.identity.instance_hash, "instance");
        assert_eq!(parsed.identity.run_id.to_string(), run_id);
        assert_eq!(parsed.state_dir, std::path::Path::new("/private/state"));
    }

    #[test]
    fn supervisor_rejects_missing_repeated_and_legacy_arguments() {
        assert!(super::parse_hook_supervisor_args(&args(&["--state-dir"])).is_err());
        assert!(super::parse_hook_supervisor_args(&args(&["--hooks-file", "/tmp/file"])).is_err());
        assert!(
            super::parse_hook_supervisor_args(&args(&[
                "--state-dir",
                "/private/state",
                "--state-dir",
                "/other/state",
            ]))
            .is_err()
        );
        assert!(
            super::parse_hook_supervisor_args(&args(&[
                "--state-dir",
                "/private/state",
                "--directory-key",
                "directory",
                "--instance-hash",
                "instance",
                "--run-id",
                "00000000-0000-0000-0000-000000000000",
            ]))
            .is_err()
        );
    }
}
