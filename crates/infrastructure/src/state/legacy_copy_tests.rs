use super::LegacyMigrationCopyPlan;
use crate::state::object::document::{DocumentWriter, Tombstone};
use crate::state::object::store::legacy_migration::LegacyRollbackSnapshot;
use crate::state::{
    LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationDeleteRequest,
    LegacyMigrationRecordRequest, LegacyMigrationSnapshotRead, LegacyMigrationWriterRequest,
    LockRequest, ManagedResource, MemoryStateStore, MigrationMutation, ResourceAddress, StateLock,
    StateStore,
};
use crate::tenant::TenantKey;
use serde_json::{Map, json};
use std::collections::BTreeMap;
use uuid::Uuid;

fn record() -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new("random_pet", "pet"),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 3,
        state: json!({"sensitive": "provider-secret", "nested": {"future": 7}}),
        private: vec![0, 255],
        dependencies: vec!["random_pet.other".into()],
        sensitive_attributes: vec!["sensitive".into()],
        tainted: true,
        identity: Some(json!({"providerDefined": {"field": "value"}})),
        serial: 7,
        generation: Uuid::new_v4(),
    }
}

fn snapshot(resource: &ManagedResource) -> LegacyRollbackSnapshot {
    LegacyRollbackSnapshot {
        records: BTreeMap::from([(
            resource.address.to_string(),
            serde_json::to_value(resource).unwrap(),
        )]),
        last_writer: Some(DocumentWriter {
            instance: "services/web:cuenv".into(),
            at: "2026-01-01T00:00:00Z".into(),
            extra: Map::new(),
        }),
        document_extensions: Map::new(),
        format_extensions: Map::new(),
        tenant_extensions: Map::new(),
        origin_extensions: Map::new(),
        lock_extensions: Map::new(),
        lock_change_extensions: Map::new(),
        tombstone: None,
        tombstone_extensions: Map::new(),
    }
}

#[test]
fn preserves_exact_records_and_writer_without_disclosing_values() {
    let resource = record();
    let plan = LegacyMigrationCopyPlan::try_from_snapshot(&snapshot(&resource)).unwrap();
    assert_eq!(plan.resources(), &[resource]);
    let writer = plan.writer().unwrap();
    assert_eq!(writer.instance.as_str(), "services/web:cuenv");
    assert_eq!(writer.recorded_at, "2026-01-01T00:00:00Z");
    assert!(!format!("{plan:?}").contains("provider-secret"));
}

#[test]
fn rejects_every_unrepresentable_metadata_extension_and_tombstone() {
    let base = snapshot(&record());
    for index in 0..8 {
        let mut candidate = base.clone();
        let extensions = match index {
            0 => &mut candidate.document_extensions,
            1 => &mut candidate.format_extensions,
            2 => &mut candidate.tenant_extensions,
            3 => &mut candidate.origin_extensions,
            4 => &mut candidate.lock_extensions,
            5 => &mut candidate.lock_change_extensions,
            6 => &mut candidate.tombstone_extensions,
            _ => &mut candidate.last_writer.as_mut().unwrap().extra,
        };
        extensions.insert("future".into(), json!("private-extension-value"));
        let error = LegacyMigrationCopyPlan::try_from_snapshot(&candidate).unwrap_err();
        assert!(!error.to_string().contains("private-extension-value"));
    }
    let mut tombstoned = base;
    tombstoned.tombstone = Some(Tombstone {
        moved_to: "elsewhere".into(),
        extra: Map::new(),
    });
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&tombstoned).is_err());
}

#[test]
fn rejects_record_extensions_and_mismatched_addresses() {
    let base = snapshot(&record());
    let mut record_extension = base.clone();
    record_extension.records.get_mut("random_pet.pet").unwrap()["future"] = json!("secret");
    let error = LegacyMigrationCopyPlan::try_from_snapshot(&record_extension).unwrap_err();
    assert!(!error.to_string().contains("secret"));
    let mut address_extension = base.clone();
    address_extension.records.get_mut("random_pet.pet").unwrap()["address"]["future"] = json!(1);
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&address_extension).is_err());
    let mut wrong_key = base;
    let value = wrong_key.records.remove("random_pet.pet").unwrap();
    wrong_key.records.insert("random_pet.other".into(), value);
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&wrong_key).is_err());
}

#[test]
fn rejects_unrepresentable_versions_and_writer_changes() {
    let mut resource = record();
    resource.generation = Uuid::nil();
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&snapshot(&resource)).is_err());
    resource.generation = Uuid::new_v4();
    resource.serial = 1_i64 << 53;
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&snapshot(&resource)).is_err());
    resource.serial = 1;
    resource.schema_version = 1_i64 << 53;
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&snapshot(&resource)).is_err());
    resource.schema_version = 0;
    let mut malformed = snapshot(&resource);
    malformed.last_writer.as_mut().unwrap().instance = "invalid\nwriter:cuenv".into();
    assert!(LegacyMigrationCopyPlan::try_from_snapshot(&malformed).is_err());
}

#[tokio::test]
async fn memory_copy_preserves_versions_and_rejects_stale_fences() {
    let store = MemoryStateStore::new();
    let tenant = TenantKey::try_new("example.com/memory", "web").unwrap();
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
        .await
        .unwrap();
    let access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &fence,
    };
    let resource = record();
    let plan = LegacyMigrationCopyPlan::try_from_snapshot(&snapshot(&resource)).unwrap();
    let copy = LegacyMigrationCopy {
        access,
        resources: plan.resources(),
        writer: plan.writer(),
    };
    assert_eq!(
        store.check_legacy_migration_copy(&copy).await.unwrap(),
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access,
                resource: &resource
            })
            .await
            .unwrap(),
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                access,
                writer: plan.writer()
            })
            .await
            .unwrap(),
        MigrationMutation::Applied
    );
    match store.read_legacy_migration_snapshot(&access).await.unwrap() {
        LegacyMigrationSnapshotRead::Available(snapshot) => {
            assert_eq!(
                snapshot.resources.as_slice(),
                std::slice::from_ref(&resource)
            );
            assert_eq!(snapshot.writer.as_ref(), plan.writer());
        }
        other => panic!("unexpected snapshot: {other:?}"),
    }
    let other = StateLock {
        lock_identifier: format!("migrated-{}", Uuid::new_v4()),
    };
    let stale = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &other,
    };
    assert_eq!(
        store
            .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                access: stale,
                address: &resource.address
            })
            .await
            .unwrap(),
        MigrationMutation::Stale
    );
    assert_eq!(
        store.list(&tenant).await.unwrap().as_slice(),
        std::slice::from_ref(&resource)
    );
    assert_eq!(
        store
            .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                access,
                address: &resource.address
            })
            .await
            .unwrap(),
        MigrationMutation::Applied
    );
}
