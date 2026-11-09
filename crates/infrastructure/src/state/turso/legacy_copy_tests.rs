//! Legacy copy and serial-boundary checks against a local libSQL server.

use super::{
    Database, SqlValue, StateLock, StateStore, Statement, TursoConfiguration, TursoStateStore,
};
use crate::state::{
    LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationRecordRequest,
    LegacyMigrationSnapshotRead, LockRequest, MigrationMutation,
};
use crate::tenant::TenantKey;
use uuid::Uuid;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) async fn exact_copy_against_sqld() -> TestResult {
    let database = Database::isolated()?;
    let store = TursoStateStore::try_new(TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: None,
    })?;
    store.migrate().await?;
    let legacy = TenantKey::try_new("example.com/maintenance", "legacy")?;
    let sibling = TenantKey::try_new("example.com/maintenance", "sibling")?;
    let fence = StateLock {
        lock_identifier: format!("migrated-{}", Uuid::new_v4()),
    };
    store
        .acquire_lock(
            &legacy,
            &LockRequest {
                lock: &fence,
                holder: "legacy migration",
                instance: None,
            },
        )
        .await?;
    let access = LegacyMigrationAccess {
        tenant: &legacy,
        fence: &fence,
    };
    let mut resource = super::probe_record();
    resource.generation = Uuid::new_v4();
    resource.serial = 12;
    assert_eq!(
        store
            .check_legacy_migration_copy(&LegacyMigrationCopy {
                access,
                resources: std::slice::from_ref(&resource),
                writer: None
            })
            .await?,
        MigrationMutation::Applied
    );
    assert_eq!(
        store
            .replace_legacy_migration_record(&LegacyMigrationRecordRequest {
                access,
                resource: &resource
            })
            .await?,
        MigrationMutation::Applied
    );
    assert!(
        matches!(store.read_legacy_migration_snapshot(&access).await?, LegacyMigrationSnapshotRead::Available(snapshot) if snapshot.resources == [resource.clone()])
    );
    let sibling_lock = store.lock(&sibling, "ordinary").await?;
    store.put(&sibling, &sibling_lock, &resource).await?;
    store
        .inner
        .execute(Statement::new(
            "UPDATE cuenv_infrastructure_resources SET serial = ? WHERE project = ?",
            vec![
                SqlValue::integer((1_i64 << 53) - 2),
                SqlValue::text("sibling"),
            ],
        ))
        .await?;
    store.put(&sibling, &sibling_lock, &resource).await?;
    assert!(store.put(&sibling, &sibling_lock, &resource).await.is_err());
    assert!(store.unlock(&sibling, &sibling_lock).await?);
    Ok(())
}
