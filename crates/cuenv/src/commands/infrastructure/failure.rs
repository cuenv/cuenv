//! Turning infrastructure errors into CLI errors with remedies.

use cuenv_infrastructure::{
    InfrastructureError, TenantLock, UnrecordedFileProblem, strip_control_characters,
    strip_control_characters_except_newlines,
};

use super::invocation::Invocation;
use crate::cli::{CliError, InfrastructureFailureKind, LockStatus};

/// Text from a provider or the store, for display: redacted from the raw
/// text first and then stripped of control characters except newlines
/// (stripping first would change a secret that contains one).
pub(super) fn printable_text(text: &str) -> String {
    strip_control_characters_except_newlines(&cuenv_events::redact(text))
}

/// Map an engine or store error to the command's error, with the help an
/// operator needs. Every command the help names carries the run's `--env`,
/// `-p` and `--package`.
pub(super) fn failure(error: &InfrastructureError, invocation: &Invocation) -> CliError {
    let message = printable_text(&error.to_string());
    let (kind, help) = match error {
        InfrastructureError::Configuration(_) => return CliError::config(message),
        InfrastructureError::Locked {
            lock_identifier, ..
        } => {
            return CliError::infrastructure(
                message,
                Some(format!(
                    "Wait for the other run to finish, then run the command again. If that run \
                     is gone, release the lock with `{}`.",
                    invocation.command(&format!(
                        "unlock {}",
                        strip_control_characters(lock_identifier)
                    ))
                )),
                InfrastructureFailureKind::Locked,
            )
            .with_lock(LockStatus {
                identifier: strip_control_characters(lock_identifier),
                released: false,
            });
        }
        InfrastructureError::LockLost { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Another run released or took this project's lock. Review `{}` before applying \
                 again.",
                invocation.command("plan")
            )),
        ),
        InfrastructureError::UnrecordedChangeLost { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "The provider may have changed an object, but cuenv could not record or save its state. Inspect the object and state store before retrying the operation."
                    .to_string(),
            ),
        ),
        InfrastructureError::Interrupted { .. }
        | InfrastructureError::InterruptedWhilePlanning
        | InfrastructureError::InterruptedUnknownOutcome { .. } => {
            (InfrastructureFailureKind::Interrupted, None)
        }
        InfrastructureError::State(_) => (
            InfrastructureFailureKind::Failed,
            Some(
                "Check the state backend settings (`infrastructure.state`), its authentication \
                 token and network access."
                    .to_string(),
            ),
        ),
        // A local file problem, never a state store one.
        InfrastructureError::UnrecordedFile { path, kind, .. } => (
            InfrastructureFailureKind::Failed,
            Some(unrecorded_file_help(
                &strip_control_characters(path),
                *kind,
                invocation,
            )),
        ),
        InfrastructureError::StateChanged { address, .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "The state store's record of {} changed after the unrecorded change was saved, \
                 so recording it would overwrite the newer record. Review the resource, then \
                 either run `{}` to write the saved record anyway (this does not accept a file \
                 saved for another backend), or move its file out of the unrecorded change \
                 directory to keep the stored record.",
                strip_control_characters(address),
                invocation.command("state recover --force")
            )),
        ),
        InfrastructureError::StateSchemaNewer { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "The state database was migrated by a newer cuenv. Upgrade cuenv; an older \
                 version never reads or writes it."
                    .to_string(),
            ),
        ),
        InfrastructureError::StateMigrationBlocked { locks, .. } => {
            return migration_blocked(message, locks, invocation);
        }
        InfrastructureError::StateMigrationPending { .. } => {
            return CliError::infrastructure(
                message,
                Some("Run the command again in a moment.".to_string()),
                InfrastructureFailureKind::Locked,
            );
        }
        InfrastructureError::StateUnreleasedLayout { tables } => {
            let listed = if tables.len() == 1 {
                "the table listed above"
            } else {
                "the tables listed above"
            };
            (
                InfrastructureFailureKind::Failed,
                Some(format!(
                    "The database holds tables written by an unreleased development build of \
                     cuenv; no released cuenv reads that layout, and cuenv never adopts or \
                     ignores it. This is not a connection problem. If the state recorded there \
                     is disposable, drop every table whose name starts with \
                     `cuenv_infrastructure` ({listed} and any others of that family) and run \
                     the command again; if it manages real resources, use another database \
                     (`infrastructure.state`) instead."
                )),
            )
        }
        InfrastructureError::StateSchemaConflict { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "The database answered, but its tables do not match what this cuenv expects; \
                 this is not a connection problem, so the state backend settings and token \
                 reach it. Point `infrastructure.state` at a database cuenv created (or an empty \
                 one), or remove the conflicting tables if they are not needed."
                    .to_string(),
            ),
        ),
        InfrastructureError::UndecodableRecord { address, .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "The state store answered, but its record of {} is damaged or was not \
                 written by cuenv; this is not a connection problem, so the state backend \
                 settings and token are fine. {}",
                strip_control_characters(address),
                undecodable_record_remedy(address, invocation)
            )),
        ),
        InfrastructureError::PlanOutdated { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "The stored state changed after the plan was made. Run the command again; it \
                 plans again under the lock."
                    .to_string(),
            ),
        ),
        InfrastructureError::OwnedByAnotherInstance { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Each project's state belongs to one CUE instance (directory and package). If \
                 this instance should manage it now (for example, the project moved), run \
                 `{}` here; otherwise give this project a different `name`.",
                invocation.command("state adopt")
            )),
        ),
        InfrastructureError::StateFromNewerProvider { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "Upgrade the provider in `infrastructure.providers` to the version that wrote \
                 this record, or a newer one; cuenv never downgrades stored state."
                    .to_string(),
            ),
        ),
        InfrastructureError::UnrecordedChange { .. }
        | InfrastructureError::UnrecordedChangesPending { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Run `{}`, then run this command again.",
                invocation.command("state recover")
            )),
        ),
        // Codec, Plugin, RemoteProcedure, Diagnostics, ApplyIncomplete, Install,
        // InputOutput and any future variant: their messages say what to do next,
        // or carry the provider's own diagnostics.
        _ => (InfrastructureFailureKind::Failed, None),
    };
    CliError::infrastructure(message, help, kind)
}

/// What to do about a damaged record: `state remove` forgets it by address
/// (without decoding it), when it has an address.
pub(super) fn undecodable_record_remedy(address: &str, invocation: &Invocation) -> String {
    let address = strip_control_characters(address);
    if address.starts_with('(') {
        return "Inspect or repair that row in the state database; it has no readable address, \
                so `state remove` cannot name it."
            .to_string();
    }
    format!(
        "Forget it with `{}` (the real object is not touched; the next plan creates it \
         again), or repair that row in the state database.",
        invocation.command(&format!("state remove {address}"))
    )
}

/// The help for an unrecorded change file that cannot be used: the remedy
/// fits what is wrong with it. Only a file saved for another state backend
/// can be recovered with `--accept-backend`.
pub(super) fn unrecorded_file_help(
    path: &str,
    kind: UnrecordedFileProblem,
    invocation: &Invocation,
) -> String {
    let move_aside = "Check it, then move it out of that directory and run the command again; \
                      moving it aside discards the change it records from cuenv's view (the \
                      resource itself is not touched).";
    match kind {
        UnrecordedFileProblem::Backend => format!(
            "{path} is in cuenv's unrecorded change directory but was saved for another state \
             backend. {move_aside} If you have checked that this backend is the right one, run \
             `{}` instead.",
            invocation.command("state recover --accept-backend")
        ),
        UnrecordedFileProblem::DevelopmentBuild => format!(
            "{path} was written by an unreleased development build of cuenv, and no released \
             cuenv reads that format. The resource it records may exist: check with the \
             provider's own tools, and with `{}` whether cuenv knows it. {move_aside}",
            invocation.command("state list")
        ),
        UnrecordedFileProblem::Format => format!(
            "{path} is in cuenv's unrecorded change directory but is not a file this cuenv can \
             read (another format, damaged, or not written by cuenv). {move_aside}"
        ),
        UnrecordedFileProblem::Other => format!(
            "{path} is in cuenv's unrecorded change directory but cannot be used. {move_aside}"
        ),
    }
}

/// How many blocking locks a migration error names commands for.
pub(super) const LISTED_UNLOCK_COMMANDS: usize = 5;

/// A migration refused because locks are held: exit code 4, with every
/// blocking lock in the message and in the JSON `blockingLocks`, and the
/// commands that release them, which need no project of that name to
/// evaluate.
pub(super) fn migration_blocked(
    message: String,
    locks: &[TenantLock],
    invocation: &Invocation,
) -> CliError {
    let mut help = format!(
        "Wait for the other `cuenv infrastructure` commands to finish, then run the command \
         again. If a run died and left its lock behind, `{}` lists every lock in the database, \
         whichever project holds it.",
        invocation.command("state locks")
    );
    if !locks.is_empty() {
        let commands: Vec<String> = locks
            .iter()
            .take(LISTED_UNLOCK_COMMANDS)
            .map(|held| format!("`{}`", invocation.unlock_command(held)))
            .collect();
        help = format!(
            "{help} After checking that its run is gone, release a lock with {}.",
            commands.join(" or ")
        );
    }
    let blocking: Vec<serde_json::Value> = locks
        .iter()
        .map(|held| {
            serde_json::json!({
                "module": held.module_path,
                "project": held.project,
                "environment": held.environment,
                "lockIdentifier": held.lock.lock_identifier,
                "holder": held.lock.holder,
                "acquiredAt": held.lock.acquired_at,
                "unlockCommand": invocation.unlock_command(held),
            })
        })
        .collect();
    CliError::infrastructure(message, Some(help), InfrastructureFailureKind::Locked)
        .with_detail("blockingLocks", serde_json::Value::Array(blocking))
}
