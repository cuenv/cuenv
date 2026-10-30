//! The object store against the fake S3 server.

use std::time::Duration;

use serde_json::json;

use super::fake::{FakeS3, FaultAction};
use super::key::TenantPath;
use super::{
    AddressingChoice, Credentials, ObjectStateFeatures, ObjectStateStore, ObjectStoreConfiguration,
    ObjectTenant, RestoreRequest,
};
use crate::error::InfrastructureError;
use crate::state::{LockRequest, ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::tenant::{ProjectInstance, TenantKey};

const BUCKET: &str = "state";
const SECRET: &str = "secret-access-key-value";

fn tenant() -> TenantKey {
    TenantKey::try_new("example.com/infra", "web").unwrap()
}

fn configuration(server: &FakeS3, package: &str) -> ObjectStoreConfiguration {
    ObjectStoreConfiguration {
        endpoint: Some(server.endpoint.clone()),
        bucket: BUCKET.into(),
        region: "us-east-1".into(),
        root: None,
        addressing: AddressingChoice::Auto,
        credentials: Credentials {
            access_key_id: "access-key-id-value".into(),
            secret_access_key: SECRET.into(),
            session_token: None,
        },
        package: package.into(),
        history: 3,
        minimum_write_interval: None,
        probe_tenant: Some(tenant()),
    }
}

fn store(server: &FakeS3) -> ObjectStateStore {
    ObjectStateStore::try_new(configuration(server, "cuenv")).unwrap()
}

fn path(package: &str, tenant: &TenantKey) -> TenantPath {
    TenantPath::new(None, package, tenant).unwrap()
}

fn record(name: &str, value: &str) -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new("random_pet", name),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 0,
        state: json!({"id": value}),
        private: Vec::new(),
        dependencies: Vec::new(),
        sensitive_attributes: Vec::new(),
        tainted: false,
        identity: None,
        serial: 0,
        generation: uuid::Uuid::nil(),
    }
}

async fn take(store: &ObjectStateStore, instance: Option<&ProjectInstance>) -> StateLock {
    let lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &lock,
                holder: "test",
                instance,
            },
        )
        .await
        .unwrap();
    lock
}

fn document(server: &FakeS3, key: &str) -> serde_json::Value {
    serde_json::from_slice(&server.object(BUCKET, key).unwrap()).unwrap()
}

#[tokio::test]
async fn conforms_to_the_state_store_contract() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    store.migrate().await.unwrap();
    crate::state::conformance::conforms(&store).await;
}

#[tokio::test]
async fn the_layout_matches_the_specification() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let root = ProjectInstance::try_new("services/web", "cuenv").unwrap();
    let lock = take(&store, Some(&root)).await;
    let marker = format!(
        "_locks/example.com/infra/_cuenv/web/_default/{}.json",
        lock.lock_identifier
    );
    assert!(
        server.object(BUCKET, &marker).is_some(),
        "{:?}",
        server.keys(BUCKET)
    );
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    let state = document(&server, "example.com/infra/_cuenv/web/_default/state.json");
    assert_eq!(state["format"], json!({"read": 1, "write": 1}));
    assert_eq!(
        state["tenant"],
        json!({"modulePath": "example.com/infra", "package": "cuenv", "project": "web", "environment": null})
    );
    assert_eq!(state["lastWriter"]["instance"], json!("services/web:cuenv"));
    assert_eq!(
        state["lock"]["lastChange"]["address"],
        json!("random_pet.pet")
    );
    assert!(state["records"]["random_pet.pet"].is_object());
    assert_eq!(
        store.location(&tenant()).as_deref(),
        Some("s3://state/example.com/infra/_cuenv/web/_default/state.json")
    );
    store.unlock(&tenant(), &lock).await.unwrap();
    assert!(
        server.object(BUCKET, &marker).is_none(),
        "the marker goes with the lock"
    );
    let history: Vec<String> = server
        .keys(BUCKET)
        .into_iter()
        .filter(|key| key.contains("/history/"))
        .collect();
    assert_eq!(history.len(), 1, "{history:?}");
    assert!(
        history[0].ends_with("/history/00000000000000000003.json"),
        "{history:?}"
    );
    let snapshot = document(&server, &history[0]);
    assert_eq!(snapshot["lock"], serde_json::Value::Null);
}

#[tokio::test]
async fn every_write_changes_serial_and_writer() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let key = "example.com/infra/_cuenv/web/_default/state.json";
    let lock = take(&store, None).await;
    let before = document(&server, key);
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    let after = document(&server, key);
    assert_eq!(
        after["serial"].as_u64().unwrap(),
        before["serial"].as_u64().unwrap() + 1
    );
    assert_ne!(after["writer"], before["writer"]);
}

#[tokio::test]
async fn a_lost_response_is_resolved_by_reading_the_writer_back() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    server.fault("PUT", "state.json", FaultAction::DropAfterApply);
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    let records = store.list(&tenant()).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].serial, 1, "the write landed once");
    let puts = server
        .requests()
        .iter()
        .filter(|request| request.method == "PUT" && request.key.ends_with("state.json"))
        .count();
    assert_eq!(
        puts, 2,
        "acquire plus exactly one record write, never re-sent blind"
    );

    // A response dropped before the write applied is retried from a fresh
    // read.
    server.fault("PUT", "state.json", FaultAction::DropBeforeApply);
    store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn conflicts_and_rate_limits_are_retried_and_a_lost_lock_is_never_written_through() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    server.fault(
        "PUT",
        "state.json",
        FaultAction::Status(
            409,
            "<Error><Code>ConditionalRequestConflict</Code></Error>".into(),
        ),
    );
    server.fault("PUT", "state.json", FaultAction::Status(429, String::new()));
    server.fault(
        "PUT",
        "state.json",
        FaultAction::Status(412, "<Error><Code>PreconditionFailed</Code></Error>".into()),
    );
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);

    // Another actor force-unlocks and takes the lock: the first run's next
    // write fails, whatever it holds in memory.
    assert!(
        store
            .force_unlock(&tenant(), &lock.lock_identifier)
            .await
            .unwrap()
    );
    let other = take(&store, None).await;
    assert!(matches!(
        store.put(&tenant(), &lock, &record("pet", "stale")).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    store.unlock(&tenant(), &other).await.unwrap();
}

#[tokio::test]
async fn a_vanished_state_object_is_never_recreated_from_empty() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let key = "example.com/infra/_cuenv/web/_default/state.json";
    server.remove_object(BUCKET, key);

    let error = store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &StateLock::generate(),
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("vanished"), "{error}");
    assert!(server.object(BUCKET, key).is_none());

    // While a lock is held, a vanished object is reported, not recreated.
    let restore_lock = StateLock::generate();
    let entry = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: None,
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await
        .unwrap();
    assert_eq!(entry.addresses, ["random_pet.pet"]);
    let restored = store.list(&tenant()).await.unwrap();
    assert_eq!(restored.len(), 1);
    let lock = take(&store, None).await;
    server.remove_object(BUCKET, key);
    let error = store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("vanished"), "{error}");
    assert!(server.object(BUCKET, key).is_none());
}

#[tokio::test]
async fn restore_rewinds_records_with_fresh_generations_and_keeps_serials_rising() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let first = store.history(&tenant()).await.unwrap();
    assert_eq!(first.len(), 1);
    let original = store.list(&tenant()).await.unwrap().remove(0);

    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 2);

    let restore_lock = StateLock::generate();
    let entry = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(first[0].serial),
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await
        .unwrap();
    assert_eq!(entry.serial, first[0].serial);
    let restored = store.list(&tenant()).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].same_content(&original));
    assert_ne!(restored[0].generation, original.generation);
    assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
    let serials: Vec<u64> = store
        .history(&tenant())
        .await
        .unwrap()
        .iter()
        .map(|entry| entry.serial)
        .collect();
    let mut sorted = serials.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(serials, sorted, "serials only rise");
    assert_eq!(serials.len(), 3);
}

#[tokio::test]
async fn history_is_pruned_by_numeric_serial() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    for value in ["a", "b", "c", "d", "e"] {
        let lock = take(&store, None).await;
        store
            .put(&tenant(), &lock, &record("pet", value))
            .await
            .unwrap();
        store.unlock(&tenant(), &lock).await.unwrap();
    }
    let history = store.history(&tenant()).await.unwrap();
    assert_eq!(history.len(), 3);
    let newest = document(&server, "example.com/infra/_cuenv/web/_default/state.json")["serial"]
        .as_u64()
        .unwrap();
    assert_eq!(history.last().unwrap().serial, newest);

    let mut none = configuration(&server, "cuenv");
    none.history = 0;
    let store = ObjectStateStore::try_new(none).unwrap();
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "f"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    assert!(store.history(&tenant()).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_lock_without_changes_writes_no_history() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store.unlock(&tenant(), &lock).await.unwrap();
    assert!(store.history(&tenant()).await.unwrap().is_empty());
}

#[tokio::test]
async fn stale_markers_are_ignored_and_markers_never_name_a_lock_by_themselves() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    server.put_object(
        BUCKET,
        "_locks/example.com/infra/_cuenv/web/_default/stale.json",
        br#"{"holder":"planted"}"#.to_vec(),
    );
    server.put_object(BUCKET, "_locks/not/a/marker", Vec::new());
    assert!(store.locks().await.unwrap().is_empty());
    let lock = take(&store, None).await;
    let locks = store.locks().await.unwrap();
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].lock.lock_identifier, lock.lock_identifier);
    assert_eq!(locks[0].lock.holder, "test");
    assert_eq!(locks[0].package.as_deref(), Some("cuenv"));
}

#[tokio::test]
async fn locks_are_listed_across_packages() {
    let server = FakeS3::start().await.unwrap();
    let ops = ObjectStateStore::try_new(configuration(&server, "ops")).unwrap();
    let cuenv = store(&server);
    let _ops_lock = take(&ops, None).await;
    let _cuenv_lock = take(&cuenv, None).await;
    let mut packages: Vec<String> = cuenv
        .locks()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|lock| lock.package)
        .collect();
    packages.sort();
    assert_eq!(packages, ["cuenv", "ops"]);
}

#[tokio::test]
async fn the_probe_refuses_stores_that_ignore_conditions_or_serve_the_public() {
    let server = FakeS3::start().await.unwrap();
    store(&server).migrate().await.unwrap();
    assert!(
        server
            .keys(BUCKET)
            .iter()
            .all(|key| !key.contains(".probe-")),
        "probe objects are deleted"
    );

    let careless = FakeS3::start().await.unwrap();
    careless.ignore_conditions();
    let error = store(&careless).migrate().await.unwrap_err();
    assert!(error.to_string().contains("capability check"), "{error}");

    let public = FakeS3::start().await.unwrap();
    public.make_public();
    let error = store(&public).migrate().await.unwrap_err();
    assert!(error.to_string().contains("public"), "{error}");

    // A rate-limited probe is retried, never read as "conditions ignored".
    let limited = FakeS3::start().await.unwrap();
    limited.fault("PUT", ".probe-", FaultAction::Status(429, String::new()));
    store(&limited).migrate().await.unwrap();
}

#[tokio::test]
async fn planted_and_newer_documents_are_refused() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let other = TenantKey::try_new("example.com/infra", "api").unwrap();
    let key = path("cuenv", &tenant()).state_key();
    let planted = super::document::StateDocument::new(&path("cuenv", &other), 1);
    server.put_object(BUCKET, &key, planted.encode().unwrap());
    let error = store.list(&tenant()).await.unwrap_err();
    assert!(error.to_string().contains("another tenant"), "{error}");

    let mut newer = super::document::StateDocument::new(&path("cuenv", &tenant()), 1);
    newer.format.read = 99;
    server.put_object(BUCKET, &key, newer.encode().unwrap());
    let error = store.list(&tenant()).await.unwrap_err();
    assert!(error.to_string().contains("newer cuenv"), "{error}");
}

#[tokio::test]
async fn a_newer_write_format_is_never_written_over() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let key = path("cuenv", &tenant()).state_key();
    let mut newer = super::document::StateDocument::new(&path("cuenv", &tenant()), 1);
    newer.format.write = 2;
    server.put_object(BUCKET, &key, newer.encode().unwrap());
    let error = store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &StateLock::generate(),
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("newer cuenv to write"),
        "{error}"
    );
    // Reading is still allowed: only the read format gates readers.
    assert!(store.list(&tenant()).await.unwrap().is_empty());
}

#[tokio::test]
async fn rename_moves_state_and_leaves_a_tombstone() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let renamed = TenantKey::try_new("example.com/infra", "website").unwrap();
    store
        .rename(
            &ObjectTenant {
                package: "cuenv".into(),
                tenant: tenant(),
            },
            &ObjectTenant {
                package: "ops".into(),
                tenant: renamed.clone(),
            },
            "rename",
        )
        .await
        .unwrap();
    let error = store.list(&tenant()).await.unwrap_err();
    assert!(error.to_string().contains("moved to"), "{error}");
    assert!(
        error
            .to_string()
            .contains("example.com/infra/_ops/website/_default/state.json"),
        "{error}"
    );
    let ops = ObjectStateStore::try_new(configuration(&server, "ops")).unwrap();
    assert_eq!(ops.list(&renamed).await.unwrap().len(), 1);
    assert_eq!(ops.current_lock(&renamed).await.unwrap(), None);
    assert!(store.locks().await.unwrap().is_empty());
    // A rename never overwrites.
    let error = ops
        .rename(
            &ObjectTenant {
                package: "ops".into(),
                tenant: renamed.clone(),
            },
            &ObjectTenant {
                package: "ops".into(),
                tenant: renamed,
            },
            "rename",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("same tenant"), "{error}");
}

#[tokio::test]
async fn tenants_lists_every_package_of_the_module_for_rename_detection() {
    let server = FakeS3::start().await.unwrap();
    let cuenv = store(&server);
    let root = ProjectInstance::try_new("web", "cuenv").unwrap();
    let lock = take(&cuenv, Some(&root)).await;
    cuenv
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    cuenv.unlock(&tenant(), &lock).await.unwrap();
    let nested = ObjectStateStore::try_new(configuration(&server, "cuenv")).unwrap();
    let inner = TenantKey::try_new("example.com/infra/web", "web").unwrap();
    let inner_lock = StateLock::generate();
    nested
        .acquire_lock(
            &inner,
            &LockRequest {
                lock: &inner_lock,
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap();
    let tenants = cuenv.tenants("example.com/infra").await.unwrap();
    assert_eq!(
        tenants.len(),
        1,
        "a nested module's tenants are not this module's"
    );
    assert_eq!(tenants[0].project, "web");
    assert_eq!(tenants[0].records, 1);
    assert_eq!(tenants[0].last_writer.as_ref().unwrap().directory(), "web");
}

#[tokio::test]
async fn errors_never_carry_the_endpoint_or_a_credential() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    server.fault(
        "PUT",
        "state.json",
        FaultAction::Status(
            403,
            format!(
                "<Error><Code>AccessDenied</Code><Message>key {SECRET} at {}</Message></Error>",
                server.endpoint
            ),
        ),
    );
    let error = store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains(SECRET), "{error}");
    assert!(error.contains("AccessDenied"), "{error}");
    let port = server.endpoint.rsplit(':').next().unwrap();
    let unreachable = ObjectStateStore::try_new(ObjectStoreConfiguration {
        endpoint: Some(format!(
            "http://127.0.0.1:{}",
            port.parse::<u16>().unwrap() ^ 1
        )),
        ..configuration(&server, "cuenv")
    })
    .unwrap();
    let error = unreachable.list(&tenant()).await.unwrap_err().to_string();
    assert!(!error.contains("127.0.0.1"), "{error}");
}

#[test]
fn configuration_is_validated_before_anything_is_sent() {
    let valid = |endpoint: Option<&str>, region: &str| {
        ObjectStateStore::validate_endpoint(endpoint, region).is_ok()
    };
    assert!(valid(Some("https://abc.r2.cloudflarestorage.com"), "auto"));
    assert!(valid(None, "eu-west-2"));
    assert!(!valid(None, "auto"), "R2 needs an endpoint");
    assert!(valid(Some("http://localhost:9000"), "us-east-1"));
    assert!(valid(Some("http://127.0.0.1:9000/"), "us-east-1"));
    assert!(valid(Some("http://[::1]:9000"), "us-east-1"));
    for invalid in [
        "http://minio:9000",
        "https://user:pass@host",
        "https://host/path",
        "https://host?query",
        "ftp://host",
        "not a url",
    ] {
        assert!(!valid(Some(invalid), "us-east-1"), "{invalid}");
    }
    for bucket in ["ab", "Bucket", "-bucket", "bucket-", "a..b", "b_c"] {
        assert!(
            ObjectStateStore::validate_bucket(bucket).is_err(),
            "{bucket}"
        );
    }
    ObjectStateStore::validate_bucket("my.state-bucket").unwrap();
    assert!(ObjectStateStore::validate_region("EU").is_err());
}

#[tokio::test]
async fn r2_endpoints_pace_writes_to_one_key() {
    let server = FakeS3::start().await.unwrap();
    let mut paced = configuration(&server, "cuenv");
    paced.minimum_write_interval = Some(Duration::from_millis(300));
    let store = ObjectStateStore::try_new(paced).unwrap();
    let lock = take(&store, None).await;
    let started = tokio::time::Instant::now();
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn the_recovery_identity_ignores_region_addressing_and_credentials() {
    let local = |region: &str, addressing, key: &str| {
        ObjectStateStore::try_new(ObjectStoreConfiguration {
            endpoint: Some("http://127.0.0.1:9000".into()),
            bucket: BUCKET.into(),
            region: region.into(),
            root: None,
            addressing,
            credentials: Credentials {
                access_key_id: key.into(),
                secret_access_key: "s".into(),
                session_token: None,
            },
            package: "cuenv".into(),
            history: 3,
            minimum_write_interval: None,
            probe_tenant: None,
        })
        .unwrap()
        .recovery_identity()
    };
    assert_eq!(
        local("us-east-1", AddressingChoice::Auto, "a"),
        local("auto", AddressingChoice::Path, "b")
    );
}

/// The conformance suite and the capability probe against a real
/// S3-compatible server. The `cuenv-infrastructure-e2e` flake check runs it
/// against RustFS; by hand:
///
/// ```sh
/// RUSTFS_ACCESS_KEY=cuenv RUSTFS_SECRET_KEY=cuenv-secret \
///   rustfs server --address 127.0.0.1:9000 /tmp/rustfs &
/// curl --aws-sigv4 aws:amz:us-east-1:s3 --user cuenv:cuenv-secret \
///   -X PUT http://127.0.0.1:9000/cuenv-test
/// CUENV_TEST_S3_ENDPOINT=http://127.0.0.1:9000 CUENV_TEST_S3_BUCKET=cuenv-test \
/// CUENV_TEST_S3_ACCESS_KEY_ID=cuenv CUENV_TEST_S3_SECRET_ACCESS_KEY=cuenv-secret \
///   cargo test -p cuenv-infrastructure --lib -- --ignored against_a_real_server
/// ```
///
/// Each run writes under a fresh random root, so it can share a bucket.
#[tokio::test]
#[ignore = "needs an S3-compatible server: set CUENV_TEST_S3_ENDPOINT, CUENV_TEST_S3_BUCKET, CUENV_TEST_S3_ACCESS_KEY_ID and CUENV_TEST_S3_SECRET_ACCESS_KEY"]
async fn conforms_against_a_real_server() {
    let variable = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let root = format!("cuenv-test-{}", uuid::Uuid::new_v4().simple());
    let store = ObjectStateStore::try_new(ObjectStoreConfiguration {
        endpoint: Some(variable("CUENV_TEST_S3_ENDPOINT")),
        bucket: variable("CUENV_TEST_S3_BUCKET"),
        region: std::env::var("CUENV_TEST_S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
        root: Some(root),
        addressing: AddressingChoice::Auto,
        credentials: Credentials {
            access_key_id: variable("CUENV_TEST_S3_ACCESS_KEY_ID"),
            secret_access_key: variable("CUENV_TEST_S3_SECRET_ACCESS_KEY"),
            session_token: std::env::var("CUENV_TEST_S3_SESSION_TOKEN").ok(),
        },
        package: "cuenv".into(),
        history: 3,
        minimum_write_interval: None,
        probe_tenant: Some(TenantKey::try_new("example.com/conformance", "web").unwrap()),
    })
    .unwrap();
    store.migrate().await.unwrap();
    crate::state::conformance::conforms(&store).await;
}

#[tokio::test]
async fn a_modules_locks_are_listed_under_its_own_prefix() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let _ours = take(&store, None).await;
    let other = TenantKey::try_new("example.com/other", "web").unwrap();
    store
        .acquire_lock(
            &other,
            &LockRequest {
                lock: &StateLock::generate(),
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap();
    let locks = store.module_locks("example.com/infra").await.unwrap();
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].module_path, "example.com/infra");
    assert_eq!(store.locks().await.unwrap().len(), 2);
}
