use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::state::fake_server::{FakeServer, fake_server, fake_server_with_headers};
use crate::state::sql::{LATEST_SCHEMA_VERSION, MIGRATIONS, Migration, MigrationWait, RetryPolicy};
use crate::state::{
    ConditionalPut, ManagedResource, OwnerClaim, OwnerClaimMode, RecordVersion, ResourceAddress,
    StateLock, StateStore,
};
use crate::tenant::{ProjectInstance, TenantKey};

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
const DATABASE: &str = "0b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9";
const TOKEN: &str = "d1-secret-token";

fn configuration() -> D1Configuration {
    D1Configuration {
        account_id: ACCOUNT.into(),
        database_id: DATABASE.into(),
        api_token: TOKEN.into(),
    }
}

/// A store talking to `server`, retrying quickly and never waiting for a
/// blocked migration.
fn store_at(server: &FakeServer) -> D1StateStore {
    let mut store = D1StateStore::at(configuration(), &server.url, Duration::from_secs(5)).unwrap();
    store.inner.retry_policy = RetryPolicy {
        retries: 3,
        initial_delay: Duration::from_millis(1),
    };
    store.inner.migration_wait = MigrationWait {
        bound: Duration::ZERO,
        poll_interval: Duration::from_millis(1),
        announcement_lifetime: Duration::from_secs(1),
    };
    store
}

fn tenant() -> TenantKey {
    TenantKey::try_new("example.com/d1", "web").unwrap()
}

fn record(name: &str) -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new("random_pet", name),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 3,
        state: json!({"id": name, "length": 2}),
        private: vec![0, 255, 16],
        dependencies: vec!["other".into()],
        sensitive_attributes: vec!["id".into()],
        tainted: true,
        identity: Some(json!({"id": name})),
        serial: 0,
        generation: uuid::Uuid::nil(),
    }
}

// -----------------------------------------------------------------------
// A D1 stand-in: the Cloudflare `/raw` endpoint over a real SQLite.
// -----------------------------------------------------------------------

/// Answers `/raw` requests the way D1 does: parameters bound as text and
/// only in a request of one statement, the statements of a request run as
/// one transaction, rows as arrays with blobs as lists of bytes, `changes`
/// from the database's change counter, and a rejected statement as HTTP 400
/// with error 7500.
struct EmulatedD1 {
    database: Mutex<rusqlite::Connection>,
}

impl EmulatedD1 {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            database: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
        })
    }

    async fn serve(self: &Arc<Self>) -> FakeServer {
        let database = Arc::clone(self);
        fake_server(Arc::new(move |_, body| Some(database.answer(&body)))).await
    }

    /// Run SQL against the database directly, outside the API.
    fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        let database = self.database.lock().unwrap();
        let mut statement = database.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|index| cell(row.get_ref(index).unwrap()))
                    .collect())
            })
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
    }

    fn answer(&self, body: &Value) -> (u16, String) {
        let sql = body["sql"].as_str().unwrap();
        let params: Vec<String> = body
            .get("params")
            .and_then(Value::as_array)
            .map(|params| {
                params
                    .iter()
                    .map(|param| param.as_str().expect("D1 binds text only").to_string())
                    .collect()
            })
            .unwrap_or_default();
        let statements: Vec<&str> = sql.split(";\n").collect();
        if statements.len() > 1 && !params.is_empty() {
            return error(
                400,
                7400,
                "The request is malformed: params with multiple statements is not supported",
            );
        }
        let mut database = self.database.lock().unwrap();
        let transaction = database.transaction().unwrap();
        let mut results = Vec::new();
        for statement in statements {
            match run(&transaction, statement, &params) {
                Ok(result) => results.push(result),
                Err(message) => {
                    drop(transaction);
                    return error(400, 7500, &format!("{message}: SQLITE_ERROR"));
                }
            }
        }
        transaction.commit().unwrap();
        (
            200,
            json!({"success": true, "errors": [], "messages": [], "result": results}).to_string(),
        )
    }
}

fn error(status: u16, code: i64, message: &str) -> (u16, String) {
    (
        status,
        json!({
            "success": false,
            "errors": [{"code": code, "message": message}],
            "messages": [],
            "result": [],
        })
        .to_string(),
    )
}

/// Run one statement, binding every parameter as text.
fn run(
    database: &rusqlite::Connection,
    sql: &str,
    params: &[String],
) -> std::result::Result<Value, String> {
    let before = database.total_changes();
    let mut statement = database.prepare(sql).map_err(|error| error.to_string())?;
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut rows = statement
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(|error| error.to_string())?;
    let mut cells = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        cells.push(Value::Array(
            (0..columns.len())
                .map(|index| cell(row.get_ref(index).unwrap()))
                .collect(),
        ));
    }
    drop(rows);
    Ok(json!({
        "results": {"columns": columns, "rows": cells},
        "success": true,
        "meta": {"changes": database.total_changes() - before},
    }))
}

/// A cell as D1 sends it: numbers as JSON numbers (so large integers lose
/// precision), blobs as lists of bytes.
fn cell(value: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => Value::Null,
        // D1's numbers are JavaScript numbers.
        #[allow(clippy::cast_precision_loss)]
        ValueRef::Integer(integer) if integer.unsigned_abs() > 1 << 53 => json!(integer as f64),
        ValueRef::Integer(integer) => json!(integer),
        ValueRef::Real(real) => json!(real),
        ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
        ValueRef::Blob(bytes) => json!(bytes),
    }
}

// -----------------------------------------------------------------------
// The state store against the stand-in.
// -----------------------------------------------------------------------

#[tokio::test]
async fn reads_before_any_migration_are_empty_and_create_nothing() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    assert!(store.list(&tenant()).await.unwrap().is_empty());
    assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
    assert_eq!(store.owner(&tenant()).await.unwrap(), None);
    assert!(store.locks().await.unwrap().is_empty());
    assert!(!store.force_unlock(&tenant(), "any").await.unwrap());
    assert!(store.lock(&tenant(), "test").await.is_err());
    assert!(
        database
            .query("SELECT name FROM sqlite_master WHERE type = 'table'")
            .is_empty(),
        "reads and a refused lock create nothing"
    );
}

#[tokio::test]
async fn version_two_preserves_existing_rows_and_owners_while_fencing_old_readers() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.inner.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    // Seed the actual version-one layout, including a lock held by an old client.
    database
        .database
        .lock()
        .unwrap()
        .execute_batch(
            "INSERT INTO cuenv_infrastructure_resources \
         (module_path, project, environment, resource_type, resource_name, provider, \
          provider_source, schema_version, state_json, private, dependencies_json, tainted, \
          identity_json, serial, generation, created_at, updated_at) VALUES \
         ('example.com/d1', 'web', '', 'random_pet', 'pet', 'random', \
          'registry.terraform.io/hashicorp/random', 3, '{\"id\":\"old-secret\"}', x'00ff', \
          '[\"random_pet.other\"]', 1, '{\"id\":\"identity\"}', 7, \
          '00000000-0000-0000-0000-000000000007', 'created', 'updated');
         INSERT INTO cuenv_infrastructure_locks VALUES \
         ('example.com/d1', 'web', '', 'old-lock', 'old-client', 'acquired');
         INSERT INTO cuenv_infrastructure_owners VALUES \
         ('example.com/d1', 'web', '', '.:web', 'claimed');",
        )
        .unwrap();
    let rows = database.query("SELECT * FROM cuenv_infrastructure_resources");
    let locks = store.locks().await.unwrap();
    let owner = store.owner(&tenant()).await.unwrap();
    assert!(
        store
            .list(&tenant())
            .await
            .unwrap_err()
            .to_string()
            .contains("migrate the state store")
    );
    assert!(matches!(
        store.migrate().await,
        Err(InfrastructureError::StateMigrationBlocked { version: 2, .. })
    ));
    assert_eq!(store.inner.schema_version().await.unwrap(), 1);
    assert_eq!(
        database.query("SELECT * FROM cuenv_infrastructure_resources"),
        rows
    );
    assert_eq!(store.locks().await.unwrap(), locks);
    assert_eq!(store.owner(&tenant()).await.unwrap(), owner);

    assert!(store.force_unlock(&tenant(), "old-lock").await.unwrap());
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
    assert_eq!(store.inner.schema_version().await.unwrap(), 2);
    assert_eq!(store.owner(&tenant()).await.unwrap(), owner);
    let mut migrated = database.query("SELECT * FROM cuenv_infrastructure_resources");
    assert_eq!(migrated[0].pop(), Some(json!("[]")));
    assert_eq!(migrated, rows);
    let legacy = store.list(&tenant()).await.unwrap().remove(0);
    assert!(legacy.sensitive_attributes.is_empty());
    assert_eq!(legacy.serial, 7);
    let lock = store.lock(&tenant(), "new-client").await.unwrap();
    let sensitive = ManagedResource {
        sensitive_attributes: vec!["id".into()],
        ..legacy.clone()
    };
    let update = ConditionalPut {
        resource: &sensitive,
        expected: RecordVersion::of(Some(&legacy)),
    };
    store
        .put_if_unchanged(&tenant(), &lock, &update)
        .await
        .unwrap();
    // A fresh client reads the names, independent of any process-local registry.
    let fresh = store_at(&server);
    let stored = fresh.list(&tenant()).await.unwrap().remove(0);
    assert!(stored.same_content(&sensitive));
    assert_eq!(stored.serial, 8);
    assert_eq!(stored.generation, legacy.generation);
    fresh
        .put_if_unchanged(&tenant(), &lock, &update)
        .await
        .unwrap();
    assert!(matches!(
        fresh
            .put_if_unchanged(
                &tenant(),
                &lock,
                &ConditionalPut {
                    resource: &legacy,
                    expected: update.expected,
                }
            )
            .await,
        Err(InfrastructureError::StateChanged { .. })
    ));
}

#[tokio::test]
async fn a_version_one_reader_paused_before_select_cannot_read_version_two_state() {
    // The exact projection used by the version-one client, without metadata.
    const VERSION_ONE_SELECT: &str = "SELECT resource_type, resource_name, provider, provider_source, \
         schema_version, state_json, private, dependencies_json, tainted, identity_json, serial, \
         generation FROM cuenv_infrastructure_resources \
         WHERE module_path = ? AND project = ? AND environment = ? \
         ORDER BY resource_type, resource_name";
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let old_client = store_at(&server);
    old_client.inner.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    let old_select = || {
        Statement::new(
            VERSION_ONE_SELECT,
            crate::state::sql::tenant_arguments(&tenant()),
        )
    };
    assert!(
        old_client
            .inner
            .execute(old_select())
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    // The old reader has passed its version check and pauses before SELECT.
    assert_eq!(old_client.inner.stored_version().await.unwrap(), 1);
    let writer = store_at(&server);
    writer.migrate().await.unwrap();
    let lock = writer.lock(&tenant(), "version-two writer").await.unwrap();
    let sensitive = ManagedResource {
        state: json!({"id": "sensitive-state-value"}),
        sensitive_attributes: vec!["id".into()],
        ..record("pet")
    };
    writer.put(&tenant(), &lock, &sensitive).await.unwrap();
    writer.unlock(&tenant(), &lock).await.unwrap();

    // Resuming the old SELECT must fail even though its earlier check passed.
    let error = old_client.inner.execute(old_select()).await.unwrap_err();
    assert!(
        error.to_string().contains("no such column: state_json"),
        "{error}"
    );
    assert!(!error.to_string().contains("sensitive-state-value"));
    let current = store_at(&server).list(&tenant()).await.unwrap().remove(0);
    assert!(current.same_content(&sensitive));
    assert_eq!(current.sensitive_attributes, ["id"]);
}

#[tokio::test]
async fn a_failed_state_column_rename_rolls_back_the_sensitivity_column() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.inner.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    // A foreign column makes the second statement fail, after ADD succeeded
    // inside the migration transaction. Neither statement may commit alone.
    database
        .database
        .lock()
        .unwrap()
        .execute_batch(
            "ALTER TABLE cuenv_infrastructure_resources ADD COLUMN resource_state_json TEXT",
        )
        .unwrap();
    assert!(matches!(
        store.migrate().await,
        Err(InfrastructureError::StateSchemaConflict { .. })
    ));
    assert_eq!(store.inner.schema_version().await.unwrap(), 1);
    assert!(database.query(
        "SELECT name FROM pragma_table_info('cuenv_infrastructure_resources') WHERE name = 'sensitive_attributes'"
    ).is_empty());
    assert_eq!(database.query(
        "SELECT name FROM pragma_table_info('cuenv_infrastructure_resources') WHERE name = 'state_json'"
    ), vec![vec![json!("state_json")]]);
}

#[tokio::test]
async fn stale_first_migrator_does_not_wait_on_current_schema_locks() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    crate::state::sql::tests::stale_first_migrator(&store_at(&server).inner).await;
}

#[tokio::test]
async fn completed_migrations_leave_no_pending_announcements() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    crate::state::sql::tests::completed_announcements(&store_at(&server).inner).await;
}

#[tokio::test]
async fn concurrent_sensitivity_migrations_converge_and_unrecorded_columns_are_refused() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.inner.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    let other = store_at(&server);
    let (first, second) = tokio::join!(store.migrate(), other.migrate());
    first.unwrap();
    second.unwrap();
    assert_eq!(store.inner.schema_version().await.unwrap(), 2);

    // A column alone does not prove that a migration was committed.
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.inner.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    database
        .database
        .lock()
        .unwrap()
        .execute_batch(MIGRATIONS[1].statements[0])
        .unwrap();
    assert!(matches!(
        store.migrate().await,
        Err(InfrastructureError::StateSchemaConflict { .. })
    ));
    assert_eq!(store.inner.schema_version().await.unwrap(), 1);
}

#[tokio::test]
async fn malformed_sensitivity_is_refused_without_echoing_any_state() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.migrate().await.unwrap();
    let lock = store.lock(&tenant(), "test").await.unwrap();
    let record = ManagedResource {
        state: json!({"id": "secret-state"}),
        ..record("pet")
    };
    store.put(&tenant(), &lock, &record).await.unwrap();
    for metadata in [
        "not-json-secret",
        "null",
        "{}",
        "\"secret-metadata\"",
        "[\"id\", {\"secret-metadata\": 1}]",
    ] {
        database
            .database
            .lock()
            .unwrap()
            .execute(
                "UPDATE cuenv_infrastructure_resources SET sensitive_attributes = ?",
                [metadata],
            )
            .unwrap();
        let error = store.list(&tenant()).await.unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::UndecodableRecord { problem, .. } if problem.contains("sensitive_attributes"))
        );
        assert!(!error.to_string().contains("secret-metadata"));
        assert!(!error.to_string().contains("not-json-secret"));
        assert!(!error.to_string().contains("secret-state"));
        // The address remains usable for explicit removal of corrupt state.
        assert_eq!(store.addresses(&tenant()).await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn records_round_trip_through_d1() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.migrate().await.unwrap();
    // Migrating again changes nothing.
    store.migrate().await.unwrap();
    assert_eq!(
        store.inner.schema_version().await.unwrap(),
        LATEST_SCHEMA_VERSION
    );

    let lock = store.lock(&tenant(), "test").await.unwrap();
    let written = record("pet");
    store.put(&tenant(), &lock, &written).await.unwrap();
    let stored = store.list(&tenant()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].same_content(&written), "{:?}", stored[0]);
    assert_eq!(stored[0].serial, 1);
    assert!(!stored[0].generation.is_nil());

    // Integers bound as text are stored as integers.
    assert_eq!(
        database.query(
            "SELECT typeof(schema_version), typeof(tainted), typeof(serial), typeof(private) \
             FROM cuenv_infrastructure_resources"
        ),
        vec![vec![
            json!("integer"),
            json!("integer"),
            json!("integer"),
            json!("blob")
        ]]
    );

    // Updates keep the generation and count the writes.
    let mut changed = written.clone();
    changed.sensitive_attributes = vec!["length".into()];
    store.put(&tenant(), &lock, &changed).await.unwrap();
    let updated = store.list(&tenant()).await.unwrap().remove(0);
    assert_eq!(updated.serial, 2);
    assert_eq!(updated.generation, stored[0].generation);
    assert_eq!(updated.sensitive_attributes, ["length"]);

    // No private bytes and no identity are stored as null.
    let bare = ManagedResource {
        private: Vec::new(),
        identity: None,
        tainted: false,
        ..record("bare")
    };
    store.put(&tenant(), &lock, &bare).await.unwrap();
    assert_eq!(
        database.query(
            "SELECT private IS NULL, identity_json IS NULL FROM cuenv_infrastructure_resources \
             WHERE resource_name = 'bare'"
        ),
        vec![vec![json!(1), json!(1)]]
    );
    let listed = store.list(&tenant()).await.unwrap();
    assert!(listed[0].same_content(&bare), "{:?}", listed[0]);
    assert_eq!(
        store.addresses(&tenant()).await.unwrap(),
        vec![
            ResourceAddress::new("random_pet", "bare"),
            ResourceAddress::new("random_pet", "pet")
        ]
    );

    // Another tenant sees none of it.
    let other = TenantKey::with_environment("example.com/d1", "web", "prod").unwrap();
    assert!(store.list(&other).await.unwrap().is_empty());

    store.delete(&tenant(), &lock, &bare.address).await.unwrap();
    // Deleting what is gone is not an error while the lock is held.
    store.delete(&tenant(), &lock, &bare.address).await.unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);
    assert!(store.unlock(&tenant(), &lock).await.unwrap());
    assert!(!store.unlock(&tenant(), &lock).await.unwrap());
}

#[tokio::test]
async fn writes_are_fenced_by_the_lock_and_by_the_stored_version() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.migrate().await.unwrap();
    let lock = store.lock(&tenant(), "first").await.unwrap();

    // Only one run holds the lock.
    let refused = store.lock(&tenant(), "second").await.unwrap_err();
    assert!(
        matches!(&refused, InfrastructureError::Locked { holder, .. } if holder == "first"),
        "{refused:?}"
    );
    let information = store.current_lock(&tenant()).await.unwrap().unwrap();
    assert_eq!(information.lock_identifier, lock.lock_identifier);

    // A write under a lock that is not the tenant's is refused.
    let stranger = StateLock::generate();
    for error in [
        store
            .put(&tenant(), &stranger, &record("pet"))
            .await
            .unwrap_err(),
        store
            .delete(&tenant(), &stranger, &record("pet").address)
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, InfrastructureError::LockLost { .. }),
            "{error:?}"
        );
    }

    // Compare and swap: a create, an update at the right version, and an
    // update at a stale version.
    let mut created = record("pet");
    created.generation = uuid::Uuid::new_v4();
    let create = ConditionalPut {
        resource: &created,
        expected: RecordVersion::Absent,
    };
    store
        .put_if_unchanged(&tenant(), &lock, &create)
        .await
        .unwrap();
    let stored = store.list(&tenant()).await.unwrap().remove(0);
    assert_eq!(stored.generation, created.generation);
    // Retrying the create after a lost response finds its own write.
    store
        .put_if_unchanged(&tenant(), &lock, &create)
        .await
        .unwrap();
    let version = RecordVersion::of(Some(&stored));
    let changed = ManagedResource {
        state: json!({"id": "pet", "length": 3}),
        ..stored.clone()
    };
    store
        .put_if_unchanged(
            &tenant(),
            &lock,
            &ConditionalPut {
                resource: &changed,
                expected: version,
            },
        )
        .await
        .unwrap();
    let stale = ManagedResource {
        state: json!({"id": "pet", "length": 4}),
        ..stored.clone()
    };
    let error = store
        .put_if_unchanged(
            &tenant(),
            &lock,
            &ConditionalPut {
                resource: &stale,
                expected: version,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, InfrastructureError::StateChanged { .. }),
        "{error:?}"
    );
    let current = store.list(&tenant()).await.unwrap().remove(0);
    assert_eq!(current.state, json!({"id": "pet", "length": 3}));
    assert_eq!(current.serial, 2);

    // Ownership is claimed once and transferred explicitly.
    let instance = ProjectInstance::try_new(".", "web").unwrap();
    let copy = ProjectInstance::try_new("_copy", "web").unwrap();
    let claim = |instance, mode| OwnerClaim { instance, mode };
    let owner = store
        .claim_owner(
            &tenant(),
            &lock,
            &claim(&instance, OwnerClaimMode::IfUnowned),
        )
        .await
        .unwrap();
    assert_eq!(owner.instance, instance);
    let kept = store
        .claim_owner(&tenant(), &lock, &claim(&copy, OwnerClaimMode::IfUnowned))
        .await
        .unwrap();
    assert_eq!(kept.instance, instance);
    let transferred = store
        .claim_owner(&tenant(), &lock, &claim(&copy, OwnerClaimMode::Transfer))
        .await
        .unwrap();
    assert_eq!(transferred.instance, copy);
    assert_eq!(
        store.owner(&tenant()).await.unwrap().unwrap().instance,
        copy
    );

    // A lock left behind is listed and released by its identifier.
    let locks = store.locks().await.unwrap();
    assert_eq!(locks.len(), 1);
    assert!(
        store
            .force_unlock(&locks[0].tenant().unwrap(), &locks[0].lock.lock_identifier)
            .await
            .unwrap()
    );
    let error = store
        .put(&tenant(), &lock, &record("pet"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, InfrastructureError::LockLost { .. }),
        "{error:?}"
    );
}

/// The shipped migrations followed by one that only creates a table.
fn migrations_with_a_probe() -> [Migration; 3] {
    [
        Migration {
            version: MIGRATIONS[0].version,
            statements: MIGRATIONS[0].statements,
        },
        Migration {
            version: MIGRATIONS[1].version,
            statements: MIGRATIONS[1].statements,
        },
        Migration {
            version: LATEST_SCHEMA_VERSION + 1,
            statements: &["CREATE TABLE cuenv_infrastructure_probe (value INTEGER NOT NULL)"],
        },
    ]
}

#[tokio::test]
async fn a_migration_refuses_while_any_lock_is_held() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.migrate().await.unwrap();
    let lock = store.lock(&tenant(), "run").await.unwrap();

    let refused = store
        .inner
        .migrate_to(&migrations_with_a_probe())
        .await
        .unwrap_err();
    let InfrastructureError::StateMigrationBlocked { version: 3, locks } = &refused else {
        panic!("{refused:?}");
    };
    assert_eq!(locks[0].lock.lock_identifier, lock.lock_identifier);
    // The transaction left nothing behind: no table, no version.
    assert!(
        database
            .query("SELECT name FROM sqlite_master WHERE name = 'cuenv_infrastructure_probe'")
            .is_empty()
    );
    assert_eq!(
        store.inner.schema_version().await.unwrap(),
        LATEST_SCHEMA_VERSION
    );

    store.unlock(&tenant(), &lock).await.unwrap();
    store
        .inner
        .migrate_to(&migrations_with_a_probe())
        .await
        .unwrap();
    assert_eq!(
        store.inner.schema_version().await.unwrap(),
        LATEST_SCHEMA_VERSION + 1
    );
    // A build that knows only the shipped versions refuses the newer schema.
    for error in [
        store.list(&tenant()).await.unwrap_err(),
        store.lock(&tenant(), "older").await.unwrap_err(),
        store.migrate().await.unwrap_err(),
    ] {
        assert!(
            matches!(
                error,
                InfrastructureError::StateSchemaNewer {
                    found: 3,
                    supported: 2
                }
            ),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn a_waiting_migration_holds_off_new_locks() {
    let database = EmulatedD1::new();
    let server = database.serve().await;
    let store = store_at(&server);
    store.migrate().await.unwrap();
    // A migrator announced itself and is waiting for the locks to drain.
    database.query(
        "INSERT INTO cuenv_infrastructure_pending_migration (version, expires_at) \
         VALUES (3, 99999999999) RETURNING version",
    );
    let error = store.lock(&tenant(), "run").await.unwrap_err();
    assert!(
        matches!(
            error,
            InfrastructureError::StateMigrationPending { version: 3 }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn recovery_identity_names_the_account_and_database() {
    let server = EmulatedD1::new().serve().await;
    let store = store_at(&server);
    let identity = store.recovery_identity().unwrap();
    assert_eq!(identity.len(), 64);
    assert!(!identity.contains(ACCOUNT) && !identity.contains(DATABASE));
    assert_eq!(store_at(&server).recovery_identity().unwrap(), identity);
    let other = D1StateStore::try_new(D1Configuration {
        database_id: "1b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9".into(),
        ..configuration()
    })
    .unwrap();
    assert_ne!(other.recovery_identity().unwrap(), identity);
}

// -----------------------------------------------------------------------
// Wire format, values and errors.
// -----------------------------------------------------------------------

#[test]
fn identifiers_follow_cloudflare_forms() {
    D1StateStore::validate_identifiers(ACCOUNT, DATABASE).unwrap();
    for (account, database) in [
        ("0123456789ABCDEF0123456789ABCDEF", DATABASE),
        ("0123456789abcdef", DATABASE),
        ("0123456789abcdef0123456789abcdeg", DATABASE),
        (ACCOUNT, "0B2C3D4E-5F60-4718-8293-A4B5C6D7E8F9"),
        (ACCOUNT, "0b2c3d4e5f6047188293a4b5c6d7e8f9"),
        (ACCOUNT, "0b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9/raw"),
        (ACCOUNT, "../../zones"),
    ] {
        assert!(
            matches!(
                D1StateStore::validate_identifiers(account, database),
                Err(InfrastructureError::Configuration(_))
            ),
            "{account} {database}"
        );
    }
    let error = D1StateStore::try_new(D1Configuration {
        api_token: String::new(),
        ..configuration()
    })
    .unwrap_err();
    assert!(error.to_string().contains("API token"), "{error}");
}

#[test]
fn debug_output_redacts_the_token() {
    let rendered = format!("{:?}", configuration());
    assert!(!rendered.contains(TOKEN), "{rendered}");
    let store = D1StateStore::try_new(configuration()).unwrap();
    let rendered = format!("{store:?}");
    assert!(!rendered.contains(TOKEN), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

#[test]
fn requests_bind_text_and_only_in_one_statement() {
    let body = QueryBody::of(&[Statement::new(
        "SELECT ?, ?, ?, ?, ?, ?",
        vec![
            SqlValue::text("a"),
            SqlValue::integer(-7),
            SqlValue::blob(&[0, 255]),
            SqlValue::blob(&[]),
            SqlValue::optional_text(Some("{}")),
            SqlValue::optional_text(None),
        ],
    )])
    .unwrap();
    assert_eq!(
        serde_json::to_value(&body).unwrap()["params"],
        json!(["a", "-7", "00ff", "", "{}", ""])
    );
    let script = QueryBody::of(&[
        Statement::new("CREATE TABLE a (x)", Vec::new()),
        Statement::new("SELECT 1", Vec::new()),
    ])
    .unwrap();
    assert_eq!(
        serde_json::to_value(&script).unwrap(),
        json!({"sql": "CREATE TABLE a (x);\nSELECT 1"})
    );
    let refused = QueryBody::of(&[
        Statement::new("SELECT ?", vec![SqlValue::integer(1)]),
        Statement::new("SELECT 1", Vec::new()),
    ])
    .unwrap_err();
    assert_eq!(refused.kind, FailureKind::Permanent);
    assert!(QueryBody::of(&[]).is_err());
    assert!(
        QueryBody::of(&[Statement::new("SELECT ?", vec![SqlValue::Null])]).is_err(),
        "null is never bound as itself"
    );
}

#[test]
fn cells_decode_exact_integers_blobs_and_nothing_else_as_such() {
    assert_eq!(value(&json!(42)), SqlValue::Integer(42));
    assert_eq!(value(&json!(-42)), SqlValue::Integer(-42));
    assert_eq!(
        value(&json!(9_007_199_254_740_992_i64)),
        SqlValue::Integer(1 << 53)
    );
    // What D1 sends for the lock fence's value when no lock is held.
    assert!(matches!(
        value(&serde_json::from_str("9223372036854776000").unwrap()),
        SqlValue::Float(_)
    ));
    assert!(matches!(
        value(&json!(9_007_199_254_740_993_i64)),
        SqlValue::Float(_)
    ));
    assert!(matches!(value(&json!(1.5)), SqlValue::Float(_)));
    assert_eq!(value(&json!("text")), SqlValue::text("text"));
    assert_eq!(value(&json!([0, 255])), SqlValue::Blob(vec![0, 255]));
    assert_eq!(value(&json!([256])), SqlValue::Unreadable);
    assert_eq!(value(&json!({"a": 1})), SqlValue::Unreadable);
    assert_eq!(value(&Value::Null), SqlValue::Null);
}

/// A server answering request number `index` with `reply(index)`.
async fn replying(
    reply: impl Fn(usize) -> Option<(u16, String)> + Send + Sync + 'static,
) -> FakeServer {
    fake_server(Arc::new(move |index, _| reply(index))).await
}

fn ok_rows(rows: &Value) -> String {
    json!({
        "success": true,
        "errors": [],
        "result": [{
            "results": {"columns": [], "rows": rows},
            "success": true,
            "meta": {"changes": 0},
        }],
    })
    .to_string()
}

#[tokio::test]
async fn retryable_d1_errors_are_retried_even_as_http_400() {
    let server = replying(|index| {
        Some(match index {
            0 => error(400, 7500, "D1_ERROR: Network connection lost."),
            1 => error(500, 7500, "D1 DB reset because its code was updated."),
            _ => (200, ok_rows(&json!([]))),
        })
    })
    .await;
    let store = store_at(&server);
    store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn rate_limits_are_retried_after_the_delay_asked_for() {
    let server = fake_server_with_headers(
        Arc::new(|index, _| {
            Some(if index == 0 {
                error(
                    429,
                    971,
                    "Please wait and consider throttling your request speed",
                )
            } else {
                (200, ok_rows(&json!([])))
            })
        }),
        "retry-after: 1\r\n".to_string(),
    )
    .await;
    let store = store_at(&server);
    let started = std::time::Instant::now();
    store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn refused_tokens_fail_at_once_and_say_why() {
    let server = replying(|_| Some(error(403, 10000, "Authentication error"))).await;
    let store = store_at(&server);
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert!(message.contains("HTTP 403"), "{message}");
    assert!(message.contains("the API token was refused"), "{message}");
}

#[tokio::test]
async fn a_failure_reported_with_http_200_is_still_a_failure() {
    let server = replying(|_| {
        Some((
            200,
            json!({
                "success": false,
                "errors": [{"code": 7000, "message": "No route for that URI"}],
                "result": [],
            })
            .to_string(),
        ))
    })
    .await;
    let store = store_at(&server);
    let message = store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("No route"), "{message}");
}

#[tokio::test]
async fn writes_never_quote_messages_and_never_leak_the_token() {
    let server = replying(|_| {
        Some(error(
            400,
            7500,
            "string or blob too big: resource_state_json \"hunter2\" d1-secret-token: SQLITE_TOOBIG",
        ))
    })
    .await;
    let store = store_at(&server);
    let written = store
        .inner
        .execute(Statement::carrying_state(
            "INSERT INTO t VALUES (?)",
            vec![SqlValue::text("hunter2")],
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(!written.contains("hunter2"), "{written}");
    assert!(written.contains("2 MB"), "{written}");
    // A statement without state is quoted, with the token redacted.
    let read = store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap_err()
        .to_string();
    assert!(read.contains("hunter2"), "{read}");
    assert!(!read.contains(TOKEN), "{read}");
    assert!(read.contains("<redacted>"), "{read}");
}

#[tokio::test]
async fn connection_failures_name_neither_the_account_nor_the_database() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut store = D1StateStore::at(configuration(), &url, Duration::from_secs(2)).unwrap();
    store.inner.retry_policy = RetryPolicy {
        retries: 1,
        initial_delay: Duration::from_millis(1),
    };
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert!(message.contains("D1 request failed"), "{message}");
    assert!(!message.contains(ACCOUNT), "{message}");
    assert!(!message.contains(DATABASE), "{message}");
}

#[tokio::test]
async fn an_overloaded_database_is_not_retried() {
    let server = replying(|_| {
        Some(error(
            400,
            7500,
            "D1 DB is overloaded. Too many requests queued.",
        ))
    })
    .await;
    let store = store_at(&server);
    store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap_err();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cloudflare_messages_never_name_the_account_or_database() {
    let server = replying(|_| {
        Some(error(
            404,
            7003,
            &format!("Could not route to /client/v4/accounts/{ACCOUNT}/d1/database/{DATABASE}/raw"),
        ))
    })
    .await;
    let store = store_at(&server);
    let message = store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("Could not route"), "{message}");
    assert!(!message.contains(ACCOUNT), "{message}");
    assert!(!message.contains(DATABASE), "{message}");
    assert!(message.contains("<account>"), "{message}");
}

#[tokio::test]
async fn a_short_result_list_is_refused() {
    let server = replying(|_| {
        Some((
            200,
            json!({"success": true, "errors": [], "result": []}).to_string(),
        ))
    })
    .await;
    let store = store_at(&server);
    let message = store
        .inner
        .execute(Statement::new("SELECT 1", Vec::new()))
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("0 results for 1 statements"), "{message}");
}

/// Against a real D1 database: the whole lifecycle. The database must be one
/// nothing else uses; the test drops cuenv's tables at the end so it can run
/// again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an otherwise unused D1 database (CUENV_INFRASTRUCTURE_TEST_D1_ACCOUNT_ID, CUENV_INFRASTRUCTURE_TEST_D1_DATABASE_ID, CLOUDFLARE_D1_API_TOKEN)"]
async fn lifecycle_against_d1() {
    let variable = |name: &str| std::env::var(name).unwrap();
    let mut store = D1StateStore::try_new(D1Configuration {
        account_id: variable("CUENV_INFRASTRUCTURE_TEST_D1_ACCOUNT_ID"),
        database_id: variable("CUENV_INFRASTRUCTURE_TEST_D1_DATABASE_ID"),
        api_token: variable("CLOUDFLARE_D1_API_TOKEN"),
    })
    .unwrap();
    store.inner.migration_wait = MigrationWait {
        bound: Duration::ZERO,
        poll_interval: Duration::from_millis(100),
        announcement_lifetime: Duration::from_secs(5),
    };
    assert_eq!(store.inner.stored_version().await.unwrap(), 0);
    let scenario = async {
        store.migrate().await?;
        let lock = store.lock(&tenant(), "d1 test").await?;
        store.put(&tenant(), &lock, &record("pet")).await?;
        let stored = store.list(&tenant()).await?;
        assert!(stored[0].same_content(&record("pet")), "{stored:?}");
        let stranger = StateLock::generate();
        assert!(matches!(
            store.put(&tenant(), &stranger, &record("pet")).await,
            Err(InfrastructureError::LockLost { .. })
        ));
        assert!(matches!(
            store.inner.migrate_to(&migrations_with_a_probe()).await,
            Err(InfrastructureError::StateMigrationBlocked { version: 3, .. })
        ));
        store
            .delete(&tenant(), &lock, &record("pet").address)
            .await?;
        store.unlock(&tenant(), &lock).await?;
        Ok::<_, InfrastructureError>(())
    }
    .await;
    for table in [
        "cuenv_infrastructure_migrations",
        "cuenv_infrastructure_resources",
        "cuenv_infrastructure_locks",
        "cuenv_infrastructure_owners",
        "cuenv_infrastructure_pending_migration",
        "cuenv_infrastructure_probe",
    ] {
        store
            .inner
            .execute(Statement::new(
                format!("DROP TABLE IF EXISTS {table}"),
                Vec::new(),
            ))
            .await
            .unwrap();
    }
    scenario.unwrap();
}
