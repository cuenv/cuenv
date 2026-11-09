//! `state migrate`: copy a project's state from its current backend to the
//! object store `infrastructure.migrateStateTo` names, leaving a fence lock
//! on the source, and roll a migration back.
//!
//! Per tenant, forward: refuse pending unrecorded changes; refuse a target
//! that already holds state of another origin; take the source lock as the
//! fence (`migrated-<id>`, kept for good); create the target with the
//! source's records and last writer, holding a migrator lock in the same
//! write; read it back and compare; commit by releasing the migrator lock;
//! record the tenant in the project's `_migration.json`. A rerun checks the
//! recorded binding and commit receipt; an unconfirmed lock requires manual
//! reconciliation under the maintenance freeze.

use std::collections::BTreeMap;

use cuenv_infrastructure::state::object::store::legacy_inventory::LegacyTargetState;
use cuenv_infrastructure::state::object::store::legacy_migration::{
    LegacyCommitEvidence, LegacyCommitRequest, LegacyMigrationBinding, LegacyMigrationSelection,
    LegacyMutation, LegacyRollbackAction, LegacyRollbackRequest, LegacyTombstoneRequest,
};
use cuenv_infrastructure::state::object::store::migration_manifest::{
    LegacyManifestMutation, LegacyManifestSnapshot, LegacyManifestUpdate,
};
use cuenv_infrastructure::state::{
    LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationCopyPlan,
    LegacyMigrationDeleteRequest, LegacyMigrationRecordRequest, LegacyMigrationSnapshot,
    LegacyMigrationSnapshotRead, LegacyMigrationWriterRequest, MigrationMutation,
};
use cuenv_infrastructure::{
    InfrastructureError, LastWriter, LockRequest, MIGRATION_FENCE_PREFIX, ManagedResource,
    ManifestEntry, MigrationManifest, ObjectStateStore, StateLock, StateStore, TenantImport,
    TenantKey, UnrecordedStore,
};

use super::action::{MigrateRequest, MigrationDirection, MigrationScope, SourceUpgrade};
use super::dispatch::CommandContext;

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
    /// Configured target object prefix.
    pub(super) target_root: Option<&'run str>,
    /// What to do.
    pub(super) request: MigrateRequest,
}

/// `state migrate`.
pub(super) async fn migrate(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
) -> Result<(), CliError> {
    super::run::require_migration_maintenance(&run.request)?;
    let fail = |error: InfrastructureError| super::failure::failure(&error, context.invocation);
    let mut manifest_snapshot = run
        .target
        .legacy_manifest_snapshot(context.tenant)
        .await
        .map_err(fail)?;
    let mut manifest = manifest_snapshot
        .as_ref()
        .map_or_else(MigrationManifest::default, |snapshot| {
            snapshot.manifest().clone()
        });
    validate_manifest(&manifest, context.tenant)?;
    let tenants = selected_tenants(context, &manifest, run.request).await?;
    if tenants.is_empty() {
        return Err(CliError::config(format!(
            "the source holds no state for project '{}'; nothing to migrate",
            cuenv_infrastructure::strip_control_characters(context.tenant.project())
        )));
    }
    let binding = migration_binding(context, run, &tenants)?;
    for tenant in &tenants {
        run.target
            .validate_legacy_target_binding(tenant, &binding)
            .map_err(fail)?;
        if let Some(found) = entry(&manifest, tenant) {
            if found.binding.as_ref() != Some(&binding) {
                return Err(CliError::config(
                    "the migration's recorded physical stores or tenant selection do not match; inspect both stores before continuing",
                ));
            }
            if found.status == ROLLED_BACK && run.request.direction == MigrationDirection::Forward {
                return Err(CliError::config(
                    "a rolled-back target must be reconciled before starting another migration",
                ));
            }
        }
    }
    if run.request.direction != MigrationDirection::Forward
        && tenants
            .iter()
            .all(|tenant| entry(&manifest, tenant).is_some_and(|entry| entry.status == ROLLED_BACK))
    {
        return Err(CliError::config(
            "the selected migration is already rolled back; nothing to do",
        ));
    }
    if run.request.direction != MigrationDirection::Forward
        && tenants
            .iter()
            .any(|tenant| entry(&manifest, tenant).is_some_and(|entry| entry.status == ROLLED_BACK))
    {
        return Err(CliError::config(
            "the migration was only partly rolled back; reconcile all stores before continuing",
        ));
    }
    let planned_id = if run.request.direction == MigrationDirection::Forward {
        Some(migration_id(&manifest, &tenants)?)
    } else {
        None
    };
    require_manifest_headroom(&manifest, &tenants, run.request.direction)?;
    preflight_targets(
        context,
        run,
        &TargetPreflight {
            tenants: &tenants,
            manifest: &manifest,
            binding: &binding,
            migration: planned_id.as_deref(),
        },
    )
    .await?;
    if run.request.source_upgrade == SourceUpgrade::Allow {
        context.store.migrate().await.map_err(fail)?;
    }
    run.target.migrate().await.map_err(fail)?;
    let mut report = MigrationReport {
        migration: String::new(),
        direction: run.request.direction,
        tenants: Vec::new(),
    };
    match run.request.direction {
        MigrationDirection::Forward => {
            let id = planned_id.ok_or_else(|| CliError::config("migration identifier missing"))?;
            report.migration.clone_from(&id);
            for tenant in &tenants {
                if entry(&manifest, tenant).is_none_or(|entry| entry.status != MIGRATED) {
                    upsert(
                        &mut manifest,
                        tenant,
                        MigrationUpdate {
                            status: PENDING,
                            migration: &id,
                            committed_serial: None,
                            commit_evidence: None,
                            binding: Some(&binding),
                        },
                    );
                    let pending = entry(&manifest, tenant)
                        .cloned()
                        .ok_or_else(|| CliError::config("pending migration entry is missing"))?;
                    let next = write_manifest_entry(
                        context,
                        run.target,
                        ManifestChange {
                            tenant,
                            previous: manifest_snapshot.as_ref(),
                            entry: &pending,
                        },
                    )
                    .await?;
                    manifest = next.manifest().clone();
                    manifest_snapshot = Some(next);
                }
            }
            for tenant in &tenants {
                let evidence = forward(
                    context,
                    run,
                    ForwardTenant {
                        tenant,
                        migration: &id,
                        binding: &binding,
                        manifest_evidence: entry(&manifest, tenant)
                            .and_then(|entry| entry.commit_evidence.as_ref()),
                    },
                )
                .await?;
                upsert(
                    &mut manifest,
                    tenant,
                    MigrationUpdate {
                        status: MIGRATED,
                        migration: &id,
                        committed_serial: Some(evidence.serial),
                        commit_evidence: Some(evidence),
                        binding: Some(&binding),
                    },
                );
                let updated = entry(&manifest, tenant)
                    .cloned()
                    .ok_or_else(|| CliError::config("migrated entry is missing"))?;
                let next = write_manifest_entry(
                    context,
                    run.target,
                    ManifestChange {
                        tenant,
                        previous: manifest_snapshot.as_ref(),
                        entry: &updated,
                    },
                )
                .await?;
                manifest = next.manifest().clone();
                manifest_snapshot = Some(next);
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
                        super::guard::tenant_label(tenant)
                    )));
                };
                report.migration.clone_from(&found.migration);
                if found.status == ROLLED_BACK {
                    continue;
                }
                rollback(
                    context,
                    run,
                    RollbackTenant {
                        tenant,
                        entry: &found,
                        binding: &binding,
                    },
                )
                .await?;
                upsert(
                    &mut manifest,
                    tenant,
                    MigrationUpdate {
                        status: ROLLED_BACK,
                        migration: &found.migration,
                        committed_serial: found.commit_evidence.map(|evidence| evidence.serial),
                        commit_evidence: found.commit_evidence,
                        binding: Some(&binding),
                    },
                );
                let updated = entry(&manifest, tenant)
                    .cloned()
                    .ok_or_else(|| CliError::config("rolled-back entry is missing"))?;
                let next = write_manifest_entry(
                    context,
                    run.target,
                    ManifestChange {
                        tenant,
                        previous: manifest_snapshot.as_ref(),
                        entry: &updated,
                    },
                )
                .await?;
                manifest = next.manifest().clone();
                manifest_snapshot = Some(next);
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

/// Reserve enough manifest revisions for all writes before changing either store.
fn require_manifest_headroom(
    manifest: &MigrationManifest,
    tenants: &[TenantKey],
    direction: MigrationDirection,
) -> Result<(), CliError> {
    let pending_writes = if direction == MigrationDirection::Forward {
        tenants
            .iter()
            .filter(|tenant| entry(manifest, tenant).is_none_or(|entry| entry.status != MIGRATED))
            .count()
    } else {
        0
    };
    let last_serial = tenants
        .len()
        .checked_add(pending_writes)
        .and_then(|writes| u64::try_from(writes).ok())
        .and_then(|writes| manifest.serial.unwrap_or(0).checked_add(writes));
    if last_serial.is_none_or(|serial| serial == u64::MAX) {
        return Err(CliError::config(
            "the migration manifest has insufficient serial capacity; reconcile it before continuing",
        ));
    }
    Ok(())
}

/// Validate every manifest entry before using or rewriting any of them.
fn validate_manifest(manifest: &MigrationManifest, project: &TenantKey) -> Result<(), CliError> {
    let mut environments = std::collections::HashSet::new();
    for entry in &manifest.tenants {
        if !environments.insert(entry.environment.as_deref()) {
            return Err(CliError::config(
                "the migration manifest repeats an environment",
            ));
        }
        let tenant = tenant_for_environment(project, entry.environment.as_deref())?;
        let Some(binding) = &entry.binding else {
            return Err(CliError::config(
                "the migration manifest has an unbound legacy entry; reconcile it manually",
            ));
        };
        binding.validate_for(&tenant).map_err(|_| {
            CliError::config("the migration manifest has an invalid tenant binding")
        })?;
        if entry.migration.len() != 16
            || !entry
                .migration
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CliError::config(
                "the migration manifest has an invalid identifier",
            ));
        }
        let evidence_is_valid = match entry.status.as_str() {
            PENDING => entry.committed_serial.is_none() && entry.commit_evidence.is_none(),
            MIGRATED => entry.commit_evidence.is_some_and(|evidence| {
                entry.committed_serial == Some(evidence.serial)
                    && !evidence.writer_nonce.is_nil()
                    && evidence.serial > 0
                    && evidence.serial < u64::MAX
            }),
            ROLLED_BACK => entry.commit_evidence.is_some_and(|evidence| {
                entry.committed_serial == Some(evidence.serial)
                    && !evidence.writer_nonce.is_nil()
                    && evidence.serial > 0
                    && evidence.serial < u64::MAX
            }),
            _ => false,
        };
        if !evidence_is_valid {
            return Err(CliError::config(
                "the migration manifest has an invalid status or commit receipt",
            ));
        }
    }
    Ok(())
}

struct TargetPreflight<'context> {
    tenants: &'context [TenantKey],
    manifest: &'context MigrationManifest,
    binding: &'context LegacyMigrationBinding,
    migration: Option<&'context str>,
}

/// Check every selected target before any probe, upgrade or manifest write.
async fn preflight_targets(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    input: &TargetPreflight<'_>,
) -> Result<(), CliError> {
    let fail = |error: InfrastructureError| super::failure::failure(&error, context.invocation);
    if input.binding.selection == LegacyMigrationSelection::All {
        let inventory = run
            .target
            .legacy_project_inventory(context.tenant)
            .await
            .map_err(fail)?;
        for state in &inventory {
            if !input.binding.environments.contains(&state.environment) {
                return Err(CliError::config(
                    "the target contains an environment outside the recorded migration; reconcile it before continuing",
                ));
            }
            let tenant = tenant_for_environment(context.tenant, state.environment.as_deref())?;
            let recorded = entry(input.manifest, &tenant);
            if recorded.is_some_and(|entry| entry.status == ROLLED_BACK)
                && state.state != LegacyTargetState::Tombstoned
            {
                return Err(CliError::config(
                    "a previously rolled-back target is no longer tombstoned",
                ));
            }
            if recorded.is_some_and(|entry| entry.status != ROLLED_BACK)
                && state.state != LegacyTargetState::Live
            {
                return Err(CliError::config(
                    "an active migration target is tombstoned; reconcile it before continuing",
                ));
            }
        }
    }
    for tenant in input.tenants {
        let recorded = entry(input.manifest, tenant);
        if recorded.is_some_and(|entry| entry.status == ROLLED_BACK) {
            return Err(CliError::config(
                "the migration was only partly rolled back; reconcile all stores before continuing",
            ));
        }
        if input.migration.is_none() && recorded.is_some_and(|entry| entry.status == PENDING) {
            return Err(CliError::config(
                "the selected migration is not recorded as committed; inspect and reconcile it before rollback",
            ));
        }
        let id = match (input.migration, recorded) {
            (Some(id), _) => id,
            (None, Some(recorded)) => recorded.migration.as_str(),
            (None, None) => {
                return Err(CliError::config(
                    "the selected tenant has no recorded migration",
                ));
            }
        };
        let source_lock = context.store.current_lock(tenant).await.map_err(fail)?;
        let source_fence = fence(id);
        let matching_fence = source_lock
            .as_ref()
            .is_some_and(|held| held.identifier() == Some(source_fence.lock_identifier.as_str()));
        if input.migration.is_none() {
            if !matching_fence {
                return Err(CliError::config(
                    "a selected source migration fence is missing or different; inspect both stores before rollback",
                ));
            }
        } else if source_lock.is_some() && (recorded.is_none() || !matching_fence) {
            return Err(CliError::config(
                "a selected source is locked by another operation",
            ));
        }
        let exists = run.target.state_size(tenant).await.map_err(fail)?.is_some();
        if exists && recorded.is_none() {
            return Err(CliError::config(
                "a selected target already contains unrelated state; refusing to overwrite it",
            ));
        }
        if input.migration.is_some()
            && !matching_fence
            && (exists || recorded.is_some_and(|entry| entry.status == MIGRATED))
        {
            return Err(CliError::config(
                "a migrated target has no matching source fence; inspect both stores before continuing",
            ));
        }
        if !exists {
            if input.migration.is_some() && recorded.is_none_or(|entry| entry.status == PENDING) {
                continue;
            }
            return Err(CliError::config(
                "a selected migration target is missing; inspect both stores before continuing",
            ));
        }
        if run
            .target
            .current_lock(tenant)
            .await
            .map_err(fail)?
            .is_some()
        {
            return Err(CliError::config(
                "a selected migration target is locked; reconcile it before continuing",
            ));
        }
        let evidence = run
            .target
            .legacy_commit_evidence(tenant, id, input.binding)
            .await
            .map_err(fail)?
            .ok_or_else(|| {
                CliError::config("a selected target lacks a matching atomic migration receipt")
            })?;
        if recorded
            .and_then(|entry| entry.commit_evidence)
            .is_some_and(|commit| commit != evidence)
        {
            return Err(CliError::config(
                "the target receipt differs from the migration manifest",
            ));
        }
    }
    Ok(())
}

fn tenant_for_environment(
    project: &TenantKey,
    environment: Option<&str>,
) -> Result<TenantKey, CliError> {
    match environment {
        Some(name) => TenantKey::with_environment(project.module_path(), project.project(), name),
        None => TenantKey::try_new(project.module_path(), project.project()),
    }
    .map_err(|_| CliError::config("the migration manifest contains an invalid environment"))
}

struct ManifestChange<'manifest> {
    tenant: &'manifest TenantKey,
    previous: Option<&'manifest LegacyManifestSnapshot>,
    entry: &'manifest ManifestEntry,
}

async fn write_manifest_entry(
    context: &CommandContext<'_>,
    target: &ObjectStateStore,
    change: ManifestChange<'_>,
) -> Result<LegacyManifestSnapshot, CliError> {
    match target
        .update_legacy_manifest(&LegacyManifestUpdate {
            tenant: change.tenant,
            previous: change.previous,
            entry: change.entry,
            revision: uuid::Uuid::new_v4(),
            write_nonce: uuid::Uuid::new_v4(),
        })
        .await
        .map_err(|error| super::failure::failure(&error, context.invocation))?
    {
        LegacyManifestMutation::Applied(snapshot) => Ok(*snapshot),
        LegacyManifestMutation::Stale | LegacyManifestMutation::Indeterminate => {
            Err(CliError::config(
                "the conditional migration manifest update could not be confirmed; inspect both stores before continuing",
            ))
        }
    }
}

/// Resume the recorded tenant set, or enumerate the source for a new
/// all-environment forward migration.
async fn selected_tenants(
    context: &CommandContext<'_>,
    manifest: &MigrationManifest,
    request: MigrateRequest,
) -> Result<Vec<TenantKey>, CliError> {
    let tenant = context.tenant;
    if request.scope == MigrationScope::Selected {
        return Ok(vec![tenant.clone()]);
    }
    if let Some(first) = manifest
        .tenants
        .iter()
        .find(|entry| entry.status != ROLLED_BACK)
    {
        let binding = first.binding.as_ref().ok_or_else(|| {
            CliError::config("the migration manifest has no recorded tenant binding")
        })?;
        if binding.selection != LegacyMigrationSelection::All {
            return Err(CliError::config(
                "the recorded migration selected one environment; continue it with the same selection",
            ));
        }
        for environment in &binding.environments {
            let found = manifest
                .tenants
                .iter()
                .find(|entry| entry.environment == *environment);
            if found.is_none_or(|entry| {
                entry.binding.as_ref() != Some(binding) || entry.migration != first.migration
            }) {
                return Err(CliError::config(
                    "the migration manifest is missing or conflicts with a recorded tenant",
                ));
            }
        }
        if manifest
            .tenants
            .iter()
            .any(|entry| entry.status != ROLLED_BACK && entry.binding.as_ref() != Some(binding))
        {
            return Err(CliError::config(
                "the project contains different active migration operations; reconcile them separately",
            ));
        }
        let source_environments = context
            .store
            .environments(tenant.module_path(), tenant.project())
            .await
            .map_err(|error| super::failure::failure(&error, context.invocation))?;
        if source_environments
            .iter()
            .any(|environment| !binding.environments.contains(environment))
        {
            return Err(CliError::config(
                "the source contains an environment outside the recorded migration; reconcile it before continuing",
            ));
        }
        return binding
            .environments
            .iter()
            .map(|environment| tenant_for_environment(tenant, environment.as_deref()))
            .collect();
    }
    if request.direction != MigrationDirection::Forward {
        return Err(CliError::config(
            "the project has no active migration to roll back",
        ));
    }
    let environments = context
        .store
        .environments(tenant.module_path(), tenant.project())
        .await
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    environments
        .into_iter()
        .map(|environment| match environment {
            Some(name) => TenantKey::with_environment(tenant.module_path(), tenant.project(), name),
            None => TenantKey::try_new(tenant.module_path(), tenant.project()),
        })
        .collect::<cuenv_infrastructure::Result<Vec<_>>>()
        .map_err(|error| super::failure::failure(&error, context.invocation))
}

/// Bind the selected tenants to the operator-attested stores and the actual
/// connected backend namespaces before any migration side effect.
fn migration_binding(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    tenants: &[TenantKey],
) -> Result<LegacyMigrationBinding, CliError> {
    let identities = run
        .request
        .physical_identities
        .ok_or_else(|| CliError::config("migration physical store identities are required"))?;
    let source = context.store.legacy_migration_source().ok_or_else(|| {
        CliError::config("the source backend has no safe migration connection identity")
    })?;
    let target_connection = run
        .target
        .legacy_connection_identity()
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    let mut environments: Vec<_> = tenants
        .iter()
        .map(|tenant| tenant.environment().map(ToString::to_string))
        .collect();
    environments.sort();
    let binding = LegacyMigrationBinding {
        source_physical_id: identities.source,
        target_physical_id: identities.target,
        selection: match run.request.scope {
            MigrationScope::All => LegacyMigrationSelection::All,
            MigrationScope::Selected => LegacyMigrationSelection::Selected,
        },
        source,
        target_connection,
        target_root: run.target_root.map(ToString::to_string),
        target_package: context.instance.package().to_string(),
        module_path: context.tenant.module_path().to_string(),
        project: context.tenant.project().to_string(),
        environments,
    };
    for tenant in tenants {
        binding
            .validate_for(tenant)
            .map_err(|error| super::failure::failure(&error, context.invocation))?;
    }
    Ok(binding)
}

/// Continue one selected migration, or choose a fresh identifier.
fn migration_id(manifest: &MigrationManifest, tenants: &[TenantKey]) -> Result<String, CliError> {
    let identities: std::collections::BTreeSet<_> = tenants
        .iter()
        .filter_map(|tenant| entry(manifest, tenant))
        .filter(|entry| entry.status != ROLLED_BACK)
        .map(|entry| entry.migration.as_str())
        .collect();
    match identities.len() {
        0 => Ok(uuid::Uuid::new_v4().simple().to_string()[..16].to_string()),
        1 => Ok(identities.first().copied().unwrap_or_default().to_string()),
        _ => Err(CliError::config(
            "the selected tenants belong to different migration operations; reconcile them separately",
        )),
    }
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

#[derive(Clone, Copy)]
struct MigrationUpdate<'update> {
    status: &'update str,
    migration: &'update str,
    committed_serial: Option<u64>,
    commit_evidence: Option<LegacyCommitEvidence>,
    binding: Option<&'update LegacyMigrationBinding>,
}

fn upsert(manifest: &mut MigrationManifest, tenant: &TenantKey, update: MigrationUpdate<'_>) {
    let updated = ManifestEntry {
        environment: tenant.environment().map(str::to_string),
        status: update.status.to_string(),
        migration: update.migration.to_string(),
        committed_serial: update.committed_serial,
        commit_evidence: update.commit_evidence,
        binding: update.binding.cloned(),
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

#[derive(Clone, Copy)]
struct ForwardTenant<'tenant> {
    tenant: &'tenant TenantKey,
    migration: &'tenant str,
    binding: &'tenant LegacyMigrationBinding,
    manifest_evidence: Option<&'tenant LegacyCommitEvidence>,
}

/// Migrate one tenant. Returns the target's atomic commit evidence.
async fn forward(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    input: ForwardTenant<'_>,
) -> Result<LegacyCommitEvidence, CliError> {
    let ForwardTenant {
        tenant,
        migration: id,
        binding,
        manifest_evidence,
    } = input;
    let fail = |error: InfrastructureError| super::failure::failure(&error, context.invocation);
    let source = context.store.as_ref();
    let target = run.target;
    if run.unrecorded.has_pending(tenant).map_err(fail)? {
        return Err(CliError::config_with_help(
            format!(
                "{} has unrecorded changes saved on this machine; refusing to migrate it",
                super::guard::tenant_label(tenant)
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
                super::guard::tenant_label(tenant)
            ),
            "A migration only creates state. If that state is left over from a rolled-back \
             migration or a test, delete it from the bucket first.",
        ));
    }
    let existing_evidence = if exists {
        if target.current_lock(tenant).await.map_err(fail)?.is_some() {
            return Err(CliError::config(
                "the migration target is locked; inspect it and reconcile the interrupted migration before retrying",
            ));
        }
        let evidence = target
            .legacy_commit_evidence(tenant, id, binding)
            .await
            .map_err(fail)?
            .ok_or_else(|| {
                CliError::config(
                    "the existing target lacks an atomic migration commit receipt; reconcile it before retrying",
                )
            })?;
        if manifest_evidence.is_some_and(|recorded| *recorded != evidence) {
            return Err(CliError::config(
                "the target commit receipt differs from the migration manifest",
            ));
        }
        Some(evidence)
    } else {
        None
    };
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
            .command(&format!(
                "state migrate --confirm-maintenance --source-physical-identity {} --target-physical-identity {} --rollback",
                binding.source_physical_id, binding.target_physical_id
            ))
    );
    take_fence(
        context,
        tenant,
        &LockRequest {
            lock: &fence,
            holder: &fence_holder,
            instance: None,
        },
    )
    .await?;
    let access = LegacyMigrationAccess {
        tenant,
        fence: &fence,
    };
    let LegacyMigrationSnapshot {
        resources: records,
        writer,
    } = match source
        .read_legacy_migration_snapshot(&access)
        .await
        .map_err(fail)?
    {
        LegacyMigrationSnapshotRead::Available(snapshot) => *snapshot,
        LegacyMigrationSnapshotRead::Stale | LegacyMigrationSnapshotRead::Indeterminate => {
            return Err(CliError::config(
                "the fenced source snapshot could not be confirmed; no target was imported",
            ));
        }
    };
    LegacyMigrationCopy {
        access,
        resources: &records,
        writer: writer.as_ref(),
    }
    .validate()
    .map_err(fail)?;
    if let Some(evidence) = existing_evidence {
        verify(
            context,
            target,
            Verification {
                tenant,
                expected: &records,
                writer: writer.as_ref(),
            },
        )
        .await?;
        Ok(evidence)
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
                    last_writer: writer.as_ref(),
                    migration: id,
                    source: &source_location,
                },
            )
            .await
            .map_err(fail)?;
        if !created {
            return Err(CliError::config(format!(
                "{target_location} was created by another run while this one migrated; inspect both stores before continuing"
            )));
        }
        verify(
            context,
            target,
            Verification {
                tenant,
                expected: &records,
                writer: writer.as_ref(),
            },
        )
        .await?;
        match target
            .commit_legacy_migration(&LegacyCommitRequest {
                tenant,
                binding,
                migration: id,
                lock: &lock,
                writer_nonce: uuid::Uuid::new_v4(),
            })
            .await
            .map_err(fail)?
        {
            LegacyMutation::Applied(evidence) => Ok(evidence),
            LegacyMutation::Stale | LegacyMutation::Indeterminate => Err(CliError::config(
                "the target migration commit could not be confirmed; the source remains fenced",
            )),
        }
    }
}

/// Take the source lock as the migration fence, or find it already taken by
/// an earlier run of the same migration.
async fn take_fence(
    context: &CommandContext<'_>,
    tenant: &TenantKey,
    request: &LockRequest<'_>,
) -> Result<(), CliError> {
    let acquired = context.store.acquire_lock(tenant, request).await;
    match acquired {
        Ok(_) => Ok(()),
        Err(InfrastructureError::Locked { .. })
            if context
                .store
                .current_lock(tenant)
                .await
                .ok()
                .flatten()
                .is_some_and(|held| {
                    held.identifier() == Some(request.lock.lock_identifier.as_str())
                }) =>
        {
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            let failure = super::failure::failure(&error, context.invocation);
            if message.contains("migrate the state store") {
                Err(failure.with_help(format!(
                    "The source database has an older schema. `{}` upgrades it first; projects \
                     pinned to older cuenv on that database stop working until they upgrade.",
                    context
                        .invocation
                        .command("state migrate --confirm-maintenance --source-physical-identity <source-uuid> --target-physical-identity <target-uuid> --upgrade-source")
                )))
            } else {
                Err(failure)
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Verification<'verification> {
    tenant: &'verification TenantKey,
    expected: &'verification [ManagedResource],
    writer: Option<&'verification LastWriter>,
}

/// Read the target back and compare it with the source's records. A
/// mismatch names addresses only, never values.
async fn verify(
    context: &CommandContext<'_>,
    target: &ObjectStateStore,
    comparison: Verification<'_>,
) -> Result<(), CliError> {
    let Verification {
        tenant,
        expected,
        writer,
    } = comparison;
    let stored = target
        .list(tenant)
        .await
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    let mismatched = differences(expected, &stored);
    let stored_writer = target
        .last_writer(tenant)
        .await
        .map_err(|error| super::failure::failure(&error, context.invocation))?;
    if mismatched.is_empty() && stored_writer.as_ref() == writer {
        return Ok(());
    }
    let mismatched = if stored_writer.as_ref() == writer {
        mismatched
    } else {
        [mismatched, vec!["last-writer metadata".to_string()]].concat()
    };
    Err(CliError::infrastructure(
        format!(
            "the copy of {} does not match its source at {}; the source stays fenced and the \
             target stays locked",
            super::guard::tenant_label(tenant),
            mismatched.join(", ")
        ),
        Some(
            "Inspect both stores and reconcile the interrupted migration before continuing."
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
            (Some(one), Some(other)) => one != other,
            _ => true,
        })
        .map(|address| cuenv_infrastructure::strip_control_characters(address))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Clone, Copy)]
struct RollbackTenant<'tenant> {
    tenant: &'tenant TenantKey,
    entry: &'tenant ManifestEntry,
    binding: &'tenant LegacyMigrationBinding,
}

/// Roll one tenant's migration back.
async fn rollback(
    context: &CommandContext<'_>,
    run: &MigrationRun<'_>,
    input: RollbackTenant<'_>,
) -> Result<(), CliError> {
    let RollbackTenant {
        tenant,
        entry: found,
        binding,
    } = input;
    let fail = |error: InfrastructureError| super::failure::failure(&error, context.invocation);
    let source = context.store.as_ref();
    let target = run.target;
    let fence = fence(&found.migration);
    let Some(commit) = found.commit_evidence.as_ref() else {
        return Err(CliError::config(
            "the migration has no atomic commit receipt; inspect both stores and reconcile it manually",
        ));
    };
    if found.committed_serial != Some(commit.serial) {
        return Err(CliError::config(
            "the migration manifest has inconsistent commit evidence; refusing rollback",
        ));
    }
    if !source
        .current_lock(tenant)
        .await
        .map_err(fail)?
        .is_some_and(|held| held.identifier() == Some(fence.lock_identifier.as_str()))
    {
        return Err(CliError::config(
            "the source migration fence is missing or different; inspect both stores before rollback",
        ));
    }
    let lock = StateLock::generate();
    let holder = holder::describe("state migrate --rollback");
    let rollback_request = LegacyRollbackRequest {
        tenant,
        binding,
        migration: &found.migration,
        commit,
        lock: &lock,
        writer_nonce: uuid::Uuid::new_v4(),
        holder: &holder,
    };
    let evidence = match target
        .lock_legacy_rollback(&rollback_request)
        .await
        .map_err(fail)?
    {
        LegacyMutation::Applied(evidence) => evidence,
        LegacyMutation::Stale | LegacyMutation::Indeterminate => {
            return Err(CliError::config(
                "the target rollback lock and commit evidence could not be confirmed; the source remains fenced",
            ));
        }
    };
    let moved = evidence.pre_lock_serial != commit.serial;
    if moved && run.request.direction == MigrationDirection::Rollback {
        if !matches!(
            target
                .release_legacy_rollback(&LegacyRollbackAction {
                    rollback: &rollback_request,
                    evidence: &evidence,
                    writer_nonce: uuid::Uuid::new_v4(),
                })
                .await
                .map_err(fail)?,
            LegacyMutation::Applied(())
        ) {
            return Err(CliError::config(
                "the changed target's rollback lock could not be released; the source remains fenced",
            ));
        }
        return Err(CliError::config_with_help(
            format!(
                "{} changed in the target since the migration; rolling back would lose \
                 those changes",
                super::guard::tenant_label(tenant)
            ),
            format!(
                "Copy them back to the source first with `{}`.",
                context
                    .invocation
                    .with_environment(tenant.environment())
                    .command(&format!(
                        "state migrate --confirm-maintenance --source-physical-identity {} --target-physical-identity {} --rollback --copy-back",
                        binding.source_physical_id, binding.target_physical_id
                    ))
            ),
        ));
    }
    if moved {
        let snapshot = match target
            .legacy_rollback_snapshot(&LegacyRollbackAction {
                rollback: &rollback_request,
                evidence: &evidence,
                writer_nonce: uuid::Uuid::new_v4(),
            })
            .await
            .map_err(fail)?
        {
            LegacyMutation::Applied(snapshot) => snapshot,
            LegacyMutation::Stale | LegacyMutation::Indeterminate => {
                return Err(CliError::config(
                    "the locked target snapshot could not be confirmed; both stores remain protected",
                ));
            }
        };
        let plan = LegacyMigrationCopyPlan::try_from_snapshot(&snapshot).map_err(fail)?;
        let access = LegacyMigrationAccess {
            tenant,
            fence: &fence,
        };
        let copy = LegacyMigrationCopy {
            access,
            resources: plan.resources(),
            writer: plan.writer(),
        };
        copy.validate().map_err(fail)?;
        require_legacy_copy(
            source
                .check_legacy_migration_copy(&copy)
                .await
                .map_err(fail)?,
        )?;
        let previous = read_legacy_source(source, &access, context).await?;
        for resource in plan.resources() {
            require_legacy_copy(
                source
                    .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                        access,
                        resource,
                    })
                    .await
                    .map_err(fail)?,
            )?;
        }
        for stale in previous.resources.iter().filter(|record| {
            !plan
                .resources()
                .iter()
                .any(|copy| copy.address == record.address)
        }) {
            require_legacy_copy(
                source
                    .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                        access,
                        address: &stale.address,
                    })
                    .await
                    .map_err(fail)?,
            )?;
        }
        require_legacy_copy(
            source
                .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                    access,
                    writer: plan.writer(),
                })
                .await
                .map_err(fail)?,
        )?;
        let written = read_legacy_source(source, &access, context).await?;
        let mismatched = differences(plan.resources(), &written.resources);
        if !mismatched.is_empty() || written.writer.as_ref() != plan.writer() {
            return Err(CliError::config(format!(
                "copying {} back to the source did not verify at {}; both stores remain protected",
                super::guard::tenant_label(tenant),
                if mismatched.is_empty() {
                    "last-writer metadata".to_string()
                } else {
                    mismatched.join(", ")
                }
            )));
        }
    }
    let back_to = source
        .location(tenant)
        .unwrap_or_else(|| run.source_label.clone());
    if !matches!(
        target
            .tombstone_legacy_rollback(&LegacyTombstoneRequest {
                action: LegacyRollbackAction {
                    rollback: &rollback_request,
                    evidence: &evidence,
                    writer_nonce: uuid::Uuid::new_v4(),
                },
                moved_to: &back_to,
            })
            .await
            .map_err(fail)?,
        LegacyMutation::Applied(())
    ) {
        return Err(CliError::config(
            "the target rollback tombstone could not be confirmed; the source remains fenced",
        ));
    }
    if !source.unlock(tenant, &fence).await.map_err(fail)? {
        return Err(CliError::config(
            "the source fence release could not be confirmed; the target remains protected for reconciliation",
        ));
    }
    if !matches!(
        target
            .release_legacy_rollback(&LegacyRollbackAction {
                rollback: &rollback_request,
                evidence: &evidence,
                writer_nonce: uuid::Uuid::new_v4(),
            })
            .await
            .map_err(fail)?,
        LegacyMutation::Applied(())
    ) {
        return Err(CliError::config(
            "the target rollback lock release could not be confirmed; inspect both stores before continuing",
        ));
    }
    Ok(())
}

fn require_legacy_copy(outcome: MigrationMutation) -> Result<(), CliError> {
    match outcome {
        MigrationMutation::Applied => Ok(()),
        MigrationMutation::Stale | MigrationMutation::Indeterminate => Err(CliError::config(
            "the source copy could not be confirmed; both stores remain protected for reconciliation",
        )),
    }
}

async fn read_legacy_source(
    source: &dyn StateStore,
    access: &LegacyMigrationAccess<'_>,
    context: &CommandContext<'_>,
) -> Result<LegacyMigrationSnapshot, CliError> {
    match source
        .read_legacy_migration_snapshot(access)
        .await
        .map_err(|error| super::failure::failure(&error, context.invocation))?
    {
        LegacyMigrationSnapshotRead::Available(snapshot) => Ok(*snapshot),
        LegacyMigrationSnapshotRead::Stale | LegacyMigrationSnapshotRead::Indeterminate => {
            Err(CliError::config(
                "the fenced source snapshot could not be confirmed; both stores remain protected",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_continues_its_own_migration() {
        let mut manifest = MigrationManifest::default();
        let tenant = TenantKey::try_new("example.com/m", "web").unwrap();
        let first = migration_id(&manifest, std::slice::from_ref(&tenant)).unwrap();
        upsert(
            &mut manifest,
            &tenant,
            MigrationUpdate {
                status: PENDING,
                migration: &first,
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            },
        );
        assert_eq!(
            migration_id(&manifest, std::slice::from_ref(&tenant)).unwrap(),
            first
        );
        upsert(
            &mut manifest,
            &tenant,
            MigrationUpdate {
                status: ROLLED_BACK,
                migration: &first,
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            },
        );
        assert_ne!(
            migration_id(&manifest, std::slice::from_ref(&tenant)).unwrap(),
            first
        );
        assert_eq!(manifest.tenants.len(), 1);
    }

    #[test]
    fn manifest_headroom_covers_every_forward_and_rollback_update() {
        let first = TenantKey::try_new("example.com/m", "web").unwrap();
        let second = TenantKey::with_environment("example.com/m", "web", "prod").unwrap();
        let tenants = [first.clone(), second.clone()];
        let mut manifest = MigrationManifest::default();
        upsert(
            &mut manifest,
            &first,
            MigrationUpdate {
                status: MIGRATED,
                migration: "1234567890abcdef",
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            },
        );
        upsert(
            &mut manifest,
            &second,
            MigrationUpdate {
                status: PENDING,
                migration: "1234567890abcdef",
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            },
        );

        // Forward rewrites pending once, then records each tenant as migrated.
        manifest.serial = Some(u64::MAX - 4);
        assert!(
            require_manifest_headroom(&manifest, &tenants, MigrationDirection::Forward).is_ok()
        );
        manifest.serial = Some(u64::MAX - 3);
        assert!(
            require_manifest_headroom(&manifest, &tenants, MigrationDirection::Forward).is_err()
        );

        // Rollback records one update per tenant.
        assert!(
            require_manifest_headroom(&manifest, &tenants, MigrationDirection::Rollback).is_ok()
        );
        manifest.serial = Some(u64::MAX - 2);
        assert!(
            require_manifest_headroom(&manifest, &tenants, MigrationDirection::Rollback).is_err()
        );
        assert!(
            require_manifest_headroom(&manifest, &tenants, MigrationDirection::CopyBack).is_err()
        );
    }
}
