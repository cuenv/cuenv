//! Running work under the state lock, and releasing it afterwards.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{InfrastructureError, LockRequest, StateLock, StateStore, TenantKey};

use super::dispatch::CommandContext;
use super::failure::{failure, printable_text};
use super::guard::tenant_label;
use super::holder;
use super::interrupts::HeldLock;
use super::invocation::Invocation;
use crate::cli::{CliError, InfrastructureFailureKind, LockStatus};

/// Take the project's lock, run `work` with it, and release it.
///
/// Creates or upgrades the state tables first: every write happens here,
/// and only here. The lock identifier is chosen and registered with the
/// interrupt handling before it is requested, so a forced exit during
/// acquisition can name it. Acquisition is never abandoned half way: an
/// interrupt while it runs is honoured once it returns, by releasing the
/// lock again. The outcome says whether the lock was released.
pub(super) async fn under_lock<Outcome, Work>(
    context: &CommandContext<'_>,
    operation: &str,
    work: impl FnOnce(StateLock) -> Work,
) -> Result<Outcome, CliError>
where
    Work: Future<Output = Result<Outcome, CliError>>,
{
    context
        .store
        .migrate()
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    context.interrupts.check()?;
    let lock = StateLock::generate();
    let held = HeldLock {
        store: Arc::clone(context.store),
        tenant: context.tenant.clone(),
        lock: lock.clone(),
    };
    context.interrupts.acquiring(held.clone());
    let holder = holder::describe(operation);
    let acquired = context
        .store
        .acquire_lock(
            context.tenant,
            &LockRequest {
                lock: &lock,
                holder: &holder,
            },
        )
        .await;
    if let Err(error) = acquired {
        context.interrupts.released();
        return Err(acquisition_failure(&error, &lock, context.invocation));
    }
    context.interrupts.hold(held);
    emit_stderr!(format!(
        "Acquired lock {} for {}",
        lock.lock_identifier,
        tenant_label(context.tenant)
    ));

    let result = match context.interrupts.check() {
        Ok(()) => work(lock.clone()).await,
        Err(interrupted) => Err(interrupted),
    };
    let released = release(context.store.as_ref(), context.tenant, &lock).await;
    context.interrupts.released();
    // An operation that finished but could not release its lock still tells
    // what it did, in the error.
    let completed = if result.is_ok() && released.is_err() {
        context.output.take_result()
    } else {
        None
    };
    if let Ok(outcome) = &released {
        emit_stderr!(release_notice(&lock, context.tenant, *outcome));
    }
    settle(Settlement {
        operation,
        lock: &lock,
        invocation: context.invocation,
        result,
        released,
        completed,
    })
}

/// A failed acquisition. When its outcome is uncertain (the response was
/// lost), the error names this run's lock so it can be released.
pub(super) fn acquisition_failure(
    error: &InfrastructureError,
    lock: &StateLock,
    invocation: &Invocation,
) -> CliError {
    let failure = failure(error, invocation);
    match error {
        InfrastructureError::Locked { .. } => failure,
        _ if error.to_string().contains(&lock.lock_identifier) => failure.with_lock(LockStatus {
            identifier: lock.lock_identifier.clone(),
            released: false,
        }),
        _ => failure,
    }
}

/// The work's outcome and the release's, for [`settle`].
pub(super) struct Settlement<'settlement, Outcome> {
    pub(super) operation: &'settlement str,
    pub(super) lock: &'settlement StateLock,
    pub(super) invocation: &'settlement Invocation,
    pub(super) result: Result<Outcome, CliError>,
    pub(super) released: cuenv_infrastructure::Result<Release>,
    /// The JSON result the operation produced, when it finished and its lock
    /// could not be released.
    pub(super) completed: Option<serde_json::Value>,
}

/// Combine the work's outcome with the release's. Infrastructure errors
/// carry the lock and whether it was released; a lock that was not
/// released is never reported as released.
pub(super) fn settle<Outcome>(settlement: Settlement<'_, Outcome>) -> Result<Outcome, CliError> {
    let Settlement {
        operation,
        lock,
        invocation,
        result,
        released,
        completed,
    } = settlement;
    let identifier = &lock.lock_identifier;
    let status = |released| LockStatus {
        identifier: identifier.clone(),
        released,
    };
    let unlock_help = format!(
        "After checking no run is active, release it with `{}`.",
        invocation.command(&format!("unlock {identifier}"))
    );
    match (result, released) {
        (Ok(outcome), Ok(_)) => Ok(outcome),
        (Err(error), Ok(release)) => Err(error.with_lock(status(release == Release::Released))),
        (Ok(_), Err(release_error)) => {
            let error = CliError::infrastructure(
                format!(
                    "{operation} succeeded but lock {identifier} was NOT released: {}",
                    printable_text(&release_error.to_string())
                ),
                Some(unlock_help),
                InfrastructureFailureKind::Failed,
            )
            .with_lock(status(false));
            Err(match completed {
                Some(result) => error.with_detail("result", result),
                None => error,
            })
        }
        (Err(error), Err(release_error)) => {
            let not_released = format!(
                "Lock {identifier} was NOT released ({}). {unlock_help}",
                printable_text(&release_error.to_string())
            );
            let help = error.help().map_or_else(
                || not_released.clone(),
                |help| format!("{help} {not_released}"),
            );
            Err(match error {
                CliError::Infrastructure { .. } => error.with_help(help).with_lock(status(false)),
                // Keep the lock in the report: an unreleased lock is an
                // infrastructure failure whatever stopped the work.
                other => CliError::infrastructure(
                    format!(
                        "{operation} failed and its lock was NOT released: {}",
                        other.message()
                    ),
                    Some(help),
                    InfrastructureFailureKind::Failed,
                )
                .with_lock(status(false)),
            })
        }
    }
}

/// Attempts to release the lock after a run.
pub(super) const RELEASE_ATTEMPTS: u32 = 3;

/// What releasing a run's lock did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Release {
    /// The lock was held and is released.
    Released,
    /// The lock was already gone when the run ended (another actor released
    /// it, or took it over): nothing was released.
    NoLongerHeld,
}

/// The line that tells what releasing the lock did, saying nothing was
/// released when the lock was no longer held.
pub(super) fn release_notice(lock: &StateLock, tenant: &TenantKey, release: Release) -> String {
    match release {
        Release::Released => format!(
            "Released lock {} for {}",
            lock.lock_identifier,
            tenant_label(tenant)
        ),
        Release::NoLongerHeld => format!(
            "warning: lock {} for {} was no longer held when the run ended (another run \
             released or took it); nothing was released",
            lock.lock_identifier,
            tenant_label(tenant)
        ),
    }
}

/// Release the lock, retrying a failure twice (after 250 ms, then 500 ms)
/// and never waiting after the last attempt.
pub(super) async fn release(
    store: &dyn StateStore,
    tenant: &TenantKey,
    lock: &StateLock,
) -> cuenv_infrastructure::Result<Release> {
    let mut attempt = 1;
    loop {
        match store.unlock(tenant, lock).await {
            Ok(true) => return Ok(Release::Released),
            Ok(false) => return Ok(Release::NoLongerHeld),
            Err(error) if attempt >= RELEASE_ATTEMPTS => return Err(error),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                attempt += 1;
            }
        }
    }
}
