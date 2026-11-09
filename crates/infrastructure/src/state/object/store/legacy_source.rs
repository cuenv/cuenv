//! Exact source-state changes under a legacy tenant fence.
//!
//! A tenant's fence check and data mutation share one conditional object
//! write. The separate project freeze is required to stop other tenants and
//! obsolete clients throughout a legacy copy-back.

use super::super::client::{Condition, S3Failure};
use super::super::document::{DocumentWriter, StateDocument};
use super::ObjectStateStore;
use super::object_state_store::{self, MUTATION_ATTEMPTS};
use crate::error::{InfrastructureError, Result};

use crate::state::{
    LastWriter, LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationDeleteRequest,
    LegacyMigrationRecordRequest, LegacyMigrationSnapshot, LegacyMigrationSnapshotRead,
    LegacyMigrationWriterRequest, ManagedResource, MigrationMutation, ResourceAddress,
};
use uuid::Uuid;

enum SourceChange<'change> {
    Record(&'change ManagedResource),
    Delete(&'change ResourceAddress),
    Writer(Option<&'change LastWriter>),
}

struct SourceMutation<'mutation> {
    access: LegacyMigrationAccess<'mutation>,
    change: SourceChange<'mutation>,
}

fn source_held(document: &StateDocument, access: LegacyMigrationAccess<'_>) -> bool {
    document.tombstone.is_none()
        && document.lock.as_ref().is_some_and(|held| {
            held.is_ordinary(document.format.write)
                && held.lock_identifier == access.fence.lock_identifier
        })
}

fn validate_source_records(document: &StateDocument) -> Result<()> {
    crate::state::legacy_copy::decode_legacy_writer(document.last_writer.as_ref())?;
    crate::state::legacy_copy::decode_legacy_records(&document.records)?;
    Ok(())
}

fn writer_value(writer: Option<&LastWriter>) -> Option<DocumentWriter> {
    let writer = writer?;
    Some(DocumentWriter {
        instance: writer.instance.as_str().to_string(),
        at: writer.recorded_at.clone(),
        extra: serde_json::Map::new(),
    })
}

fn matches_change(document: &StateDocument, change: &SourceChange<'_>) -> Result<bool> {
    match change {
        SourceChange::Record(resource) => {
            Ok(document.resource(&resource.address)?.as_ref() == Some(*resource))
        }
        SourceChange::Delete(address) => Ok(!document.records.contains_key(&address.to_string())),
        SourceChange::Writer(writer) => Ok(document.last_writer == writer_value(*writer)),
    }
}

fn apply_change(document: &mut StateDocument, change: &SourceChange<'_>) -> Result<()> {
    match change {
        SourceChange::Record(resource) => document.store(resource),
        SourceChange::Delete(address) => {
            document.records.remove(&address.to_string());
            Ok(())
        }
        SourceChange::Writer(writer) => {
            document.last_writer = writer_value(*writer);
            Ok(())
        }
    }
}

fn has_headroom(document: &StateDocument, mutations: usize) -> Result<()> {
    let mutations = u64::try_from(mutations)
        .map_err(|_| InfrastructureError::state("legacy migration copy is too large"))?;
    if document
        .serial
        .checked_add(mutations)
        .is_none_or(|serial| serial == u64::MAX)
    {
        return Err(InfrastructureError::state(
            "legacy source serial is exhausted",
        ));
    }
    Ok(())
}

impl ObjectStateStore {
    pub(super) async fn read_legacy_source_snapshot(
        &self,
        access: &LegacyMigrationAccess<'_>,
    ) -> Result<LegacyMigrationSnapshotRead> {
        access.validate()?;
        self.ensure_migration_capability(access.tenant).await?;
        let path = self
            .path(access.tenant)
            .map_err(|_| InfrastructureError::state("invalid legacy source"))?;
        let document = match self.read(&path).await {
            Ok(Some((document, _))) => document,
            Ok(None) => return Ok(LegacyMigrationSnapshotRead::Stale),
            Err(_) => return Ok(LegacyMigrationSnapshotRead::Indeterminate),
        };
        if !source_held(&document, *access) {
            return Ok(LegacyMigrationSnapshotRead::Stale);
        }
        document
            .require_writable(&path.state_key())
            .map_err(|_| InfrastructureError::state("legacy source format is unsupported"))?;
        if document.origin.is_some() {
            return Err(InfrastructureError::state(
                "legacy import cannot represent source provenance",
            ));
        }
        let lock = document
            .lock
            .as_ref()
            .ok_or_else(|| InfrastructureError::state("legacy source lock is missing"))?;
        let mut lock_extensions = lock.extra.clone();
        lock_extensions.remove("cuenvLockKind");
        let change_extensions = lock.last_change.as_ref().map(|change| &change.extra);
        crate::state::legacy_copy::ensure_no_legacy_extensions(&[
            &document.extra,
            &document.format.extra,
            &document.tenant.extra,
            &lock_extensions,
        ])?;
        if let Some(extensions) = change_extensions {
            crate::state::legacy_copy::ensure_no_legacy_extensions(&[extensions])?;
        }
        let resources = crate::state::legacy_copy::decode_legacy_records(&document.records)?;
        let writer =
            crate::state::legacy_copy::decode_legacy_writer(document.last_writer.as_ref())?;
        Ok(LegacyMigrationSnapshotRead::Available(Box::new(
            LegacyMigrationSnapshot { resources, writer },
        )))
    }

    pub(super) async fn check_legacy_source_copy(
        &self,
        request: &LegacyMigrationCopy<'_>,
    ) -> Result<MigrationMutation> {
        request.validate()?;
        self.ensure_migration_capability(request.access.tenant)
            .await?;
        let path = self.path(request.access.tenant)?;
        let Some((document, _)) = self
            .read(&path)
            .await
            .map_err(|_| InfrastructureError::state("cannot read legacy source"))?
        else {
            return Ok(MigrationMutation::Stale);
        };
        if !source_held(&document, request.access) {
            return Ok(MigrationMutation::Stale);
        }
        document
            .require_writable(&path.state_key())
            .map_err(|_| InfrastructureError::state("legacy source format is unsupported"))?;
        validate_source_records(&document)?;
        let mutations = document
            .records
            .len()
            .checked_add(request.resources.len())
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| InfrastructureError::state("legacy migration copy is too large"))?;
        has_headroom(&document, mutations)?;
        Ok(MigrationMutation::Applied)
    }

    pub(super) async fn replace_legacy_source_record(
        &self,
        request: &LegacyMigrationRecordRequest<'_>,
    ) -> Result<MigrationMutation> {
        request.validate()?;
        self.mutate_legacy_source(&SourceMutation {
            access: request.access,
            change: SourceChange::Record(request.resource),
        })
        .await
    }

    pub(super) async fn delete_legacy_source_record(
        &self,
        request: &LegacyMigrationDeleteRequest<'_>,
    ) -> Result<MigrationMutation> {
        request.access.validate()?;
        if ResourceAddress::parse(&request.address.to_string()).as_ref() != Some(request.address) {
            return Err(InfrastructureError::configuration(
                "invalid legacy migration resource address",
            ));
        }
        self.mutate_legacy_source(&SourceMutation {
            access: request.access,
            change: SourceChange::Delete(request.address),
        })
        .await
    }

    pub(super) async fn set_legacy_source_writer(
        &self,
        request: &LegacyMigrationWriterRequest<'_>,
    ) -> Result<MigrationMutation> {
        request.access.validate()?;
        self.mutate_legacy_source(&SourceMutation {
            access: request.access,
            change: SourceChange::Writer(request.writer),
        })
        .await
    }

    async fn mutate_legacy_source(
        &self,
        operation: &SourceMutation<'_>,
    ) -> Result<MigrationMutation> {
        self.ensure_migration_capability(operation.access.tenant)
            .await?;
        let path = self.path(operation.access.tenant)?;
        let key = path.state_key();
        for _ in 0..MUTATION_ATTEMPTS {
            let Some((mut document, etag)) = self
                .read(&path)
                .await
                .map_err(|_| InfrastructureError::state("cannot read legacy source"))?
            else {
                return Ok(MigrationMutation::Stale);
            };
            if !source_held(&document, operation.access) {
                return Ok(MigrationMutation::Stale);
            }
            document
                .require_writable(&key)
                .map_err(|_| InfrastructureError::state("legacy source format is unsupported"))?;
            validate_source_records(&document)?;
            if matches_change(&document, &operation.change)? {
                return Ok(MigrationMutation::Applied);
            }
            has_headroom(&document, 1)?;
            let nonce = loop {
                let candidate = Uuid::new_v4();
                let suffix = operation
                    .access
                    .fence
                    .lock_identifier
                    .strip_prefix("migrated-")
                    .unwrap_or_default();
                if Uuid::parse_str(suffix).ok() != Some(candidate)
                    && document.writer != candidate.to_string()
                {
                    break candidate;
                }
            };
            apply_change(&mut document, &operation.change)?;
            document.advance(&nonce.to_string())?;
            let body = object_state_store::encode_bounded(&document, &key)
                .map_err(|_| InfrastructureError::state("cannot encode legacy source"))?;
            match self
                .client
                .put(&key, body, &Condition::Matches(etag))
                .await
                .map_err(|_| InfrastructureError::state("cannot write legacy source"))?
            {
                Ok(_) => return Ok(MigrationMutation::Applied),
                Err(S3Failure::PreconditionFailed) => {}
                Err(S3Failure::Missing) => return Ok(MigrationMutation::Stale),
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    return Ok(match self.read(&path).await {
                        Ok(Some((found, _)))
                            if found.writer == nonce.to_string()
                                && source_held(&found, operation.access)
                                && matches_change(&found, &operation.change).unwrap_or(false) =>
                        {
                            MigrationMutation::Applied
                        }
                        _ => MigrationMutation::Indeterminate,
                    });
                }
                Err(S3Failure::Permanent(_)) => {
                    return Err(InfrastructureError::state("cannot write legacy source"));
                }
            }
        }
        Ok(MigrationMutation::Indeterminate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::object::Credentials;
    use crate::state::object::fake::{BarrierPosition, FakeS3, FaultAction};
    use crate::state::object::store::{AddressingChoice, ObjectStoreConfiguration};
    use crate::state::{LockRequest, StateLock, StateStore};
    use crate::tenant::TenantKey;
    use serde_json::{Value, json};
    use std::time::Duration;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Fixture {
        server: FakeS3,
        store: ObjectStateStore,
        tenant: TenantKey,
        fence: StateLock,
    }

    impl Fixture {
        async fn new() -> TestResult<Self> {
            let server = FakeS3::start().await?;
            let tenant = TenantKey::try_new("example.com/source", "api")?;
            let store = ObjectStateStore::try_new(ObjectStoreConfiguration {
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
            })?;
            let fence = StateLock {
                lock_identifier: format!("migrated-{}", Uuid::new_v4()),
            };
            store
                .acquire_lock(
                    &tenant,
                    &LockRequest {
                        lock: &fence,
                        holder: "migration",
                        instance: None,
                    },
                )
                .await?;
            Ok(Self {
                server,
                store,
                tenant,
                fence,
            })
        }

        fn access(&self) -> LegacyMigrationAccess<'_> {
            LegacyMigrationAccess {
                tenant: &self.tenant,
                fence: &self.fence,
            }
        }

        fn resource() -> ManagedResource {
            ManagedResource {
                address: ResourceAddress::new("random_pet", "one"),
                provider: "random".into(),
                provider_source: "registry.example/random".into(),
                schema_version: 0,
                state: json!({"value": "target"}),
                private: Vec::new(),
                dependencies: Vec::new(),
                sensitive_attributes: Vec::new(),
                tainted: false,
                identity: None,
                serial: 7,
                generation: Uuid::new_v4(),
            }
        }
    }

    #[tokio::test]
    async fn exact_copy_preserves_versions_and_requires_the_held_fence() -> TestResult {
        let fixture = Fixture::new().await?;
        let expected_connection = fixture.store.legacy_connection_identity()?;
        assert!(
            matches!(fixture.store.legacy_migration_source(), Some(crate::state::LegacyMigrationSource::Object { connection, root: None, package }) if connection == expected_connection && package == "cuenv")
        );
        let resource = Fixture::resource();
        let copy = LegacyMigrationCopy {
            access: fixture.access(),
            resources: std::slice::from_ref(&resource),
            writer: None,
        };
        assert_eq!(
            fixture.store.check_legacy_migration_copy(&copy).await?,
            MigrationMutation::Applied
        );
        assert_eq!(
            fixture
                .store
                .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                    access: fixture.access(),
                    resource: &resource
                })
                .await?,
            MigrationMutation::Applied
        );
        assert_eq!(
            fixture.store.list(&fixture.tenant).await?.as_slice(),
            std::slice::from_ref(&resource)
        );
        let wrong = StateLock {
            lock_identifier: "migrated-foreign".into(),
        };
        let access = LegacyMigrationAccess {
            tenant: &fixture.tenant,
            fence: &wrong,
        };
        assert_eq!(
            fixture
                .store
                .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                    access,
                    address: &resource.address
                })
                .await?,
            MigrationMutation::Stale
        );
        assert_eq!(
            fixture.store.list(&fixture.tenant).await?.as_slice(),
            std::slice::from_ref(&resource)
        );
        assert_eq!(
            fixture
                .store
                .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                    access: fixture.access(),
                    address: &resource.address
                })
                .await?,
            MigrationMutation::Applied
        );
        assert!(fixture.store.list(&fixture.tenant).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn full_copy_preflight_rejects_extended_source_before_any_change() -> TestResult {
        let fixture = Fixture::new().await?;
        let resource = Fixture::resource();
        fixture
            .store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access: fixture.access(),
                resource: &resource,
            })
            .await?;
        let path = fixture.store.path(&fixture.tenant)?;
        let key = path.state_key();
        let mut raw: Value = serde_json::from_slice(
            &fixture
                .server
                .object("state", &key)
                .ok_or("missing source")?,
        )?;
        raw["records"]["random_pet.one"]["futureExtension"] = json!("must survive");
        let before = serde_json::to_vec(&raw)?;
        fixture.server.put_object("state", &key, before.clone());
        let copy = LegacyMigrationCopy {
            access: fixture.access(),
            resources: &[],
            writer: None,
        };
        assert!(
            fixture
                .store
                .check_legacy_migration_copy(&copy)
                .await
                .is_err()
        );
        assert!(
            fixture
                .store
                .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                    access: fixture.access(),
                    address: &resource.address
                })
                .await
                .is_err()
        );
        assert_eq!(fixture.server.object("state", &key), Some(before));
        Ok(())
    }

    #[tokio::test]
    async fn forward_snapshot_is_fenced_and_rejects_lossy_extensions() -> TestResult {
        let fixture = Fixture::new().await?;
        let resource = Fixture::resource();
        fixture
            .store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access: fixture.access(),
                resource: &resource,
            })
            .await?;
        let result = fixture
            .store
            .read_legacy_migration_snapshot(&fixture.access())
            .await?;
        let LegacyMigrationSnapshotRead::Available(snapshot) = result else {
            return Err("source snapshot unavailable".into());
        };
        assert_eq!(snapshot.resources, [resource]);
        let wrong = StateLock {
            lock_identifier: "migrated-foreign".into(),
        };
        assert!(matches!(
            fixture
                .store
                .read_legacy_migration_snapshot(&LegacyMigrationAccess {
                    tenant: &fixture.tenant,
                    fence: &wrong
                })
                .await?,
            LegacyMigrationSnapshotRead::Stale
        ));
        let key = fixture.store.path(&fixture.tenant)?.state_key();
        let clean: Value = serde_json::from_slice(
            &fixture
                .server
                .object("state", &key)
                .ok_or("missing source")?,
        )?;
        for extension in ["document", "record", "writer"] {
            let mut raw = clean.clone();
            match extension {
                "document" => raw["futureDocument"] = json!("secret"),
                "record" => raw["records"]["random_pet.one"]["futureRecord"] = json!("secret"),
                _ => {
                    raw["lastWriter"] =
                        json!({"instance": "project:cuenv", "at": "now", "futureWriter": "secret"});
                }
            }
            let before = serde_json::to_vec(&raw)?;
            fixture.server.put_object("state", &key, before.clone());
            assert!(
                fixture
                    .store
                    .read_legacy_migration_snapshot(&fixture.access())
                    .await
                    .is_err()
            );
            assert_eq!(fixture.server.object("state", &key), Some(before));
        }
        Ok(())
    }

    #[tokio::test]
    async fn lost_response_requires_exact_writer_and_effect_evidence() -> TestResult {
        let fixture = Fixture::new().await?;
        let resource = Fixture::resource();
        let copy = LegacyMigrationCopy {
            access: fixture.access(),
            resources: std::slice::from_ref(&resource),
            writer: None,
        };
        fixture.store.check_legacy_migration_copy(&copy).await?;
        fixture
            .server
            .fault("PUT", "state.json", FaultAction::DropAfterApply);
        let mut barrier =
            fixture
                .server
                .pause_next("PUT", "state.json", BarrierPosition::AfterApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            let key = fixture.store.path(&fixture.tenant)?.state_key();
            let mut raw: Value = serde_json::from_slice(
                &fixture
                    .server
                    .object("state", &key)
                    .ok_or("missing source")?,
            )?;
            raw["writer"] = json!("later-writer");
            fixture
                .server
                .put_object("state", &key, serde_json::to_vec(&raw)?);
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let request = LegacyMigrationRecordRequest {
            access: fixture.access(),
            resource: &resource,
        };
        let (result, race) = tokio::join!(
            fixture.store.replace_legacy_migration_record(&request),
            racing
        );
        race?;
        assert_eq!(result?, MigrationMutation::Indeterminate);
        Ok(())
    }

    #[tokio::test]
    async fn fence_change_between_read_and_cas_refuses_the_write() -> TestResult {
        let fixture = Fixture::new().await?;
        let resource = Fixture::resource();
        let copy = LegacyMigrationCopy {
            access: fixture.access(),
            resources: std::slice::from_ref(&resource),
            writer: None,
        };
        fixture.store.check_legacy_migration_copy(&copy).await?;
        let mut barrier =
            fixture
                .server
                .pause_next("PUT", "state.json", BarrierPosition::BeforeApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            let key = fixture.store.path(&fixture.tenant)?.state_key();
            let mut raw: Value = serde_json::from_slice(
                &fixture
                    .server
                    .object("state", &key)
                    .ok_or("missing source")?,
            )?;
            raw["lock"]["lockIdentifier"] = json!("migrated-foreign");
            fixture
                .server
                .put_object("state", &key, serde_json::to_vec(&raw)?);
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let request = LegacyMigrationRecordRequest {
            access: fixture.access(),
            resource: &resource,
        };
        let (result, race) = tokio::join!(
            fixture.store.replace_legacy_migration_record(&request),
            racing
        );
        race?;
        assert_eq!(result?, MigrationMutation::Stale);
        Ok(())
    }
}
