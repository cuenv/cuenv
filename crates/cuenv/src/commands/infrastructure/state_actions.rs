//! The `state` subcommands and lock maintenance: remove, recover, unlock
//! and the retired `state adopt`.

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    BackendMismatch, MIGRATION_FENCE_PREFIX, RecoverOptions, RecoverOverrides, UnrecordedStore,
    strip_control_characters,
};

use super::action::FenceRelease;

use super::dispatch::CommandContext;
use super::failure::failure;
use super::guard::tenant_label;
use super::lock::under_lock;
use super::output::{Finding, LockOutcome, LockReport, OtherEnvironment};
use crate::cli::{CliError, InfrastructureFailureKind, LockStatus};

/// `state remove`: forget one resource under the lock (a fenced delete).
pub(super) async fn remove_resource(
    context: &CommandContext<'_>,
    address: &str,
) -> Result<(), CliError> {
    under_lock(context, "state remove", |lock| async move {
        // The addresses alone are read, so a record whose content cannot be
        // decoded can still be named and forgotten.
        let recorded = context
            .store
            .addresses(context.tenant)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        let Some(recorded_address) = recorded
            .iter()
            .find(|candidate| candidate.to_string() == address)
        else {
            let known: Vec<String> = recorded
                .iter()
                .map(|candidate| strip_control_characters(&candidate.to_string()))
                .collect();
            return Err(CliError::config_with_help(
                format!(
                    "{} has no managed resource {} in state",
                    tenant_label(context.tenant),
                    strip_control_characters(address)
                ),
                if known.is_empty() {
                    "No resources are recorded for this project.".to_string()
                } else {
                    format!("Recorded resources: {}.", known.join(", "))
                },
            ));
        };
        context
            .store
            .delete(context.tenant, &lock, recorded_address)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.output.removed(context.tenant, recorded_address);
        Ok(())
    })
    .await
}

/// `state recover`: record every unrecorded change of the tenant under the
/// lock, deleting each saved file once it is recorded. With nothing to
/// recover, the lock is not taken.
pub(super) async fn recover(
    context: &CommandContext<'_>,
    unrecorded: &UnrecordedStore,
    overrides: RecoverOverrides,
) -> Result<(), CliError> {
    if !unrecorded
        .has_pending(context.tenant)
        .map_err(|error| failure(&error, context.invocation))?
    {
        context.output.recovered(context.tenant, &[]);
        context
            .output
            .other_environments(&pending_in_other_environments(context, unrecorded));
        return Ok(());
    }
    let result = under_lock(context, "state recover", |lock| async move {
        if overrides.backend == BackendMismatch::Accept {
            let pending = unrecorded
                .list(context.tenant)
                .map_err(|error| failure(&error, context.invocation))?;
            for record in pending
                .iter()
                .filter(|record| unrecorded.differs_from_backend(record))
            {
                emit_stderr!(format!(
                    "warning: recording {} from {} although it was saved for a different state \
                     backend than the one in use",
                    record.record.address,
                    strip_control_characters(&record.file.display().to_string())
                ));
            }
        }
        let recovered = unrecorded
            .recover(
                context.store.as_ref(),
                context.tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides,
                },
            )
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.output.recovered(context.tenant, &recovered);
        Ok(())
    })
    .await;
    context
        .output
        .other_environments(&pending_in_other_environments(context, unrecorded));
    result
}

/// What a run without `--env` says about other environments of the project
/// that have unrecorded changes saved: `state recover` only looks at the
/// identity it was given, and exiting quietly would leave them unnoticed.
pub(super) fn pending_in_other_environments(
    context: &CommandContext<'_>,
    unrecorded: &UnrecordedStore,
) -> Vec<OtherEnvironment> {
    context
        .siblings
        .declared()
        .into_iter()
        .filter(|(_, tenant)| unrecorded.has_pending(tenant).unwrap_or(false))
        .map(|(name, _)| OtherEnvironment {
            command: context
                .invocation
                .with_environment(Some(&name))
                .command("state recover"),
            environment: name,
            finding: Finding::UnrecordedChanges,
        })
        .collect()
}

/// What `state list` without `--env` says about the project's environments:
/// state is recorded per environment, so a list of the identity without
/// `--env` is silent about them. Every environment with records is named;
/// a project that declares environments and no top-level `resources`
/// manages nothing without `--env`, so there every environment is named.
pub(super) async fn resources_in_other_environments(
    context: &CommandContext<'_>,
) -> Vec<OtherEnvironment> {
    let environments_only =
        context.siblings.top_level.resources == 0 && context.siblings.top_level.data_sources == 0;
    let mut found = Vec::new();
    for (name, tenant) in context.siblings.declared() {
        let count = context
            .store
            .addresses(&tenant)
            .await
            .map_or(0, |addresses| addresses.len());
        if count > 0 || environments_only {
            found.push(OtherEnvironment {
                command: context
                    .invocation
                    .with_environment(Some(&name))
                    .command("state list"),
                environment: name,
                finding: Finding::Resources(count),
            });
        }
    }
    found
}

/// `state adopt`: retired. Project names are unique, so a project's state
/// follows it to any directory; there is no owner to transfer. Kept so
/// scripts and policies naming it keep working; it changes nothing.
pub(super) fn adopt(context: &CommandContext<'_>) {
    context.output.adoption_retired(context.tenant);
}

/// `unlock`: show the lock, or release the one named. Naming another lock
/// than the one held is refused, and so is naming a lock when none is held
/// (the identifier belongs to another environment, or to a lock that is
/// already gone): `unlock <id>` that matches no lock must not exit
/// successfully, or a script cannot tell it released nothing.
pub(super) async fn unlock(
    context: &CommandContext<'_>,
    lock_identifier: Option<&str>,
    fence: FenceRelease,
) -> Result<(), CliError> {
    let tenant = context.tenant;
    let current = context
        .store
        .current_lock(tenant)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    // What an object store adds (the cuenv version, the last change), for
    // whoever decides whether the run is gone. Best effort.
    let details = match (&current, context.store.object_features()) {
        (Some(_), Some(features)) => features.lock_details(tenant).await.ok().flatten(),
        _ => None,
    };
    let (Some(current), Some(lock_identifier)) = (current.as_ref(), lock_identifier) else {
        if let Some(lock_identifier) = lock_identifier {
            return Err(unmatched_lock(context, lock_identifier).await);
        }
        context.output.lock(&LockReport {
            tenant,
            lock: current.as_ref(),
            outcome: LockOutcome::Shown,
            unlock_command: current.as_ref().map(|lock| {
                context.invocation.command(&format!(
                    "unlock {}",
                    strip_control_characters(&lock.lock_identifier)
                ))
            }),
            details: details.as_ref(),
        });
        context
            .output
            .other_environments(&locks_in_other_environments(context).await);
        return Ok(());
    };
    if lock_identifier.starts_with(MIGRATION_FENCE_PREFIX) && fence == FenceRelease::Refuse {
        return Err(CliError::config_with_help(
            format!(
                "{} is a migration fence: its state was migrated to another backend, and the \
                 lock keeps writes off this copy; nothing was released",
                strip_control_characters(lock_identifier)
            ),
            format!(
                "To undo the migration, run `{}`. To release the fence anyway (writes here will \
                 not reach the migrated state), add --migration-fence.",
                context.invocation.command("state migrate --rollback")
            ),
        ));
    }
    if let Some(details) = &details {
        emit_stderr!(format!(
            "Releasing lock {} held by '{}' since {}{}, taken by cuenv {}; last change: {}",
            strip_control_characters(lock_identifier),
            strip_control_characters(&current.holder),
            strip_control_characters(&current.acquired_at),
            current
                .age_description()
                .map_or_else(String::new, |age| format!(" ({age} ago)")),
            details.cuenv_version,
            details.last_change.as_deref().unwrap_or("none")
        ));
    }
    let released = context
        .store
        .force_unlock(tenant, lock_identifier)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    if !released {
        // The lock is held, just not by the run the operator named: that
        // is concurrent activity, not a configuration mistake.
        let holder = strip_control_characters(&current.lock_identifier);
        return Err(CliError::infrastructure(
            format!(
                "{} is locked by {holder}, not {}; nothing was released",
                tenant_label(tenant),
                strip_control_characters(lock_identifier)
            ),
            Some(format!(
                "Run `{}` without an identifier to see who holds the lock now.",
                context.invocation.command("unlock")
            )),
            InfrastructureFailureKind::Locked,
        )
        .with_lock(LockStatus {
            identifier: holder,
            released: false,
        }));
    }
    context.output.lock(&LockReport {
        tenant,
        lock: Some(current),
        outcome: LockOutcome::Released,
        unlock_command: None,
        details: details.as_ref(),
    });
    Ok(())
}

/// The error for `unlock <identifier>` when no lock is held: it says where
/// the identifier may belong instead.
pub(super) async fn unmatched_lock(
    context: &CommandContext<'_>,
    lock_identifier: &str,
) -> CliError {
    let identifier = strip_control_characters(lock_identifier);
    let mut elsewhere = Vec::new();
    for (name, tenant) in context.siblings.declared() {
        let held = context.store.current_lock(&tenant).await.ok().flatten();
        if held.is_some_and(|lock| lock.lock_identifier == lock_identifier) {
            elsewhere.push(format!(
                "`{}`",
                context
                    .invocation
                    .with_environment(Some(&name))
                    .command(&format!("unlock {identifier}"))
            ));
        }
    }
    let help = if elsewhere.is_empty() {
        format!(
            "The lock may have been released already, or belong to another environment or \
             project (locks are per environment); run `{}` to see the lock this selection \
             holds, or `{}` to list every lock in the database.",
            context.invocation.command("unlock"),
            context.invocation.command("state locks")
        )
    } else {
        format!(
            "That lock is held by another environment of the project: run {}.",
            elsewhere.join(" or ")
        )
    };
    CliError::config_with_help(
        format!(
            "{} is not locked by {identifier}; nothing was released",
            tenant_label(context.tenant)
        ),
        help,
    )
}

/// What a run without `--env` says about other environments of the project
/// that hold a lock: `unlock` only looks at the identity it was given.
pub(super) async fn locks_in_other_environments(
    context: &CommandContext<'_>,
) -> Vec<OtherEnvironment> {
    let mut found = Vec::new();
    for (name, tenant) in context.siblings.declared() {
        if let Ok(Some(lock)) = context.store.current_lock(&tenant).await {
            let identifier = strip_control_characters(&lock.lock_identifier);
            found.push(OtherEnvironment {
                command: context
                    .invocation
                    .with_environment(Some(&name))
                    .command(&format!("unlock {identifier}")),
                environment: name,
                finding: Finding::Locked {
                    lock_identifier: identifier,
                    holder: strip_control_characters(&lock.holder),
                },
            });
        }
    }
    found
}
