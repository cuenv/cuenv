//! `state migrate`: copy a project's state from its current backend to the
//! object store `infrastructure.migrateStateTo` names, leaving a fence lock
//! on the source, and roll a migration back.
//!
//! Per tenant, forward: refuse pending unrecorded changes; refuse a target
//! that already holds state of another origin; take the source lock as the
//! fence (`migrated-<id>`, kept for good); create the target with the
//! source's records and last writer, holding a migrator lock in the same
//! write; read it back and compare; commit by releasing the migrator lock;
//! record the tenant in the project's `_migration.json`. A rerun recognises
//! its own migration id and continues where it stopped.

use std::collections::BTreeMap;

use cuenv_infrastructure::{
    InfrastructureError, LastWriter, LockRequest, MIGRATION_FENCE_PREFIX, ManagedResource,
    ManifestEntry, MigrationManifest, ObjectStateStore, StateLock, StateStore, TenantImport,
    TenantKey, UnrecordedStore, strip_control_characters,
};

use super::action::{MigrateRequest, MigrationDirection, MigrationScope, SourceUpgrade};
use super::dispatch::CommandContext;
use super::failure::failure;
use super::guard::tenant_label;
use super::holder;
use super::output::{MigratedTenant, MigrationReport};
use crate::cli::CliError;

/// Manifest status of a tenant whose migration started.
const PENDING: &str = "pending";
/// Manifest status of a migrated tenant.
const MIGRATED: &str = "migrated";
/// Manifest status of a tenant whose migration was rolled back.
const ROLLED_BACK: &str = "rolled-back";

/// What a migration works with besides the context.
pub(super) struct MigrationRun<'run> {
    /// The `migrateStateTo` store.
    pub(super) target: &'run ObjectStateStore,
    /// Where unrecorded changes are saved.
    pub(super) unrecorded: &'run UnrecordedStore,
    /// The source backend, for messages (`turso`, `d1`, `s3`).
    pub(super) source_label: String,
    /// What to do.
    pub(super) request: MigrateRequest,
}

/// `state migrate`.
pub(super) async fn migrate(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
) -> Result<(), CliError> {
    let fail = |error: InfrastructureError| failure(&error, context.invocation);
    if run.request.source_upgrade == SourceUpgrade::Allow {
        context.store.migrate().await.map_err(fail)?;
    }
    let tenants = selected_tenants(context, run.request.scope).await?;
    let Some(first) = tenants.first() else {
        return Err(CliError::config(format!(
            "the source holds no state for project '{}'; nothing to migrate",
            strip_control_characters(context.tenant.project())
        )));
    };
    run.target.migrate().await.map_err(fail)?;
    let mut manifest = run
        .target
        .manifest(first)
        .await
        .map_err(fail)?
        .unwrap_or_default();
    let mut report = MigrationReport {
        migration: String::new(),
        direction: run.request.direction,
        tenants: Vec::new(),
    };
    match run.request.direction {
        MigrationDirection::Forward => {
            let id = migration_id(&manifest);
            report.migration.clone_from(&id);
            for tenant in &tenants {
                if entry(&manifest, tenant).is_none_or(|entry| entry.status != MIGRATED) {
                    upsert(&mut manifest, tenant, PENDING, &id, None);
                }
            }
            run.target
                .write_manifest(first, &manifest)
                .await
                .map_err(fail)?;
            for tenant in &tenants {
                let serial = forward(context, run, tenant, &id).await?;
                upsert(&mut manifest, tenant, MIGRATED, &id, Some(serial));
                run.target
                    .write_manifest(first, &manifest)
                    .await
                    .map_err(fail)?;
                report.tenants.push(MigratedTenant {
                    tenant: tenant.clone(),
                    location: run.target.location(tenant),
                });
            }
        }
        MigrationDirection::Rollback | MigrationDirection::CopyBack => {
            for tenant in &tenants {
                let Some(found) = entry(&manifest, tenant).cloned() else {
                    return Err(CliError::config(format!(
                        "{} was not migrated by `state migrate`; nothing to roll back",
                        tenant_label(tenant)
                    )));
                };
                report.migration.clone_from(&found.migration);
                if found.status == ROLLED_BACK {
                    continue;
                }
                rollback(context, run, tenant, &found).await?;
                upsert(&mut manifest, tenant, ROLLED_BACK, &found.migration, None);
                run.target
                    .write_manifest(first, &manifest)
                    .await
                    .map_err(fail)?;
                report.tenants.push(MigratedTenant {
                    tenant: tenant.clone(),
                    location: run.target.location(tenant),
                });
            }
        }
    }
    context.output.migrated(&report);
    Ok(())
}

/// The tenants a migration works on: the selected one, or every
/// environment the source holds state for.
async fn selected_tenants(
    context: &CommandContext<'_>,
    scope: MigrationScope,
) -> Result<Vec<TenantKey>, CliError> {
    let tenant = context.tenant;
    if scope == MigrationScope::Selected {
        return Ok(vec![tenant.clone()]);
    }
    let environments = context
        .store
        .environments(tenant.module_path(), tenant.project())
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    environments
        .into_iter()
        .map(|environment| match environment {
            Some(name) => TenantKey::with_environment(tenant.module_path(), tenant.project(), name),
            None => TenantKey::try_new(tenant.module_path(), tenant.project()),
        })
        .collect::<cuenv_infrastructure::Result<Vec<_>>>()
        .map_err(|error| failure(&error, context.invocation))
}

/// The migration id a run continues, or a new one.
fn migration_id(manifest: &MigrationManifest) -> String {
    manifest
        .tenants
        .iter()
        .find(|entry| entry.status != ROLLED_BACK)
        .map_or_else(
            || uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
            |entry| entry.migration.clone(),
        )
}

fn entry<'manifest>(
    manifest: &'manifest MigrationManifest,
    tenant: &TenantKey,
) -> Option<&'manifest ManifestEntry> {
    manifest
        .tenants
        .iter()
        .find(|entry| entry.environment.as_deref() == tenant.environment())
}

fn upsert(
    manifest: &mut MigrationManifest,
    tenant: &TenantKey,
    status: &str,
    migration: &str,
    committed_serial: Option<u64>,
) {
    let updated = ManifestEntry {
        environment: tenant.environment().map(str::to_string),
        status: status.to_string(),
        migration: migration.to_string(),
        committed_serial,
    };
    match manifest
        .tenants
        .iter_mut()
        .find(|entry| entry.environment.as_deref() == tenant.environment())
    {
        Some(existing) => *existing = updated,
        None => manifest.tenants.push(updated),
    }
}

/// The fence lock of migration `id`.
fn fence(id: &str) -> StateLock {
    StateLock {
        lock_identifier: format!("{MIGRATION_FENCE_PREFIX}{id}"),
    }
}

/// Migrate one tenant. Returns the target's serial at the commit.
async fn forward(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    tenant: &TenantKey,
    id: &str,
) -> Result<u64, CliError> {
    let fail = |error: InfrastructureError| failure(&error, context.invocation);
    let source = context.store.as_ref();
    let target = run.target;
    if run.unrecorded.has_pending(tenant).map_err(fail)? {
        return Err(CliError::config_with_help(
            format!(
                "{} has unrecorded changes saved on this machine; refusing to migrate it",
                tenant_label(tenant)
            ),
            format!(
                "Record them first with `{}`.",
                context
                    .invocation
                    .with_environment(tenant.environment())
                    .command("state recover")
            ),
        ));
    }
    let origin = target.origin(tenant).await.map_err(fail)?;
    let exists = target.state_size(tenant).await.map_err(fail)?.is_some();
    let ours = origin.as_ref().is_some_and(|(origin, _, _)| origin == id);
    if exists && !ours {
        return Err(CliError::config_with_help(
            format!(
                "{} already holds state for {}; refusing to overwrite it",
                target
                    .location(tenant)
                    .unwrap_or_else(|| "the target".to_string()),
                tenant_label(tenant)
            ),
            "A migration only creates state. If that state is left over from a rolled-back \
             migration or a test, delete it from the bucket first.",
        ));
    }
    let target_location = target
        .location(tenant)
        .unwrap_or_else(|| "the target".to_string());
    let version = env!("CARGO_PKG_VERSION");
    let fence = fence(id);
    let fence_holder = format!(
        "migrated to {target_location} by cuenv {version} at {}. Builds older than {version} do \
         not protect this fence. Roll back with `{}`",
        chrono::Utc::now().to_rfc3339(),
        context
            .invocation
            .with_environment(tenant.environment())
            .command("state migrate --rollback")
    );
    take_fence(context, tenant, &fence, &fence_holder).await?;
    let records = source.list(tenant).await.map_err(fail)?;
    // The source's last writer carries over; only state no run recorded a
    // writer for gets this run's instance.
    let writer = source
        .last_writer(tenant)
        .await
        .map_err(fail)?
        .unwrap_or_else(|| LastWriter {
            instance: context.instance.clone(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        });
    if exists {
        // A run of this migration stopped after creating the target; a
        // migrator lock it did not release means it did not commit.
        if let Some(held) = target.current_lock(tenant).await.map_err(fail)?
            && held
                .holder
                .starts_with("cuenv infrastructure state migrate")
        {
            verify(context, target, tenant, &records).await?;
            target
                .unlock(
                    tenant,
                    &StateLock {
                        lock_identifier: held.lock_identifier,
                    },
                )
                .await
                .map_err(fail)?;
        } else {
            verify(context, target, tenant, &records).await?;
        }
    } else {
        let lock = StateLock::generate();
        let holder = holder::describe("state migrate");
        let source_location = source
            .location(tenant)
            .unwrap_or_else(|| run.source_label.clone());
        let created = target
            .import(
                tenant,
                &TenantImport {
                    lock: &lock,
                    holder: &holder,
                    resources: &records,
                    last_writer: Some(&writer),
                    migration: id,
                    source: &source_location,
                },
            )
            .await
            .map_err(fail)?;
        if !created {
            return Err(CliError::config(format!(
                "{target_location} was created by another run while this one migrated; run \
                 `state migrate` again"
            )));
        }
        verify(context, target, tenant, &records).await?;
        target.unlock(tenant, &lock).await.map_err(fail)?;
    }
    let serial = target
        .origin(tenant)
        .await
        .map_err(fail)?
        .map_or(0, |(_, _, serial)| serial);
    Ok(serial)
}

/// Take the source lock as the migration fence, or find it already taken by
/// an earlier run of the same migration.
async fn take_fence(
    context: &CommandContext<'_>,
    tenant: &TenantKey,
    fence: &StateLock,
    holder: &str,
) -> Result<(), CliError> {
    let acquired = context
        .store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: fence,
                holder,
                instance: None,
            },
        )
        .await;
    match acquired {
        Ok(_) => Ok(()),
        Err(InfrastructureError::Locked { .. })
            if context
                .store
                .current_lock(tenant)
                .await
                .ok()
                .flatten()
                .is_some_and(|held| held.lock_identifier == fence.lock_identifier) =>
        {
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            let failure = failure(&error, context.invocation);
            if message.contains("migrate the state store") {
                Err(failure.with_help(format!(
                    "The source database has an older schema. `{}` upgrades it first; projects \
                     pinned to older cuenv on that database stop working until they upgrade.",
                    context.invocation.command("state migrate --upgrade-source")
                )))
            } else {
                Err(failure)
            }
        }
    }
}

/// Read the target back and compare it with the source's records. A
/// mismatch names addresses only, never values.
async fn verify(
    context: &CommandContext<'_>,
    target: &ObjectStateStore,
    tenant: &TenantKey,
    expected: &[ManagedResource],
) -> Result<(), CliError> {
    let stored = target
        .list(tenant)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    let mismatched = differences(expected, &stored);
    if mismatched.is_empty() {
        return Ok(());
    }
    Err(CliError::infrastructure(
        format!(
            "the copy of {} does not match its source at {}; the source stays fenced and the \
             target stays locked",
            tenant_label(tenant),
            mismatched.join(", ")
        ),
        Some(
            "Run `state migrate` again to retry, or `state migrate --rollback` to release the \
             source."
                .to_string(),
        ),
        crate::cli::InfrastructureFailureKind::Failed,
    ))
}

/// The addresses whose content differs between two record sets.
fn differences(left: &[ManagedResource], right: &[ManagedResource]) -> Vec<String> {
    let index = |records: &[ManagedResource]| -> BTreeMap<String, ManagedResource> {
        records
            .iter()
            .map(|record| (record.address.to_string(), record.clone()))
            .collect()
    };
    let (left, right) = (index(left), index(right));
    left.keys()
        .chain(right.keys())
        .filter(|address| match (left.get(*address), right.get(*address)) {
            (Some(one), Some(other)) => !one.same_content(other),
            _ => true,
        })
        .map(|address| strip_control_characters(address))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Roll one tenant's migration back.
async fn rollback(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    tenant: &TenantKey,
    found: &ManifestEntry,
) -> Result<(), CliError> {
    let fail = |error: InfrastructureError| failure(&error, context.invocation);
    let source = context.store.as_ref();
    let target = run.target;
    let fence = fence(&found.migration);
    if let Some((origin, _, serial)) = target.origin(tenant).await.map_err(fail)? {
        if origin != found.migration {
            return Err(CliError::config(format!(
                "{} holds state of another migration; refusing to roll it back",
                target
                    .location(tenant)
                    .unwrap_or_else(|| "the target".to_string())
            )));
        }
        let moved = found.committed_serial != Some(serial);
        if moved && run.request.direction == MigrationDirection::Rollback {
            return Err(CliError::config_with_help(
                format!(
                    "{} changed in the target since the migration; rolling back would lose \
                     those changes",
                    tenant_label(tenant)
                ),
                format!(
                    "Copy them back to the source first with `{}`.",
                    context
                        .invocation
                        .with_environment(tenant.environment())
                        .command("state migrate --rollback --copy-back")
                ),
            ));
        }
        let lock = StateLock::generate();
        let holder = holder::describe("state migrate --rollback");
        target
            .acquire_lock(
                tenant,
                &LockRequest {
                    lock: &lock,
                    holder: &holder,
                    instance: None,
                },
            )
            .await
            .map_err(fail)?;
        if moved {
            let copied = target.list(tenant).await.map_err(fail)?;
            let previous = source.list(tenant).await.map_err(fail)?;
            source
                .put_many(tenant, &fence, &copied)
                .await
                .map_err(fail)?;
            for stale in previous
                .iter()
                .filter(|record| !copied.iter().any(|copy| copy.address == record.address))
            {
                source
                    .delete(tenant, &fence, &stale.address)
                    .await
                    .map_err(fail)?;
            }
            let written = source.list(tenant).await.map_err(fail)?;
            let mismatched = differences(&copied, &written);
            if !mismatched.is_empty() {
                return Err(CliError::config(format!(
                    "copying {} back to the source did not verify at {}; the target stays \
                     locked by {}",
                    tenant_label(tenant),
                    mismatched.join(", "),
                    lock.lock_identifier
                )));
            }
        }
        let back_to = source
            .location(tenant)
            .unwrap_or_else(|| run.source_label.clone());
        target
            .tombstone(tenant, &lock, &back_to)
            .await
            .map_err(fail)?;
        target.unlock(tenant, &lock).await.map_err(fail)?;
    }
    source.unlock(tenant, &fence).await.map_err(fail)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_continues_its_own_migration() {
        let mut manifest = MigrationManifest::default();
        let tenant = TenantKey::try_new("example.com/m", "web").unwrap();
        let first = migration_id(&manifest);
        upsert(&mut manifest, &tenant, PENDING, &first, None);
        assert_eq!(migration_id(&manifest), first);
        upsert(&mut manifest, &tenant, ROLLED_BACK, &first, None);
        assert_ne!(migration_id(&manifest), first);
        assert_eq!(manifest.tenants.len(), 1);
    }
}
