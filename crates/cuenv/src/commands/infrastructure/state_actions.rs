//! The `state` subcommands and lock maintenance: remove, recover, unlock
//! and the retired `state adopt`.

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    BackendMismatch, MIGRATION_FENCE_PREFIX, RecoverOptions, RecoverOverrides, UnrecordedStore,
};

use super::action::FenceRelease;

use super::dispatch::CommandContext;

use super::output::{Finding, LockOutcome, LockReport, OtherEnvironment};
use crate::cli::{CliError, InfrastructureFailureKind, LockStatus};

/// `state remove`: forget one resource under the lock (a fenced delete).
pub(super) async fn remove_resource(
    context: &CommandContext<'_>,
    address: &str,
) -> Result<(), CliError> {
    super::lock::under_lock(context, "state remove", |lock| async move {
        // The addresses alone are read, so a record whose content cannot be
        // decoded can still be named and forgotten.
        let recorded = context
            .store
            .addresses(context.tenant)
            .await
            .map_err(|error| super::failure::failure(&error, context.invocation))?;
        let Some(recorded_address) = recorded
            .iter()
            .find(|candidate| candidate.to_string() == address)
        else {
            let known: Vec<String> = recorded
                .iter()
                .map(|candidate| {
                    cuenv_infrastructure::strip_control_characters(&candidate.to_string())
                })
                .collect();
            return Err(CliError::config_with_help(
                format!(
                    "{} has no managed resource {} in state",
                    super::guard::tenant_label(context.tenant),
                    cuenv_infrastructure::strip_control_characters(address)
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
            .map_err(|error| super::failure::failure(&error, context.invocation))?;
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
    let others = pending_in_other_environments(context, unrecorded)?;
    if !unrecorded
        .has_pending(context.tenant)
        .map_err(|error| super::failure::failure(&error, context.invocation))?
    {
        context.output.recovered(context.tenant, &[]);
        context.output.other_environments(&others);
        return Ok(());
    }
    let result = super::lock::under_lock(context, "state recover", |lock| async move {
        if overrides.backend == BackendMismatch::Accept {
            let pending = unrecorded
                .list(context.tenant)
                .map_err(|error| super::failure::failure(&error, context.invocation))?;
            for record in pending
                .iter()
                .filter(|record| unrecorded.differs_from_backend(record))
            {
                emit_stderr!(format!(
                    "warning: recording {} from {} although it was saved for a different state \
                     backend than the one in use",
                    record.record.address,
                    cuenv_infrastructure::strip_control_characters(
                        &record.file.display().to_string()
                    )
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
            .map_err(|error| super::failure::failure(&error, context.invocation))?;
        context.output.recovered(context.tenant, &recovered);
        Ok(())
    })
    .await;
    context.output.other_environments(&others);
    result
}

/// What a run without `--env` says about other environments of the project
/// that have unrecorded changes saved: `state recover` only looks at the
/// identity it was given, and exiting quietly would leave them unnoticed.
pub(super) fn pending_in_other_environments(
    context: &CommandContext<'_>,
    unrecorded: &UnrecordedStore,
) -> Result<Vec<OtherEnvironment>, CliError> {
    let mut found = Vec::new();
    for (name, tenant) in context.siblings.declared()? {
        let invocation = context.invocation.with_environment(Some(&name));
        if unrecorded
            .has_pending(&tenant)
            .map_err(|error| super::failure::failure(&error, &invocation))?
        {
            found.push(OtherEnvironment {
                command: invocation.command("state recover"),
                environment: name,
                finding: Finding::UnrecordedChanges,
            });
        }
    }
    Ok(found)
}

/// What `state list` without `--env` says about the project's environments:
/// state is recorded per environment, so a list of the identity without
/// `--env` is silent about them. Every environment with records is named;
/// a project that declares environments and no top-level `resources`
/// manages nothing without `--env`, so there every environment is named.
pub(super) async fn resources_in_other_environments(
    context: &CommandContext<'_>,
) -> Result<Vec<OtherEnvironment>, CliError> {
    let environments_only =
        context.siblings.top_level.resources == 0 && context.siblings.top_level.data_sources == 0;
    let mut found = Vec::new();
    for (name, tenant) in context.siblings.declared()? {
        let count = context
            .interrupts
            .until_interrupted(context.store.addresses(&tenant))
            .await?
            .map_err(|error| {
                super::failure::failure(&error, &context.invocation.with_environment(Some(&name)))
            })?
            .len();
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
    Ok(found)
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
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    // What an object store adds (the cuenv version, the last change), for
    // whoever decides whether the run is gone. Best effort.
    let details = match (&current, context.store.object_features()) {
        (Some(lock), Some(features)) if lock.identifier().is_some() => {
            features.lock_details(tenant).await.ok().flatten()
        }
        _ => None,
    };
    let (Some(current), Some(lock_identifier)) = (current.as_ref(), lock_identifier) else {
        if let Some(lock_identifier) = lock_identifier {
            return Err(unmatched_lock(context, lock_identifier).await?);
        }
        let others = locks_in_other_environments(context).await?;
        context.output.lock(&LockReport {
            tenant,
            lock: current.as_ref(),
            outcome: LockOutcome::Shown,
            unlock_command: current
                .as_ref()
                .and_then(|lock| lock.identifier())
                .map(|identifier| {
                    context.invocation.command(&format!(
                        "unlock {}",
                        cuenv_infrastructure::strip_control_characters(identifier)
                    ))
                }),
            details: details.as_ref(),
        });
        context.output.other_environments(&others);
        return Ok(());
    };
    let Some(current_identifier) = current.identifier() else {
        return Err(CliError::config_with_help(
            "this lock belongs to a migration; nothing was released",
            "Inspect the migration operation and use its recovery procedure.",
        ));
    };
    if lock_identifier != current_identifier {
        return Err(CliError::infrastructure(
            format!(
                "{} is locked by another run; nothing was released",
                super::guard::tenant_label(tenant)
            ),
            Some(format!(
                "Run `{}` without an identifier to inspect the lock.",
                context.invocation.command("unlock")
            )),
            InfrastructureFailureKind::Locked,
        )
        .with_lock(LockStatus {
            identifier: cuenv_infrastructure::strip_control_characters(current_identifier),
            released: false,
        }));
    }
    if lock_identifier.starts_with(MIGRATION_FENCE_PREFIX) && fence == FenceRelease::Refuse {
        return Err(CliError::config_with_help(
            format!(
                "{} is a migration fence: its state was migrated to another backend, and the \
                 lock keeps writes off this copy; nothing was released",
                cuenv_infrastructure::strip_control_characters(lock_identifier)
            ),
            format!(
                "To undo the migration, run `{}`. To release the fence anyway (writes here will \
                 not reach the migrated state), add --migration-fence.",
                context
                    .invocation
                    .command("state migrate --confirm-maintenance --source-physical-identity <source-uuid> --target-physical-identity <target-uuid> --rollback")
            ),
        ));
    }
    if let Some(details) = &details {
        emit_stderr!(format!(
            "Releasing lock {} held by '{}' since {}{}, taken by cuenv {}; last change: {}",
            cuenv_infrastructure::strip_control_characters(lock_identifier),
            cuenv_infrastructure::strip_control_characters(&current.holder),
            cuenv_infrastructure::strip_control_characters(&current.acquired_at),
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
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    if !released {
        // The lock is held, just not by the run the operator named: that
        // is concurrent activity, not a configuration mistake.
        let holder = cuenv_infrastructure::strip_control_characters(current_identifier);
        return Err(CliError::infrastructure(
            format!(
                "{} is no longer locked by {holder}; nothing was released",
                super::guard::tenant_label(tenant)
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
) -> Result<CliError, CliError> {
    let mut elsewhere = Vec::new();
    for (name, tenant) in context.siblings.declared()? {
        let held = context
            .interrupts
            .until_interrupted(context.store.current_lock(&tenant))
            .await?
            .map_err(|error| {
                super::failure::failure(&error, &context.invocation.with_environment(Some(&name)))
            })?;
        if held.is_some_and(|lock| lock.identifier() == Some(lock_identifier)) {
            elsewhere.push(format!(
                "`{}`",
                context
                    .invocation
                    .with_environment(Some(&name))
                    .command("unlock")
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
    Ok(CliError::config_with_help(
        format!(
            "{} is not locked by the supplied identifier; nothing was released",
            super::guard::tenant_label(context.tenant)
        ),
        help,
    ))
}

/// What a run without `--env` says about other environments of the project
/// that hold a lock: `unlock` only looks at the identity it was given.
pub(super) async fn locks_in_other_environments(
    context: &CommandContext<'_>,
) -> Result<Vec<OtherEnvironment>, CliError> {
    let mut found = Vec::new();
    for (name, tenant) in context.siblings.declared()? {
        let held = context
            .interrupts
            .until_interrupted(context.store.current_lock(&tenant))
            .await?
            .map_err(|error| {
                super::failure::failure(&error, &context.invocation.with_environment(Some(&name)))
            })?;
        if let Some(lock) = held {
            let command = lock.identifier().map_or_else(
                || {
                    context
                        .invocation
                        .with_environment(Some(&name))
                        .command("unlock")
                },
                |identifier| {
                    context
                        .invocation
                        .with_environment(Some(&name))
                        .command(&format!(
                            "unlock {}",
                            cuenv_infrastructure::strip_control_characters(identifier)
                        ))
                },
            );
            found.push(OtherEnvironment {
                command,
                environment: name,
                finding: Finding::Locked { lock },
            });
        }
    }
    Ok(found)
}
