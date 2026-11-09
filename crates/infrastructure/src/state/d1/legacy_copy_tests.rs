use super::{EmulatedD1, LockRequest, StateLock, StateStore};

use crate::state::object::fake::FakeS3;
use crate::state::object::key::TenantPath;
use crate::state::object::{
    AddressingChoice, Credentials, ObjectStateStore, ObjectStoreConfiguration,
};
use crate::state::{
    ConditionalPut, LastWriter, LegacyMigrationAccess, LegacyMigrationCopy,
    LegacyMigrationDeleteRequest, LegacyMigrationRecordRequest, LegacyMigrationSnapshot,
    LegacyMigrationSnapshotRead, LegacyMigrationWriterRequest, MigrationMutation, RecordVersion,
};
use crate::tenant::{ProjectInstance, TenantKey};
use crate::type_system::{self, Type};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn take(store: &super::D1StateStore, tenant: &TenantKey) -> TestResult<StateLock> {
    let fence = StateLock {
        lock_identifier: format!("migrated-{}", Uuid::new_v4()),
    };
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &fence,
                holder: "legacy migration",
                instance: None,
            },
        )
        .await?;
    Ok(fence)
}

fn object_store(
    server: &FakeS3,
    package: &str,
    tenant: &TenantKey,
) -> TestResult<ObjectStateStore> {
    Ok(ObjectStateStore::try_new(ObjectStoreConfiguration {
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
        package: package.into(),
        history: 0,
        minimum_write_interval: None,
        probe_tenant: Some(tenant.clone()),
    })?)
}

async fn take_object(
    store: &ObjectStateStore,
    tenant: &TenantKey,
    instance: Option<&ProjectInstance>,
) -> TestResult<StateLock> {
    let fence = StateLock {
        lock_identifier: format!("migrated-{}", Uuid::new_v4()),
    };
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &fence,
                holder: "legacy migration",
                instance,
            },
        )
        .await?;
    Ok(fence)
}

async fn copy_snapshot(
    store: &dyn StateStore,
    access: LegacyMigrationAccess<'_>,
    snapshot: &LegacyMigrationSnapshot,
) -> TestResult<LegacyMigrationSnapshot> {
    assert_eq!(
        store
            .check_legacy_migration_copy(&LegacyMigrationCopy {
                access,
                resources: &snapshot.resources,
                writer: snapshot.writer.as_ref(),
            })
            .await?,
        MigrationMutation::Applied
    );
    for resource in &snapshot.resources {
        assert_eq!(
            store
                .replace_legacy_migration_record(&LegacyMigrationRecordRequest { access, resource })
                .await?,
            MigrationMutation::Applied
        );
    }
    assert_eq!(
        store
            .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                access,
                writer: snapshot.writer.as_ref(),
            })
            .await?,
        MigrationMutation::Applied
    );
    match store.read_legacy_migration_snapshot(&access).await? {
        LegacyMigrationSnapshotRead::Available(found) => {
            assert_eq!(*found, *snapshot);
            Ok(*found)
        }
        other => Err(format!("unexpected snapshot: {other:?}").into()),
    }
}

#[tokio::test]
async fn s3_sql_s3_copy_preserves_exact_state_and_refuses_unsafe_numbers() -> TestResult {
    let server = FakeS3::start().await?;
    let database = EmulatedD1::new();
    let d1_server = database.serve().await;
    let sql = super::store_at(&d1_server);
    sql.migrate().await?;
    let tenant = TenantKey::try_new("example.com/precision", "roundtrip")?;
    let source = object_store(&server, "source", &tenant)?;
    let destination = object_store(&server, "destination", &tenant)?;
    source.migrate().await?;
    destination.migrate().await?;
    let instance = ProjectInstance::try_new("services/web", "cuenv")?;
    let source_fence = take_object(&source, &tenant, Some(&instance)).await?;
    let mut resource = super::record("pet");
    resource.state = serde_json::json!({
        "raw": {"$serde_json::private::RawValue": "42"},
        "number": {"$serde_json::private::Number": "42"},
        "integer": u64::MAX,
        "binary_exact": 0.125,
        "large_cty_number": "18446744073709551617",
        "precise_cty_number": "0.123456789012345678901234567890123456789"
    });
    resource.identity = Some(serde_json::json!({
        "$serde_json::private::RawValue": "42"
    }));
    source.put(&tenant, &source_fence, &resource).await?;
    let source_access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &source_fence,
    };
    let snapshot = match source
        .read_legacy_migration_snapshot(&source_access)
        .await?
    {
        LegacyMigrationSnapshotRead::Available(snapshot) => snapshot,
        other => return Err(format!("unexpected snapshot: {other:?}").into()),
    };
    assert_eq!(snapshot.resources[0].state, resource.state);
    assert_eq!(snapshot.resources[0].identity, resource.identity);
    assert!(snapshot.writer.is_some());

    let sql_fence = take(&sql, &tenant).await?;
    let sql_access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &sql_fence,
    };
    let sql_snapshot = copy_snapshot(&sql, sql_access, &snapshot).await?;
    let destination_fence = take_object(&destination, &tenant, None).await?;
    let destination_access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &destination_fence,
    };
    copy_snapshot(&destination, destination_access, &sql_snapshot).await?;
    let restored = destination.list(&tenant).await?.remove(0);
    assert_eq!(restored.state, resource.state);
    assert_eq!(restored.identity, resource.identity);
    for name in ["large_cty_number", "precise_cty_number"] {
        let encoded = serde_json::to_vec(&restored.state[name])?;
        let decoded = type_system::from_json_bytes(&encoded, &Type::Number)?;
        assert_eq!(decoded.to_state_json(&Type::Number)?, restored.state[name]);
    }

    let unsafe_tenant = TenantKey::try_new("example.com/precision", "unsafe")?;
    let unsafe_source = object_store(&server, "unsafe", &unsafe_tenant)?;
    unsafe_source.migrate().await?;
    let unsafe_fence = take_object(&unsafe_source, &unsafe_tenant, None).await?;
    let mut unsafe_record = super::record("pet");
    unsafe_record.state = serde_json::json!({"value": "numeric-marker"});
    unsafe_source
        .put(&unsafe_tenant, &unsafe_fence, &unsafe_record)
        .await?;
    let path = TenantPath::try_new(None, "unsafe", &unsafe_tenant)?;
    let key = path.state_key();
    let original = String::from_utf8(server.object("state", &key).ok_or("missing source")?)?;
    let access = LegacyMigrationAccess {
        tenant: &unsafe_tenant,
        fence: &unsafe_fence,
    };
    for number in [
        "18446744073709551617",
        "0.123456789012345678901234567890123456789",
    ] {
        let unsafe_body = original.replace("\"numeric-marker\"", number).into_bytes();
        server.put_object("state", &key, unsafe_body.clone());
        assert!(matches!(
            unsafe_source
                .read_legacy_migration_snapshot(&access)
                .await?,
            LegacyMigrationSnapshotRead::Indeterminate
        ));
        assert_eq!(server.object("state", &key), Some(unsafe_body));
        assert!(sql.list(&unsafe_tenant).await?.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn exact_copy_checks_snapshot_fence_and_preserves_versions() -> TestResult {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = super::store_at(&server);
    store.migrate().await?;
    let tenant = TenantKey::with_environment("example.com/copy", "web", "dev")?;
    let fence = take(&store, &tenant).await?;
    let access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &fence,
    };
    assert!(
        matches!(store.read_legacy_migration_snapshot(&access).await?, LegacyMigrationSnapshotRead::Available(snapshot) if snapshot.resources.is_empty() && snapshot.writer.is_none())
    );
    let stale_lock = StateLock {
        lock_identifier: format!("migrated-{}", Uuid::new_v4()),
    };
    let stale = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &stale_lock,
    };
    assert!(matches!(
        store.read_legacy_migration_snapshot(&stale).await?,
        LegacyMigrationSnapshotRead::Stale
    ));
    let mut desired = super::record("pet");
    desired.serial = 17;
    desired.generation = Uuid::new_v4();
    let writer = LastWriter {
        instance: ProjectInstance::try_new("app", "cuenv")?,
        recorded_at: "2026-01-01T00:00:00Z".into(),
    };
    let records = [desired.clone()];
    assert_eq!(
        store
            .check_legacy_migration_copy(&LegacyMigrationCopy {
                access,
                resources: &records,
                writer: Some(&writer)
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access,
                resource: &desired
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                access,
                writer: Some(&writer)
            })
            .await?,
        MigrationMutation::Applied
    );
    match store.read_legacy_migration_snapshot(&access).await? {
        LegacyMigrationSnapshotRead::Available(snapshot) => {
            assert_eq!(snapshot.resources, records);
            assert_eq!(snapshot.writer, Some(writer.clone()));
        }
        other => return Err(format!("unexpected snapshot: {other:?}").into()),
    }
    let mut invalid = desired.clone();
    invalid.serial = 1_i64 << 53;
    assert!(
        store
            .check_legacy_migration_copy(&LegacyMigrationCopy {
                access,
                resources: &[desired.clone(), invalid],
                writer: Some(&writer)
            })
            .await
            .is_err()
    );
    assert_eq!(
        store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access: stale,
                resource: &desired
            })
            .await?,
        MigrationMutation::Stale
    );
    assert_eq!(
        store
            .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                access: stale,
                address: &desired.address
            })
            .await?,
        MigrationMutation::Stale
    );
    assert_eq!(
        store
            .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                access: stale,
                writer: None
            })
            .await?,
        MigrationMutation::Stale
    );
    assert_eq!(store.list(&tenant).await?, vec![desired.clone()]);
    assert_eq!(
        store
            .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                access,
                address: &desired.address
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .delete_legacy_migration_record(&LegacyMigrationDeleteRequest {
                access,
                address: &desired.address
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .set_legacy_migration_writer(&LegacyMigrationWriterRequest {
                access,
                writer: None
            })
            .await?,
        MigrationMutation::Applied
    );
    Ok(())
}

#[tokio::test]
async fn sql_json_preserves_reserved_objects_and_rejects_rounded_numbers() -> TestResult {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = super::store_at(&server);
    store.migrate().await?;
    let tenant = TenantKey::try_new("example.com/precision", "web")?;
    let fence = take(&store, &tenant).await?;
    let access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &fence,
    };
    let mut original = super::record("pet");
    original.state = serde_json::json!({
        "raw": {"$serde_json::private::RawValue": "42"},
        "number": {"$serde_json::private::Number": "42"},
        "exact": 0.5
    });
    original.identity = Some(serde_json::json!({
        "$serde_json::private::RawValue": "42"
    }));
    store.put(&tenant, &fence, &original).await?;
    let stored = store.list(&tenant).await?.remove(0);
    assert_eq!(stored.state, original.state);
    assert_eq!(stored.identity, original.identity);
    match store.read_legacy_migration_snapshot(&access).await? {
        LegacyMigrationSnapshotRead::Available(snapshot) => {
            assert_eq!(snapshot.resources, [stored]);
        }
        other => return Err(format!("unexpected snapshot: {other:?}").into()),
    }

    for (column, invalid) in [
        ("resource_state_json", r#"{"large":18446744073709551617}"#),
        (
            "resource_state_json",
            r#"{"precise":0.123456789012345678901234567890123456789}"#,
        ),
        (
            "resource_state_json",
            r#"{"duplicate":18446744073709551617,"duplicate":1}"#,
        ),
        ("identity_json", r#"{"large":18446744073709551617}"#),
    ] {
        let sql = format!("UPDATE cuenv_infrastructure_resources SET {column} = ?");
        database.database.lock().unwrap().execute(&sql, [invalid])?;
        let before = database.query(&format!(
            "SELECT {column} FROM cuenv_infrastructure_resources"
        ));
        assert!(store.list(&tenant).await.is_err());
        assert!(store.read_legacy_migration_snapshot(&access).await.is_err());
        assert_eq!(
            database.query(&format!(
                "SELECT {column} FROM cuenv_infrastructure_resources"
            )),
            before
        );
        let restored = if column == "resource_state_json" {
            serde_json::to_string(&original.state)?
        } else {
            serde_json::to_string(&original.identity)?
        };
        database
            .database
            .lock()
            .unwrap()
            .execute(&sql, [restored])?;
    }
    assert_eq!(store.list(&tenant).await?.remove(0).state, original.state);
    Ok(())
}

#[tokio::test]
async fn sql_snapshot_refuses_malformed_identity_and_private_columns() -> TestResult {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = super::store_at(&server);
    store.migrate().await?;
    let tenant = TenantKey::try_new("example.com/precision", "malformed")?;
    let fence = take(&store, &tenant).await?;
    let access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &fence,
    };
    let original = super::record("pet");
    store.put(&tenant, &fence, &original).await?;
    for (column, expression) in [
        ("identity_json", "X'00FF'"),
        ("private", "'foreign-private-data'"),
        ("tainted", "2"),
    ] {
        database.database.lock().unwrap().execute(
            &format!("UPDATE cuenv_infrastructure_resources SET {column} = {expression}"),
            [],
        )?;
        let before = database.query(&format!(
            "SELECT {column} FROM cuenv_infrastructure_resources"
        ));
        assert!(store.list(&tenant).await.is_err());
        assert!(store.read_legacy_migration_snapshot(&access).await.is_err());
        assert_eq!(
            database.query(&format!(
                "SELECT {column} FROM cuenv_infrastructure_resources"
            )),
            before
        );
        if column == "identity_json" {
            database.database.lock().unwrap().execute(
                "UPDATE cuenv_infrastructure_resources SET identity_json = ?",
                [serde_json::to_string(&original.identity)?],
            )?;
        } else if column == "private" {
            database.database.lock().unwrap().execute(
                "UPDATE cuenv_infrastructure_resources SET private = ?",
                [&original.private],
            )?;
        } else {
            database.database.lock().unwrap().execute(
                "UPDATE cuenv_infrastructure_resources SET tainted = ?",
                [i64::from(original.tainted)],
            )?;
        }
    }
    let restored = store.list(&tenant).await?.remove(0);
    assert_eq!(restored.identity, original.identity);
    assert_eq!(restored.private, original.private);
    assert_eq!(restored.tainted, original.tainted);
    Ok(())
}

#[tokio::test]
async fn serial_boundary_and_lost_response_do_not_double_increment() -> TestResult {
    let database = EmulatedD1::new();
    let drop_next = Arc::new(AtomicBool::new(false));
    let shared_database = Arc::clone(&database);
    let shared_drop = Arc::clone(&drop_next);
    let server = crate::state::fake_server::fake_server(Arc::new(move |_, body| {
        let response = shared_database.answer(&body);
        let sql = body["sql"].as_str().unwrap_or_default();
        if sql.starts_with("UPDATE cuenv_infrastructure_resources SET")
            && shared_drop.swap(false, Ordering::SeqCst)
        {
            None
        } else {
            Some(response)
        }
    }))
    .await;
    let store = super::store_at(&server);
    store.migrate().await?;
    let tenant = TenantKey::try_new("example.com/copy", "serial")?;
    let lock = store.lock(&tenant, "ordinary").await?;
    let original = super::record("pet");
    store.put(&tenant, &lock, &original).await?;
    database.database.lock().unwrap().execute(
        "UPDATE cuenv_infrastructure_resources SET serial = ?",
        [(1_i64 << 53) - 2],
    )?;
    drop_next.store(true, Ordering::SeqCst);
    let updated = super::record("pet");
    store.put(&tenant, &lock, &updated).await?;
    let stored = store.list(&tenant).await?.remove(0);
    assert_eq!(stored.serial, (1_i64 << 53) - 1);
    assert!(
        store
            .put(&tenant, &lock, &updated)
            .await
            .unwrap_err()
            .to_string()
            .contains("exhausted")
    );
    let put = ConditionalPut {
        resource: &stored,
        expected: RecordVersion::of(Some(&stored)),
    };
    assert!(
        store
            .put_if_unchanged(&tenant, &lock, &put)
            .await
            .unwrap_err()
            .to_string()
            .contains("exhausted")
    );
    assert_eq!(store.list(&tenant).await?, vec![stored]);
    let mut ambiguous = updated;
    ambiguous.schema_version = 1_i64 << 53;
    assert!(store.put(&tenant, &lock, &ambiguous).await.is_err());
    Ok(())
}

#[tokio::test]
async fn lost_exact_copy_response_uses_fenced_readback() -> TestResult {
    let database = EmulatedD1::new();
    let drop_next = Arc::new(AtomicBool::new(false));
    let shared_database = Arc::clone(&database);
    let shared_drop = Arc::clone(&drop_next);
    let server = crate::state::fake_server::fake_server(Arc::new(move |_, body| {
        let response = shared_database.answer(&body);
        let sql = body["sql"].as_str().unwrap_or_default();
        if sql.starts_with("INSERT INTO cuenv_infrastructure_resources")
            && shared_drop.swap(false, Ordering::SeqCst)
        {
            None
        } else {
            Some(response)
        }
    }))
    .await;
    let store = super::store_at(&server);
    store.migrate().await?;
    let tenant = TenantKey::try_new("example.com/copy", "lost")?;
    let fence = take(&store, &tenant).await?;
    let access = LegacyMigrationAccess {
        tenant: &tenant,
        fence: &fence,
    };
    let mut resource = super::record("pet");
    resource.serial = 8;
    resource.generation = Uuid::new_v4();
    drop_next.store(true, Ordering::SeqCst);
    assert_eq!(
        store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access,
                resource: &resource
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(store.list(&tenant).await?, vec![resource]);
    Ok(())
}

#[tokio::test]
async fn malformed_environment_rows_are_never_skipped_or_rewritten() -> TestResult {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = super::store_at(&server);
    store.migrate().await?;
    database.database.lock().unwrap().execute_batch("INSERT INTO cuenv_infrastructure_owners VALUES ('example.com/copy', 'web', 'bad' || char(10), 'owner', 'now')")?;
    let error = store
        .environments("example.com/copy", "web")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid environment"));
    assert!(!error.to_string().contains("bad"));
    database.database.lock().unwrap().execute_batch("DELETE FROM cuenv_infrastructure_owners; INSERT INTO cuenv_infrastructure_owners VALUES ('example.com/copy', 'web', X'FF', 'owner', 'now')")?;
    assert!(store.environments("example.com/copy", "web").await.is_err());
    Ok(())
}
