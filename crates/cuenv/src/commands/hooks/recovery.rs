//! Inspect and recover hook reservations after the operator drains a run.

use cuenv_core::{Error, Result};
use cuenv_hooks::{HookRecoveryOutcome, HookRunIdentity, StateManager};
use std::fmt::Write;
use std::path::PathBuf;

/// Arguments for listing or recovering hook reservations.
#[derive(Clone, Debug)]
pub struct HookRecoveryRequest {
    /// List reservations instead of recovering one.
    pub list: bool,
    /// Directory identity from the reservation listing.
    pub directory_key: Option<String>,
    /// Instance identity from the reservation listing.
    pub instance_hash: Option<String>,
    /// Run UUID from the reservation listing.
    pub run_id: Option<String>,
    /// Attests that the run and its descendants have been drained.
    pub drained: bool,
}

/// List durable reservations or recover one exact, externally drained run.
///
/// # Errors
///
/// Returns an error for invalid arguments, inaccessible or corrupt state, or
/// an identity that no longer matches the reservation.
pub async fn execute_env_recover(request: &HookRecoveryRequest) -> Result<String> {
    if request.list {
        if request.directory_key.is_some()
            || request.instance_hash.is_some()
            || request.run_id.is_some()
            || request.drained
        {
            return Err(Error::configuration(
                "--list cannot be combined with recovery arguments",
            ));
        }
        let manager = state_manager()?;
        let mut reservations = manager.reservations().await?;
        if reservations.is_empty() {
            return Ok("No hook reservations require recovery.".to_string());
        }
        reservations.sort_by(|left, right| {
            (
                &left.identity.directory_key,
                &left.identity.instance_hash,
                left.identity.run_id,
            )
                .cmp(&(
                    &right.identity.directory_key,
                    &right.identity.instance_hash,
                    right.identity.run_id,
                ))
        });
        let mut output = String::new();
        for reservation in reservations {
            writeln!(
                output,
                "directory: {:?}\ndirectory-key: {}\ninstance-hash: {}\nrun-id: {}\nphase: {:?}\nworker-pid: {}\nchild-pid: {}\n",
                reservation.directory,
                reservation.identity.directory_key,
                reservation.identity.instance_hash,
                reservation.identity.run_id,
                reservation.phase,
                reservation.worker_pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                reservation.child_pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
            )
            .map_err(|error| Error::configuration(format!("Cannot format hook reservation: {error}")))?;
        }
        return Ok(output.trim_end().to_string());
    }

    let (Some(directory_key), Some(instance_hash), Some(run_id), true) = (
        request.directory_key.as_ref(),
        request.instance_hash.as_ref(),
        request.run_id.as_ref(),
        request.drained,
    ) else {
        return Err(Error::configuration(
            "Recovery requires --directory-key, --instance-hash, --run-id, and --drained",
        ));
    };
    let run_id = uuid::Uuid::parse_str(run_id)
        .map_err(|error| Error::configuration(format!("Invalid hook run UUID: {error}")))?;
    let identity = HookRunIdentity {
        directory_key: directory_key.clone(),
        instance_hash: instance_hash.clone(),
        run_id,
    };
    let manager = state_manager()?;
    match manager.recover_drained(&identity).await? {
        HookRecoveryOutcome::Recovered => Ok(format!(
            "Recovered drained hook reservation {}.",
            identity.run_id
        )),
        HookRecoveryOutcome::NotFound => Err(Error::configuration(
            "Hook reservation not found; list reservations again before retrying",
        )),
        HookRecoveryOutcome::Stale => Err(Error::configuration(
            "Hook reservation changed; list reservations again before retrying",
        )),
    }
}

pub(super) fn state_manager() -> Result<StateManager> {
    match std::env::var_os("CUENV_STATE_DIR") {
        Some(path) => Ok(StateManager::new(PathBuf::from(path))),
        None => Ok(StateManager::with_default_dir()?),
    }
}
