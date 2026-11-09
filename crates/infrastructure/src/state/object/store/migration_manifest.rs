//! Conditional, per-environment updates to a legacy migration manifest.
//!
//! Claim checks and manifest writes are separate S3 operations. Callers must
//! enforce the maintenance freeze and verify physical backend identities.

use std::collections::BTreeSet;
use std::fmt;

use super::super::client::{Condition, S3Failure};
use super::{ManifestEntry, MigrationManifest, ObjectStateStore};
use crate::error::{InfrastructureError, Result};
use crate::state::BackendIdentity;
use crate::tenant::TenantKey;
use uuid::Uuid;

/// One exact manifest version and its private object-store precondition.
#[derive(Clone)]
pub struct LegacyManifestSnapshot {
    manifest: MigrationManifest,
    project_key: String,
    etag: String,
    connection: BackendIdentity,
}

impl LegacyManifestSnapshot {
    /// The decoded manifest at this exact version.
    #[must_use]
    pub const fn manifest(&self) -> &MigrationManifest {
        &self.manifest
    }
}

impl fmt::Debug for LegacyManifestSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyManifestSnapshot(<redacted>)")
    }
}

/// Replace one environment while retaining every unrelated manifest entry.
#[derive(Clone, Copy)]
pub struct LegacyManifestUpdate<'update> {
    /// Tenant whose environment is being updated.
    pub tenant: &'update TenantKey,
    /// Exact previously read version, or `None` to create a manifest.
    pub previous: Option<&'update LegacyManifestSnapshot>,
    /// Replacement entry for this tenant.
    pub entry: &'update ManifestEntry,
    /// Fresh revision selected before issuing the conditional request.
    pub revision: Uuid,
    /// Fresh write nonce selected before issuing the conditional request.
    pub write_nonce: Uuid,
}

impl fmt::Debug for LegacyManifestUpdate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyManifestUpdate(<redacted>)")
    }
}

/// Outcome of a conditional manifest update.
#[derive(Debug, Clone)]
pub enum LegacyManifestMutation {
    /// The exact requested manifest was observed.
    Applied(Box<LegacyManifestSnapshot>),
    /// The expected manifest version was no longer current.
    Stale,
    /// The request may have applied, but exact evidence was not observed.
    Indeterminate,
}

impl ObjectStateStore {
    fn validate_legacy_manifest(
        &self,
        tenant: &TenantKey,
        manifest: &MigrationManifest,
    ) -> Result<()> {
        match (manifest.revision, manifest.write_nonce, manifest.serial) {
            (None, None, None) => {}
            (Some(revision), Some(nonce), Some(serial))
                if !revision.is_nil()
                    && !nonce.is_nil()
                    && revision != nonce
                    && serial > 0
                    && serial < u64::MAX => {}
            _ => {
                return Err(InfrastructureError::state(
                    "invalid migration manifest revision",
                ));
            }
        }
        let mut seen = BTreeSet::new();
        for entry in &manifest.tenants {
            if !seen.insert(entry.environment.as_deref())
                || !matches!(
                    entry.status.as_str(),
                    "pending" | "migrated" | "rolled-back"
                )
                || entry.migration.len() != 16
                || !entry
                    .migration
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(InfrastructureError::state(
                    "invalid migration manifest entry",
                ));
            }
            let selected = match &entry.environment {
                Some(environment) => {
                    TenantKey::with_environment(tenant.module_path(), tenant.project(), environment)
                }
                None => TenantKey::try_new(tenant.module_path(), tenant.project()),
            }
            .map_err(|_| InfrastructureError::state("invalid migration manifest environment"))?;
            self.path(&selected).map_err(|_| {
                InfrastructureError::state("invalid migration manifest environment")
            })?;
            if let Some(binding) = &entry.binding {
                binding.validate_for(&selected)?;
                if binding.target_root != self.root
                    || binding.target_package != self.package
                    || binding.target_connection != self.legacy_connection_identity()?
                {
                    return Err(InfrastructureError::state(
                        "migration manifest targets a different backend",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Read and validate the exact current project manifest version.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, unsupported, or malformed manifests.
    pub async fn legacy_manifest_snapshot(
        &self,
        tenant: &TenantKey,
    ) -> Result<Option<LegacyManifestSnapshot>> {
        let path = self
            .path(tenant)
            .map_err(|_| InfrastructureError::state("invalid migration project"))?;
        let project_key = path.migration_manifest_key();
        let object = self
            .get(&project_key)
            .await
            .map_err(|_| InfrastructureError::state("cannot read migration manifest"))?;
        let Some(object) = object else {
            return Ok(None);
        };
        let manifest: MigrationManifest = serde_json::from_slice(&object.body)
            .map_err(|_| InfrastructureError::state("migration manifest does not decode"))?;
        self.validate_legacy_manifest(tenant, &manifest)?;
        let connection = self.legacy_connection_identity()?;
        Ok(Some(LegacyManifestSnapshot {
            manifest,
            project_key,
            etag: object.etag,
            connection,
        }))
    }

    /// Conditionally update one environment without overwriting unrelated entries.
    ///
    /// Lost responses are accepted only after reading the exact proposed body.
    /// A later write may erase that evidence, in which case the result is
    /// `Indeterminate` and the caller must not replay it blindly.
    /// Target completion evidence does not prove that the source fence was
    /// released; the caller must independently confirm that release.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input or definite storage failures.
    pub async fn update_legacy_manifest(
        &self,
        request: &LegacyManifestUpdate<'_>,
    ) -> Result<LegacyManifestMutation> {
        if request.revision.is_nil()
            || request.write_nonce.is_nil()
            || request.revision == request.write_nonce
            || request.entry.environment.as_deref() != request.tenant.environment()
            || request.entry.binding.is_none()
        {
            return Err(InfrastructureError::configuration(
                "invalid conditional manifest update",
            ));
        }
        let path = self
            .path(request.tenant)
            .map_err(|_| InfrastructureError::state("invalid migration project"))?;
        let key = path.migration_manifest_key();
        let connection = self.legacy_connection_identity()?;
        let mut manifest = match request.previous {
            Some(previous) if previous.project_key == key && previous.connection == connection => {
                self.validate_legacy_manifest(request.tenant, &previous.manifest)?;
                if [previous.manifest.revision, previous.manifest.write_nonce]
                    .into_iter()
                    .flatten()
                    .any(|old| old == request.revision || old == request.write_nonce)
                {
                    return Err(InfrastructureError::configuration(
                        "manifest identifiers must be fresh",
                    ));
                }
                previous.manifest.clone()
            }
            Some(_) => return Ok(LegacyManifestMutation::Stale),
            None => MigrationManifest::default(),
        };
        let mut verify_commit = false;
        let mut verify_completion = false;
        if let Some(existing) = manifest
            .tenants
            .iter_mut()
            .find(|entry| entry.environment == request.entry.environment)
        {
            if existing.binding.is_none()
                || existing.binding != request.entry.binding
                || existing.migration != request.entry.migration
                || existing.commit_evidence.is_some()
                    && existing.commit_evidence != request.entry.commit_evidence
                || existing.commit_evidence.is_some()
                    && existing.committed_serial != request.entry.committed_serial
            {
                return Err(InfrastructureError::state(
                    "migration manifest binding or receipt cannot change",
                ));
            }
            if existing.status != "pending" && existing.commit_evidence.is_none() {
                return Err(InfrastructureError::state(
                    "a legacy manifest entry has no commit evidence",
                ));
            }
            let permitted = matches!(
                (existing.status.as_str(), request.entry.status.as_str()),
                ("pending", "pending" | "migrated")
                    | ("migrated", "migrated" | "rolled-back")
                    | ("rolled-back", "rolled-back")
            );
            if !permitted
                || (request.entry.status == "migrated" && request.entry.commit_evidence.is_none())
                || (request.entry.status == "pending"
                    && (request.entry.commit_evidence.is_some()
                        || request.entry.committed_serial.is_some()))
                || request
                    .entry
                    .commit_evidence
                    .as_ref()
                    .is_some_and(|evidence| {
                        evidence.writer_nonce.is_nil()
                            || evidence.serial == 0
                            || evidence.serial == u64::MAX
                            || request.entry.committed_serial != Some(evidence.serial)
                    })
            {
                return Err(InfrastructureError::state(
                    "invalid migration manifest transition",
                ));
            }
            verify_commit = existing.status == "pending" && request.entry.status == "migrated";
            verify_completion =
                existing.status == "migrated" && request.entry.status == "rolled-back";
            *existing = request.entry.clone();
        } else {
            if request.entry.status != "pending"
                || request.entry.commit_evidence.is_some()
                || request.entry.committed_serial.is_some()
            {
                return Err(InfrastructureError::state(
                    "a new migration manifest entry must be pending",
                ));
            }
            manifest.tenants.push(request.entry.clone());
        }
        let serial = manifest
            .serial
            .unwrap_or(0)
            .checked_add(1)
            .filter(|value| *value < u64::MAX)
            .ok_or_else(|| InfrastructureError::state("migration manifest serial is exhausted"))?;
        manifest.revision = Some(request.revision);
        manifest.write_nonce = Some(request.write_nonce);
        manifest.serial = Some(serial);
        self.validate_legacy_manifest(request.tenant, &manifest)?;
        let binding =
            request.entry.binding.as_ref().ok_or_else(|| {
                InfrastructureError::state("migration manifest binding is missing")
            })?;
        if verify_commit {
            let evidence = self
                .legacy_commit_evidence(request.tenant, &request.entry.migration, binding)
                .await?;
            if evidence != request.entry.commit_evidence {
                return Err(InfrastructureError::state(
                    "migration target commit evidence does not match",
                ));
            }
        }
        if verify_completion {
            let evidence = request.entry.commit_evidence.as_ref().ok_or_else(|| {
                InfrastructureError::state("migration commit evidence is missing")
            })?;
            let completed = self
                .legacy_rollback_completed(
                    &super::legacy_migration::LegacyRollbackCompletionRequest {
                        tenant: request.tenant,
                        migration: &request.entry.migration,
                        binding,
                        commit: evidence,
                    },
                )
                .await?;
            if !completed {
                return Err(InfrastructureError::state(
                    "migration target rollback is incomplete",
                ));
            }
        }
        let body = serde_json::to_vec(&manifest)
            .map_err(|_| InfrastructureError::state("cannot encode migration manifest"))?;
        let condition = request.previous.map_or(Condition::Absent, |previous| {
            Condition::Matches(previous.etag.clone())
        });
        self.ensure_migration_capability(request.tenant).await?;
        let outcome = self
            .client
            .put(&key, body, &condition)
            .await
            .map_err(|_| InfrastructureError::state("cannot write migration manifest"))?;
        match outcome {
            Ok(etag) => Ok(LegacyManifestMutation::Applied(Box::new(
                LegacyManifestSnapshot {
                    manifest,
                    project_key: key,
                    etag,
                    connection,
                },
            ))),
            Err(S3Failure::PreconditionFailed | S3Failure::Missing) => {
                Ok(match self.legacy_manifest_snapshot(request.tenant).await? {
                    Some(found) if found.manifest == manifest => {
                        LegacyManifestMutation::Applied(Box::new(found))
                    }
                    _ => LegacyManifestMutation::Stale,
                })
            }
            Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                Ok(match self.legacy_manifest_snapshot(request.tenant).await {
                    Ok(Some(found)) if found.manifest == manifest => {
                        LegacyManifestMutation::Applied(Box::new(found))
                    }
                    _ => LegacyManifestMutation::Indeterminate,
                })
            }
            Err(S3Failure::Permanent(_)) => Err(InfrastructureError::state(
                "cannot write migration manifest",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::object::Credentials;
    use crate::state::object::fake::{BarrierPosition, FakeS3, FaultAction};
    use crate::state::object::store::legacy_migration::{
        LegacyCommitEvidence, LegacyCommitRequest, LegacyMigrationBinding,
        LegacyMigrationSelection, LegacyMutation, LegacyRollbackAction, LegacyRollbackRequest,
        LegacyTombstoneRequest,
    };
    use crate::state::object::store::{AddressingChoice, ObjectStoreConfiguration, TenantImport};
    use crate::state::{BackendIdentity, LegacyMigrationSource, StateLock};
    use std::time::Duration;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn store(server: &FakeS3) -> Result<ObjectStateStore> {
        ObjectStateStore::try_new(ObjectStoreConfiguration {
            endpoint: Some(server.endpoint.clone()),
            bucket: "state".into(),
            region: "us-east-1".into(),
            root: None,
            addressing: AddressingChoice::Path,
            credentials: Credentials {
                access_key_id: "access".into(),
                secret_access_key: "secret".into(),
                session_token: None,
            },
            package: "cuenv".into(),
            history: 0,
            minimum_write_interval: None,
            probe_tenant: None,
        })
    }

    fn binding(store: &ObjectStateStore) -> Result<LegacyMigrationBinding> {
        Ok(LegacyMigrationBinding {
            source_physical_id: Uuid::new_v4(),
            target_physical_id: Uuid::new_v4(),
            source: LegacyMigrationSource::Memory {
                connection: BackendIdentity::from_digest([7; 32]),
            },
            target_connection: store.legacy_connection_identity()?,
            target_root: None,
            target_package: "cuenv".into(),
            module_path: "example.com/project".into(),
            project: "api".into(),
            selection: LegacyMigrationSelection::All,
            environments: vec![None, Some("extra".into()), Some("other".into())],
        })
    }

    fn entry(environment: Option<&str>, binding: &LegacyMigrationBinding) -> ManifestEntry {
        ManifestEntry {
            environment: environment.map(ToString::to_string),
            status: "pending".into(),
            migration: "123456789abcdef0".into(),
            committed_serial: None,
            commit_evidence: None,
            binding: Some(binding.clone()),
        }
    }

    fn update<'a>(
        tenant: &'a TenantKey,
        previous: Option<&'a LegacyManifestSnapshot>,
        entry: &'a ManifestEntry,
    ) -> LegacyManifestUpdate<'a> {
        LegacyManifestUpdate {
            tenant,
            previous,
            entry,
            revision: Uuid::new_v4(),
            write_nonce: Uuid::new_v4(),
        }
    }

    #[tokio::test]
    async fn transitions_keep_binding_and_commit_evidence_immutable() -> TestResult {
        let server = FakeS3::start().await?;
        let store = store(&server)?;
        let tenant = TenantKey::try_new("example.com/project", "api")?;
        let binding = binding(&store)?;
        let pending = entry(None, &binding);
        let LegacyManifestMutation::Applied(first) = store
            .update_legacy_manifest(&update(&tenant, None, &pending))
            .await?
        else {
            return Err("initial manifest failed".into());
        };
        let forged = LegacyCommitEvidence {
            writer_nonce: Uuid::new_v4(),
            serial: 42,
        };
        let forged_entry = ManifestEntry {
            status: "migrated".into(),
            committed_serial: Some(42),
            commit_evidence: Some(forged),
            ..pending.clone()
        };
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, Some(&first), &forged_entry))
                .await
                .is_err()
        );
        let import_lock = StateLock::generate();
        assert!(
            store
                .import(
                    &tenant,
                    &TenantImport {
                        lock: &import_lock,
                        holder: "test",
                        resources: &[],
                        last_writer: None,
                        migration: &pending.migration,
                        source: "source",
                    }
                )
                .await?
        );
        let LegacyMutation::Applied(evidence) = store
            .commit_legacy_migration(&LegacyCommitRequest {
                tenant: &tenant,
                binding: &binding,
                migration: &pending.migration,
                lock: &import_lock,
                writer_nonce: Uuid::new_v4(),
            })
            .await?
        else {
            return Err("target commit failed".into());
        };
        let migrated = ManifestEntry {
            status: "migrated".into(),
            committed_serial: Some(evidence.serial),
            commit_evidence: Some(evidence),
            ..pending.clone()
        };
        let LegacyManifestMutation::Applied(second) = store
            .update_legacy_manifest(&update(&tenant, Some(&first), &migrated))
            .await?
        else {
            return Err("commit update failed".into());
        };
        let rolled_back = ManifestEntry {
            status: "rolled-back".into(),
            ..migrated.clone()
        };
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, Some(&second), &rolled_back))
                .await
                .is_err()
        );
        let rollback_lock = StateLock::generate();
        let rollback = LegacyRollbackRequest {
            tenant: &tenant,
            binding: &binding,
            migration: &pending.migration,
            commit: &evidence,
            lock: &rollback_lock,
            writer_nonce: Uuid::new_v4(),
            holder: "test",
        };
        let LegacyMutation::Applied(lock_evidence) = store.lock_legacy_rollback(&rollback).await?
        else {
            return Err("rollback lock failed".into());
        };
        let tombstone_action = LegacyRollbackAction {
            rollback: &rollback,
            evidence: &lock_evidence,
            writer_nonce: Uuid::new_v4(),
        };
        assert!(matches!(
            store
                .tombstone_legacy_rollback(&LegacyTombstoneRequest {
                    action: tombstone_action,
                    moved_to: "source"
                })
                .await?,
            LegacyMutation::Applied(())
        ));
        let release_action = LegacyRollbackAction {
            writer_nonce: Uuid::new_v4(),
            ..tombstone_action
        };
        assert!(matches!(
            store.release_legacy_rollback(&release_action).await?,
            LegacyMutation::Applied(())
        ));
        let LegacyManifestMutation::Applied(last) = store
            .update_legacy_manifest(&update(&tenant, Some(&second), &rolled_back))
            .await?
        else {
            return Err("terminal update failed".into());
        };
        assert_eq!(last.manifest().serial, Some(3));
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, Some(&last), &pending))
                .await
                .is_err()
        );
        let cleared = ManifestEntry {
            committed_serial: None,
            ..rolled_back.clone()
        };
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, Some(&last), &cleared))
                .await
                .is_err()
        );
        let foreign = ManifestEntry {
            binding: Some(self::binding(&store)?),
            ..rolled_back
        };
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, Some(&last), &foreign))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn conditional_updates_preserve_other_environments_after_conflict() -> TestResult {
        let server = FakeS3::start().await?;
        let first_store = store(&server)?;
        let second_store = store(&server)?;
        let tenant = TenantKey::try_new("example.com/project", "api")?;
        let extra = TenantKey::with_environment("example.com/project", "api", "extra")?;
        let other = TenantKey::with_environment("example.com/project", "api", "other")?;
        let binding = binding(&first_store)?;
        let initial = entry(None, &binding);
        let LegacyManifestMutation::Applied(snapshot) = first_store
            .update_legacy_manifest(&update(&tenant, None, &initial))
            .await?
        else {
            return Err("initial manifest failed".into());
        };
        let extra_entry = entry(Some("extra"), &binding);
        let other_entry = entry(Some("other"), &binding);
        let mut barrier = server.pause_next("PUT", "migration", BarrierPosition::BeforeApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            let result = second_store
                .update_legacy_manifest(&update(&other, Some(&snapshot), &other_entry))
                .await?;
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        let extra_update = update(&extra, Some(&snapshot), &extra_entry);
        let (stale, winner) =
            tokio::join!(first_store.update_legacy_manifest(&extra_update), racing);
        assert!(matches!(winner?, LegacyManifestMutation::Applied(_)));
        assert!(matches!(stale?, LegacyManifestMutation::Stale));
        let latest = first_store
            .legacy_manifest_snapshot(&tenant)
            .await?
            .ok_or("missing manifest")?;
        let LegacyManifestMutation::Applied(merged) = first_store
            .update_legacy_manifest(&update(&extra, Some(&latest), &extra_entry))
            .await?
        else {
            return Err("merge failed".into());
        };
        assert_eq!(merged.manifest().tenants.len(), 3);
        assert_eq!(merged.manifest().serial, Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn unknown_manifest_fields_are_never_overwritten() -> TestResult {
        let server = FakeS3::start().await?;
        let store = store(&server)?;
        let tenant = TenantKey::try_new("example.com/project", "api")?;
        let key = store.path(&tenant)?.migration_manifest_key();
        let unknown = br#"{"tenants":[],"future":"keep"}"#.to_vec();
        server.put_object("state", &key, unknown.clone());
        assert!(store.legacy_manifest_snapshot(&tenant).await.is_err());
        let binding = binding(&store)?;
        assert!(
            store
                .update_legacy_manifest(&update(&tenant, None, &entry(None, &binding)))
                .await
                .is_err()
        );
        assert_eq!(server.object("state", &key), Some(unknown));
        Ok(())
    }

    #[tokio::test]
    async fn lost_response_after_a_later_write_is_indeterminate() -> TestResult {
        let server = FakeS3::start().await?;
        let store = store(&server)?;
        let tenant = TenantKey::try_new("example.com/project", "api")?;
        let extra = TenantKey::with_environment("example.com/project", "api", "extra")?;
        let other = TenantKey::with_environment("example.com/project", "api", "other")?;
        let binding = binding(&store)?;
        let initial = entry(None, &binding);
        let LegacyManifestMutation::Applied(snapshot) = store
            .update_legacy_manifest(&update(&tenant, None, &initial))
            .await?
        else {
            return Err("initial manifest failed".into());
        };
        let extra_entry = entry(Some("extra"), &binding);
        let other_entry = entry(Some("other"), &binding);
        server.fault("PUT", "migration", FaultAction::DropAfterApply);
        let mut barrier = server.pause_next("PUT", "migration", BarrierPosition::AfterApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            let next = store
                .legacy_manifest_snapshot(&tenant)
                .await?
                .ok_or("missing manifest")?;
            assert!(matches!(
                store
                    .update_legacy_manifest(&update(&other, Some(&next), &other_entry))
                    .await?,
                LegacyManifestMutation::Applied(_)
            ));
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let extra_update = update(&extra, Some(&snapshot), &extra_entry);
        let (result, race) = tokio::join!(store.update_legacy_manifest(&extra_update), racing);
        race?;
        assert!(matches!(result?, LegacyManifestMutation::Indeterminate));
        let current = store
            .legacy_manifest_snapshot(&tenant)
            .await?
            .ok_or("missing manifest")?;
        assert_eq!(current.manifest().tenants.len(), 3);
        Ok(())
    }
}
