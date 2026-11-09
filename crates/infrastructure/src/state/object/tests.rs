//! The object store against the fake S3 server.

use std::time::Duration;

use serde_json::json;

use super::client::{Addressing, BucketAccess, Condition, S3Client, S3Failure};
use super::fake::{BarrierPosition, FakeS3, FaultAction};
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

fn client(server: &FakeS3) -> S3Client {
    S3Client::try_new(BucketAccess {
        endpoint: server.endpoint.parse().unwrap(),
        bucket: BUCKET.to_string(),
        region: "us-east-1".to_string(),
        addressing: Addressing::Path,
        credentials: configuration(server, "cuenv").credentials,
        minimum_write_interval: Duration::ZERO,
        timeout: Duration::from_secs(5),
    })
    .unwrap()
}

fn path(package: &str, tenant: &TenantKey) -> TenantPath {
    TenantPath::try_new(None, package, tenant).unwrap()
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
async fn exhausted_document_serials_refuse_acquire_mutation_and_release() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    let key = path("cuenv", &tenant()).state_key();
    let mut state = document(&server, &key);
    state["serial"] = json!(u64::MAX - 1);
    let held = serde_json::to_vec(&state).unwrap();
    server.put_object(BUCKET, &key, held.clone());

    assert!(
        store
            .put(&tenant(), &lock, &record("pet", "a"))
            .await
            .is_err()
    );
    assert_eq!(server.object(BUCKET, &key), Some(held));
    assert!(store.unlock(&tenant(), &lock).await.unwrap());
    let mut state = document(&server, &key);
    assert_eq!(state["serial"], json!(u64::MAX));

    let next_lock = StateLock::generate();
    let exhausted = server.object(BUCKET, &key).unwrap();
    assert!(
        store
            .acquire_lock(
                &tenant(),
                &LockRequest {
                    lock: &next_lock,
                    holder: "test",
                    instance: None,
                }
            )
            .await
            .is_err()
    );
    assert_eq!(server.object(BUCKET, &key), Some(exhausted));

    state["lock"] = json!({
        "lockIdentifier": lock.lock_identifier, "holder": "test", "acquiredAt": "now"
    });
    let exhausted_held = serde_json::to_vec(&state).unwrap();
    server.put_object(BUCKET, &key, exhausted_held.clone());
    assert!(store.unlock(&tenant(), &lock).await.is_err());
    assert_eq!(server.object(BUCKET, &key), Some(exhausted_held));
}

#[tokio::test]
async fn applied_rate_limits_use_write_evidence_without_replaying() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let key = path("cuenv", &tenant()).state_key();
    let lock = take(&store, None).await;
    let before = document(&server, &key)["serial"].as_u64().unwrap();
    server.fault(
        "PUT",
        "state.json",
        FaultAction::StatusAfterApply(429, "rate limited".into()),
    );
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    let after = document(&server, &key);
    assert_eq!(after["serial"], json!(before + 1));
    assert_eq!(after["records"]["random_pet.pet"]["serial"], json!(1));

    server.fault(
        "PUT",
        "state.json",
        FaultAction::StatusAfterApply(429, "rate limited".into()),
    );
    assert!(store.unlock(&tenant(), &lock).await.unwrap());
    let serial = document(&server, &key)["serial"].as_u64().unwrap();
    let next_lock = StateLock::generate();
    server.fault(
        "PUT",
        "state.json",
        FaultAction::StatusAfterApply(429, "rate limited".into()),
    );
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &next_lock,
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(document(&server, &key)["serial"], json!(serial + 1));
}

#[tokio::test]
async fn lost_write_evidence_never_replays_an_ambiguous_mutation() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let key = path("cuenv", &tenant()).state_key();
    let lock = take(&store, None).await;
    server.fault(
        "PUT",
        "state.json",
        FaultAction::StatusAfterApply(429, "rate limited".into()),
    );
    let mut barrier = server.pause_next("PUT", "state.json", BarrierPosition::AfterApply);
    let racing = async {
        barrier.reached(Duration::from_secs(10)).await.unwrap();
        let mut changed = document(&server, &key);
        changed["serial"] = json!(changed["serial"].as_u64().unwrap() + 1);
        changed["writer"] = json!(uuid::Uuid::new_v4().to_string());
        let changed = serde_json::to_vec(&changed).unwrap();
        server.put_object(BUCKET, &key, changed.clone());
        barrier.release(Duration::from_secs(10)).await.unwrap();
        changed
    };
    let project = tenant();
    let resource = record("pet", "a");
    let (outcome, changed) = tokio::join!(store.put(&project, &lock, &resource), racing);
    assert!(outcome.is_err());
    assert_eq!(server.object(BUCKET, &key), Some(changed));
}

#[tokio::test]
async fn rollback_tombstone_retains_records_for_recovery() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.tombstone(&tenant(), &lock, "source").await.unwrap();
    let state = document(&server, &path("cuenv", &tenant()).state_key());
    assert!(state["records"]["random_pet.pet"].is_object());
    assert!(state["tombstone"].is_object());
    assert!(store.list(&tenant()).await.is_err());
    assert!(store.unlock(&tenant(), &lock).await.unwrap());
    assert!(store.list(&tenant()).await.is_err());
}

#[tokio::test]
async fn record_updates_refuse_exhausted_or_extended_versions() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    let key = path("cuenv", &tenant()).state_key();
    let mut raw = document(&server, &key);
    raw["records"]["random_pet.pet"]["serial"] = json!(i64::MAX);
    let exhausted = serde_json::to_vec(&raw).unwrap();
    server.put_object(BUCKET, &key, exhausted.clone());
    assert!(
        store
            .put(&tenant(), &lock, &record("pet", "b"))
            .await
            .is_err()
    );
    assert_eq!(server.object(BUCKET, &key), Some(exhausted));
    raw["records"]["random_pet.pet"]["serial"] = json!(1);
    raw["records"]["random_pet.pet"]["futureExtension"] = json!("preserved");
    let extended = serde_json::to_vec(&raw).unwrap();
    server.put_object(BUCKET, &key, extended.clone());
    assert!(
        store
            .put(&tenant(), &lock, &record("pet", "b"))
            .await
            .is_err()
    );
    assert_eq!(server.object(BUCKET, &key), Some(extended));
}

#[tokio::test]
async fn restore_keeps_unknown_snapshot_record_fields() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let serial = store.history(&tenant()).await.unwrap()[0].serial;
    let history_key = path("cuenv", &tenant()).history_key(serial);
    let mut snapshot = document(&server, &history_key);
    snapshot["records"]["random_pet.pet"]["futureExtension"] = json!({"kept": true});
    server.put_object(BUCKET, &history_key, serde_json::to_vec(&snapshot).unwrap());
    let lock = StateLock::generate();
    store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(serial),
                holder: "restore",
                lock: &lock,
            },
        )
        .await
        .unwrap();
    let restored = document(&server, &path("cuenv", &tenant()).state_key());
    assert_eq!(
        restored["records"]["random_pet.pet"]["futureExtension"]["kept"],
        true
    );
    assert_ne!(
        restored["records"]["random_pet.pet"]["generation"],
        snapshot["records"]["random_pet.pet"]["generation"]
    );
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

    // An unconfirmed response cannot prove that the write did not apply.
    server.fault("PUT", "state.json", FaultAction::DropBeforeApply);
    assert!(
        store
            .put(&tenant(), &lock, &record("cat", "b"))
            .await
            .is_err()
    );
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);
    store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn history_readback_requires_the_exact_snapshot_body() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let path = path("cuenv", &tenant());
    let mut document = super::document::StateDocument::new(&path, 9);
    document.writer = uuid::Uuid::new_v4().to_string();
    let mut foreign = document.clone();
    foreign.written_by = "another body with the same writer".into();
    let foreign_body = foreign.encode().unwrap();
    let key = path.history_key(document.serial);
    server.put_object(BUCKET, &key, foreign_body.clone());

    assert!(store.snapshot_and_prune(&path, &document).await.is_err());
    assert_eq!(server.object(BUCKET, &key), Some(foreign_body));
}

#[tokio::test]
async fn malformed_record_keys_never_look_like_an_empty_or_partial_tenant()
-> Result<(), Box<dyn std::error::Error>> {
    const PRIVATE_KEY: &str = "STATE_PRIVATE_KEY_SENTINEL";
    for include_valid_record in [false, true] {
        let server = FakeS3::start().await?;
        let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
        let tenant_path = path("cuenv", &tenant());
        let key = tenant_path.state_key();
        let mut document = super::document::StateDocument::new(&tenant_path, 1);
        if include_valid_record {
            document.store(&record("good", "value"))?;
        }
        let mut invalid = record("bad", "value");
        invalid.address = ResourceAddress::new("invalid", PRIVATE_KEY);
        document.store(&invalid)?;
        let original = document.encode()?;
        server.put_object(BUCKET, &key, original.clone());

        let Err(addresses_error) = store.addresses(&tenant()).await else {
            return Err("invalid record key was omitted from addresses".into());
        };
        let Err(list_error) = store.list(&tenant()).await else {
            return Err("invalid record key was accepted in a state list".into());
        };
        for error in [addresses_error, list_error] {
            let text = error.to_string();
            assert!(text.contains("unknown resource"), "{text}");
            assert!(!text.contains(PRIVATE_KEY), "{text}");
        }
        assert_eq!(server.object(BUCKET, &key), Some(original));
    }
    Ok(())
}

#[tokio::test]
async fn mismatched_embedded_address_fails_list_but_keeps_key_removable()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let tenant_path = path("cuenv", &tenant());
    let key = tenant_path.state_key();
    let address = ResourceAddress::new("random_pet", "pet");
    let mut document = super::document::StateDocument::new(&tenant_path, 1);
    document.records.insert(
        address.to_string(),
        serde_json::to_value(record("other", "value"))?,
    );
    server.put_object(BUCKET, &key, document.encode()?);

    assert_eq!(store.addresses(&tenant()).await?, vec![address.clone()]);
    assert!(store.list(&tenant()).await.is_err());
    let lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    store.delete(&tenant(), &lock, &address).await?;
    assert!(store.unlock(&tenant(), &lock).await?);
    assert!(store.addresses(&tenant()).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn damaged_history_is_never_restored_or_hidden_from_history_preview()
-> Result<(), Box<dyn std::error::Error>> {
    const PRIVATE_KEY: &str = "STATE_PRIVATE_KEY_SENTINEL";
    for damage in [
        "key",
        "address",
        "field",
        "serial_missing",
        "serial_zero",
        "serial_negative",
        "snapshot_serial",
        "write_format",
    ] {
        let server = FakeS3::start().await?;
        let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
        let tenant_path = path("cuenv", &tenant());
        let state_key = tenant_path.state_key();
        let initial_lock = StateLock::generate();
        store
            .acquire_lock(
                &tenant(),
                &LockRequest {
                    lock: &initial_lock,
                    holder: "test",
                    instance: None,
                },
            )
            .await?;
        store
            .put(&tenant(), &initial_lock, &record("pet", "value"))
            .await?;
        assert!(store.unlock(&tenant(), &initial_lock).await?);
        let history = store.history(&tenant()).await?;
        let serial = history.first().ok_or("missing history snapshot")?.serial;
        let history_key = tenant_path.history_key(serial);
        let mut snapshot: serde_json::Value = serde_json::from_slice(
            &server
                .object(BUCKET, &history_key)
                .ok_or("missing snapshot")?,
        )?;
        match damage {
            "key" => {
                let mut invalid = record("bad", "value");
                invalid.address = ResourceAddress::new("invalid", PRIVATE_KEY);
                snapshot["records"][invalid.address.to_string()] = serde_json::to_value(&invalid)?;
            }
            "address" => snapshot["records"]["random_pet.pet"]["address"]["name"] = json!("other"),
            "field" => snapshot["records"]["random_pet.pet"]["schema_version"] = json!(PRIVATE_KEY),
            "serial_missing" => {
                snapshot["records"]["random_pet.pet"]
                    .as_object_mut()
                    .ok_or("invalid snapshot record")?
                    .remove("serial");
            }
            "serial_zero" => snapshot["records"]["random_pet.pet"]["serial"] = json!(0),
            "serial_negative" => snapshot["records"]["random_pet.pet"]["serial"] = json!(-1),
            "snapshot_serial" => snapshot["serial"] = json!(serial + 1),
            "write_format" => snapshot["format"]["write"] = json!(999),
            _ => return Err("unknown history corruption".into()),
        }
        let damaged = serde_json::to_vec(&snapshot)?;
        server.put_object(BUCKET, &history_key, damaged.clone());
        let original_state = server.object(BUCKET, &state_key).ok_or("missing state")?;
        let request_count = server.requests().len();
        if matches!(damage, "key" | "snapshot_serial") {
            let Err(error) = store.history(&tenant()).await else {
                return Err("history preview omitted an invalid key".into());
            };
            assert!(!error.to_string().contains(PRIVATE_KEY));
        }
        let restore_lock = StateLock::generate();
        let Err(error) = store
            .restore(
                &tenant(),
                &RestoreRequest {
                    serial: Some(serial),
                    holder: "restore",
                    lock: &restore_lock,
                },
            )
            .await
        else {
            return Err(format!("restored a snapshot with invalid {damage}").into());
        };
        assert!(!error.to_string().contains(PRIVATE_KEY));
        assert_eq!(server.object(BUCKET, &state_key), Some(original_state));
        assert_eq!(server.object(BUCKET, &history_key), Some(damaged));
        assert!(
            server
                .object(
                    BUCKET,
                    &tenant_path.marker_key(&restore_lock.lock_identifier)
                )
                .is_none()
        );
        assert!(
            server
                .requests()
                .iter()
                .skip(request_count)
                .all(|request| { request.method != "PUT" && request.method != "DELETE" })
        );
    }
    Ok(())
}

#[tokio::test]
async fn restore_rejects_an_extended_current_writer_before_any_write()
-> Result<(), Box<dyn std::error::Error>> {
    const PRIVATE_EXTENSION: &str = "STATE_PRIVATE_WRITER_SENTINEL";
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let tenant_path = path("cuenv", &tenant());
    let state_key = tenant_path.state_key();
    let initial_lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &initial_lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    store
        .put(&tenant(), &initial_lock, &record("pet", "value"))
        .await?;
    assert!(store.unlock(&tenant(), &initial_lock).await?);
    let history = store.history(&tenant()).await?;
    let serial = history.first().ok_or("missing history snapshot")?.serial;
    let mut current: serde_json::Value =
        serde_json::from_slice(&server.object(BUCKET, &state_key).ok_or("missing state")?)?;
    current["format"]["write"] = json!(2);
    current["lastWriter"] = json!({
        "instance": "project:cuenv",
        "at": "2026-01-01T00:00:00Z",
        "futureWriter": PRIVATE_EXTENSION,
    });
    let original = serde_json::to_vec(&current)?;
    server.put_object(BUCKET, &state_key, original.clone());
    let request_count = server.requests().len();
    let restore_lock = StateLock::generate();
    let Err(error) = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(serial),
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await
    else {
        return Err("restore replaced an extended current writer".into());
    };
    assert!(!error.to_string().contains(PRIVATE_EXTENSION));
    assert_eq!(server.object(BUCKET, &state_key), Some(original));
    assert!(
        server
            .object(
                BUCKET,
                &tenant_path.marker_key(&restore_lock.lock_identifier)
            )
            .is_none()
    );
    assert!(
        server
            .requests()
            .iter()
            .skip(request_count)
            .all(|request| request.method != "PUT" && request.method != "DELETE")
    );
    Ok(())
}

#[tokio::test]
async fn restore_replaces_a_nil_snapshot_generation_with_a_fresh_one()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let tenant_path = path("cuenv", &tenant());
    let initial_lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &initial_lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    store
        .put(&tenant(), &initial_lock, &record("pet", "value"))
        .await?;
    assert!(store.unlock(&tenant(), &initial_lock).await?);
    let history = store.history(&tenant()).await?;
    let serial = history.first().ok_or("missing history snapshot")?.serial;
    let history_key = tenant_path.history_key(serial);
    let mut snapshot: serde_json::Value = serde_json::from_slice(
        &server
            .object(BUCKET, &history_key)
            .ok_or("missing snapshot")?,
    )?;
    snapshot["records"]["random_pet.pet"]["generation"] = json!(uuid::Uuid::nil());
    server.put_object(BUCKET, &history_key, serde_json::to_vec(&snapshot)?);

    let restore_lock = StateLock::generate();
    let restored = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(serial),
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await?;
    assert_eq!(restored.addresses, ["random_pet.pet"]);
    let records = store.list(&tenant()).await?;
    assert_eq!(records.len(), 1);
    assert!(!records[0].generation.is_nil());
    Ok(())
}

#[tokio::test]
async fn missing_state_restore_preserves_snapshot_extensions_and_masks_untrusted_writer()
-> Result<(), Box<dyn std::error::Error>> {
    const PRIVATE_WRITER: &str = "STATE_PRIVATE_WRITTEN_BY_SENTINEL";
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let tenant_path = path("cuenv", &tenant());
    let state_key = tenant_path.state_key();
    let initial_lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &initial_lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    store
        .put(&tenant(), &initial_lock, &record("pet", "value"))
        .await?;
    assert!(store.unlock(&tenant(), &initial_lock).await?);
    let history = store.history(&tenant()).await?;
    let serial = history.first().ok_or("missing history snapshot")?.serial;
    let history_key = tenant_path.history_key(serial);
    let mut snapshot: serde_json::Value = serde_json::from_slice(
        &server
            .object(BUCKET, &history_key)
            .ok_or("missing snapshot")?,
    )?;
    for writer in [
        PRIVATE_WRITER,
        "cuenv 1.0.0-STATE-PRIVATE-WRITTEN-BY-SENTINEL",
        "cuenv 1.0.0+STATE-PRIVATE-WRITTEN-BY-SENTINEL",
    ] {
        snapshot["writtenBy"] = json!(writer);
        server.put_object(BUCKET, &history_key, serde_json::to_vec(&snapshot)?);
        assert_eq!(
            store
                .history(&tenant())
                .await?
                .first()
                .ok_or("missing history snapshot")?
                .written_by,
            "unknown"
        );
    }
    snapshot["writtenBy"] = json!(PRIVATE_WRITER);
    snapshot["format"]["futureFormat"] = json!({"kept": true});
    snapshot["tenant"]["futureTenant"] = json!({"kept": true});
    snapshot["futureDocument"] = json!({"kept": true});
    snapshot["origin"] = json!({
        "kind": "rename",
        "id": "earlier-operation",
        "source": "previous-state",
        "futureOrigin": {"kept": true},
    });
    snapshot["lastWriter"] = json!({
        "instance": "project:cuenv",
        "at": "2026-01-01T00:00:00Z",
        "futureWriter": {"kept": true},
    });
    server.put_object(BUCKET, &history_key, serde_json::to_vec(&snapshot)?);
    let history = store.history(&tenant()).await?;
    assert_eq!(
        history
            .first()
            .ok_or("missing history snapshot")?
            .written_by,
        "unknown"
    );
    server.remove_object(BUCKET, &state_key);

    let restore_lock = StateLock::generate();
    let entry = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(serial),
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await?;
    assert_eq!(entry.written_by, "unknown");
    let restored: serde_json::Value = serde_json::from_slice(
        &server
            .object(BUCKET, &state_key)
            .ok_or("missing restored state")?,
    )?;
    assert_eq!(
        restored["format"]["futureFormat"],
        snapshot["format"]["futureFormat"]
    );
    assert_eq!(
        restored["tenant"]["futureTenant"],
        snapshot["tenant"]["futureTenant"]
    );
    assert_eq!(restored["futureDocument"], snapshot["futureDocument"]);
    assert_eq!(restored["origin"], snapshot["origin"]);
    assert_eq!(restored["lastWriter"], snapshot["lastWriter"]);
    Ok(())
}

#[tokio::test]
async fn restore_fails_when_its_lock_is_force_released_before_its_unlock()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let restoring = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let contender = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let original_lock = StateLock::generate();
    restoring
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &original_lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    restoring
        .put(&tenant(), &original_lock, &record("pet", "before"))
        .await?;
    assert!(restoring.unlock(&tenant(), &original_lock).await?);
    let history = restoring.history(&tenant()).await?;
    let serial = history.first().ok_or("missing history snapshot")?.serial;
    let changed_lock = StateLock::generate();
    restoring
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &changed_lock,
                holder: "test",
                instance: None,
            },
        )
        .await?;
    restoring
        .put(&tenant(), &changed_lock, &record("pet", "after"))
        .await?;
    assert!(restoring.unlock(&tenant(), &changed_lock).await?);

    let mut acquired = server.pause_next("PUT", "state.json", BarrierPosition::AfterApply);
    let mut restored = server.pause_next("PUT", "state.json", BarrierPosition::AfterApply);
    let mut releasing = server.pause_next("PUT", "state.json", BarrierPosition::BeforeApply);
    let restore_lock = StateLock::generate();
    let worker_lock = restore_lock.clone();
    let worker_tenant = tenant();
    let worker = tokio::spawn(async move {
        restoring
            .restore(
                &worker_tenant,
                &RestoreRequest {
                    serial: Some(serial),
                    holder: "restore",
                    lock: &worker_lock,
                },
            )
            .await
    });
    acquired.reached(Duration::from_secs(10)).await?;
    acquired.release(Duration::from_secs(10)).await?;
    restored.reached(Duration::from_secs(10)).await?;
    restored.release(Duration::from_secs(10)).await?;
    releasing.reached(Duration::from_secs(10)).await?;

    assert!(
        contender
            .force_unlock(&tenant(), &restore_lock.lock_identifier)
            .await?
    );
    let takeover = StateLock::generate();
    contender
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &takeover,
                holder: "takeover",
                instance: None,
            },
        )
        .await?;
    releasing.release(Duration::from_secs(10)).await?;
    assert!(worker.await?.is_err());
    let current = contender
        .current_lock(&tenant())
        .await?
        .ok_or("missing takeover lock")?;
    assert_eq!(
        current.identifier(),
        Some(takeover.lock_identifier.as_str())
    );
    assert!(contender.unlock(&tenant(), &takeover).await?);
    Ok(())
}

#[tokio::test]
async fn ambiguous_writes_stop_and_a_lost_lock_is_never_written_through() {
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
    assert!(
        store
            .put(&tenant(), &lock, &record("pet", "a"))
            .await
            .is_err()
    );
    assert!(
        store
            .put(&tenant(), &lock, &record("pet", "a"))
            .await
            .is_err()
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
async fn restore_keeps_changes_in_flight_and_never_revives_cleared_ones() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let restore_lock = StateLock::generate();
    let restore = |serial: u64| RestoreRequest {
        serial: Some(serial),
        holder: "restore",
        lock: &restore_lock,
    };
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let before_marker = store.history(&tenant()).await.unwrap()[0].serial;

    let address = ResourceAddress::new("random_pet", "cat");
    let lock = take(&store, None).await;
    let marker =
        crate::in_flight::marker_record(&address, "create", &lock.lock_identifier).unwrap();
    store.put(&tenant(), &lock, &marker).await.unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let with_marker = store
        .history(&tenant())
        .await
        .unwrap()
        .last()
        .unwrap()
        .serial;

    // Rewinding to before the change keeps it fenced.
    store
        .restore(&tenant(), &restore(before_marker))
        .await
        .unwrap();
    let (resources, in_flight) = crate::in_flight::separate(store.list(&tenant()).await.unwrap());
    assert_eq!(resources.len(), 1);
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].address, address);

    // Once cleared, a snapshot taken while it was in flight does not bring
    // the marker back.
    let lock = take(&store, None).await;
    store
        .delete(&tenant(), &lock, &marker.address)
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    store
        .restore(&tenant(), &restore(with_marker))
        .await
        .unwrap();
    let (resources, in_flight) = crate::in_flight::separate(store.list(&tenant()).await.unwrap());
    assert_eq!(resources.len(), 1);
    assert!(in_flight.is_empty(), "{in_flight:?}");
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
async fn restore_reserves_serials_before_writing_or_creating_a_lock() {
    for (missing, history_serial, succeeds) in [
        (false, u64::MAX - 1, false),
        (true, u64::MAX - 1, false),
        (false, u64::MAX - 2, true),
        (true, u64::MAX - 2, true),
    ] {
        let server = FakeS3::start().await.unwrap();
        let store = store(&server);
        let tenant_path = path("cuenv", &tenant());
        let key = tenant_path.state_key();
        let lock = take(&store, None).await;
        store
            .put(&tenant(), &lock, &record("pet", "a"))
            .await
            .unwrap();
        store.unlock(&tenant(), &lock).await.unwrap();
        let mut snapshot = document(&server, &key);
        snapshot["serial"] = json!(history_serial);
        server.put_object(
            BUCKET,
            &tenant_path.history_key(history_serial),
            serde_json::to_vec(&snapshot).unwrap(),
        );
        if missing {
            server.remove_object(BUCKET, &key);
        }
        let state_before = server.object(BUCKET, &key);
        let keys_before = server.keys(BUCKET);
        let requests_before = server.requests().len();
        let restore_lock = StateLock::generate();
        let result = store
            .restore(
                &tenant(),
                &RestoreRequest {
                    serial: Some(history_serial),
                    holder: "restore",
                    lock: &restore_lock,
                },
            )
            .await;
        if succeeds {
            result.unwrap();
            assert_eq!(document(&server, &key)["serial"], json!(u64::MAX));
            assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
            assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(server.object(BUCKET, &key), state_before);
            assert_eq!(server.keys(BUCKET), keys_before);
            assert!(
                server.requests()[requests_before..]
                    .iter()
                    .all(|request| request.method != "PUT")
            );
        }
    }
}

#[tokio::test]
async fn restore_refuses_an_existing_document_without_three_serials() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let key = path("cuenv", &tenant()).state_key();
    let mut state = document(&server, &key);
    state["serial"] = json!(u64::MAX - 2);
    let original = serde_json::to_vec(&state).unwrap();
    server.put_object(BUCKET, &key, original.clone());
    let requests_before = server.requests().len();
    let lock = StateLock::generate();
    assert!(
        store
            .restore(
                &tenant(),
                &RestoreRequest {
                    serial: None,
                    holder: "restore",
                    lock: &lock,
                }
            )
            .await
            .is_err()
    );
    assert_eq!(server.object(BUCKET, &key), Some(original));
    assert!(
        server.requests()[requests_before..]
            .iter()
            .all(|request| request.method != "PUT")
    );
}

#[tokio::test]
async fn format_two_writes_and_restore_retain_migration_provenance() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let first = store.history(&tenant()).await.unwrap()[0].serial;
    let key = path("cuenv", &tenant()).state_key();
    let mut raw = document(&server, &key);
    raw["format"]["write"] = json!(2);
    raw["origin"] = json!({"kind": "migration", "id": "op", "source": "source"});
    raw["migrationMarker"] = json!({"operation": "op", "baseline": "original"});
    server.put_object(BUCKET, &key, serde_json::to_vec(&raw).unwrap());
    let previous_writer = raw["writer"].clone();
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("cat", "b"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let changed = document(&server, &key);
    assert_eq!(changed["migrationMarker"], raw["migrationMarker"]);
    assert_eq!(changed["origin"], raw["origin"]);
    assert_eq!(changed["format"]["write"], 2);
    assert_ne!(changed["writer"], previous_writer);
    let restore_lock = StateLock::generate();
    store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: Some(first),
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await
        .unwrap();
    let restored = document(&server, &key);
    assert_eq!(restored["migrationMarker"], raw["migrationMarker"]);
    assert_eq!(restored["origin"], raw["origin"]);
    assert_eq!(restored["format"]["write"], 2);
    assert_ne!(restored["writer"], changed["writer"]);
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);
    let renamed = ObjectTenant {
        package: "cuenv".into(),
        tenant: TenantKey::try_new("example.com/infra", "renamed").unwrap(),
    };
    let source = ObjectTenant {
        package: "cuenv".into(),
        tenant: tenant(),
    };
    assert!(store.rename(&source, &renamed, "test").await.is_err());
    assert_eq!(document(&server, &key), restored);
}

#[tokio::test]
async fn restore_never_recreates_missing_migration_bound_state() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "a"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let manifest = super::MigrationManifest {
        tenants: vec![super::ManifestEntry {
            environment: None,
            status: "migrated".into(),
            migration: "op".into(),
            committed_serial: Some(1),
            commit_evidence: None,
            binding: None,
        }],
        revision: None,
        write_nonce: None,
        serial: None,
    };
    server.put_object(
        BUCKET,
        &path("cuenv", &tenant()).migration_manifest_key(),
        serde_json::to_vec(&manifest).unwrap(),
    );
    let key = path("cuenv", &tenant()).state_key();
    server.remove_object(BUCKET, &key);
    let restore_lock = StateLock::generate();
    let error = store
        .restore(
            &tenant(),
            &RestoreRequest {
                serial: None,
                holder: "restore",
                lock: &restore_lock,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("migration-bound"), "{error}");
    assert!(server.object(BUCKET, &key).is_none());
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
    server.put_object(
        BUCKET,
        "_locks/example.com/infra/_cuenv/absent/_default/stale.json",
        Vec::new(),
    );
    assert!(store.locks().await.unwrap().is_empty());
    let lock = take(&store, None).await;
    let locks = store.locks().await.unwrap();
    assert_eq!(locks.len(), 1);
    assert_eq!(
        locks[0].lock.identifier(),
        Some(lock.lock_identifier.as_str())
    );
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
    newer.format.write = 3;
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
async fn rename_reserves_serials_before_creating_either_lock() {
    for (serial, succeeds) in [(u64::MAX - 2, false), (u64::MAX - 3, true)] {
        let server = FakeS3::start().await.unwrap();
        let store = store(&server);
        let lock = take(&store, None).await;
        store
            .put(&tenant(), &lock, &record("pet", "a"))
            .await
            .unwrap();
        store.unlock(&tenant(), &lock).await.unwrap();
        let source = ObjectTenant {
            package: "cuenv".into(),
            tenant: tenant(),
        };
        let target_tenant = TenantKey::try_new("example.com/infra", "renamed").unwrap();
        let target = ObjectTenant {
            package: "cuenv".into(),
            tenant: target_tenant.clone(),
        };
        let source_key = path("cuenv", &source.tenant).state_key();
        let target_key = path("cuenv", &target.tenant).state_key();
        let mut state = document(&server, &source_key);
        state["serial"] = json!(serial);
        let original = serde_json::to_vec(&state).unwrap();
        server.put_object(BUCKET, &source_key, original.clone());
        let keys_before = server.keys(BUCKET);
        let requests_before = server.requests().len();
        let result = store.rename(&source, &target, "rename").await;
        if succeeds {
            result.unwrap();
            assert_eq!(document(&server, &source_key)["serial"], json!(u64::MAX));
            assert_eq!(document(&server, &target_key)["serial"], json!(u64::MAX));
            assert_eq!(store.current_lock(&target_tenant).await.unwrap(), None);
            assert_eq!(store.list(&target_tenant).await.unwrap().len(), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(server.object(BUCKET, &source_key), Some(original));
            assert!(server.object(BUCKET, &target_key).is_none());
            assert_eq!(server.keys(BUCKET), keys_before);
            assert!(
                server.requests()[requests_before..]
                    .iter()
                    .all(|request| request.method != "PUT")
            );
        }
    }
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

#[tokio::test]
async fn provider_error_fields_do_not_enter_returned_diagnostics() {
    const CODE_SENTINEL: &str = "private-state-in-code";
    const MESSAGE_SENTINEL: &str = "private-lock-in-message";
    const REGION_SENTINEL: &str = "private-state-in-region";
    const HEADER_SENTINEL: &str = "private-lock-in-region-header";

    let server = FakeS3::start().await.unwrap();
    let client = client(&server);

    for (status, code, expected) in [
        (403, CODE_SENTINEL, "HTTP 403"),
        (301, "PermanentRedirect", "another region"),
        (403, "ExpiredToken", "session token has expired"),
        (404, "NoSuchKey", "no object"),
        (404, "NoSuchBucket", "bucket does not exist"),
        (412, "PreconditionFailed", "precondition"),
        (429, "SlowDown", "rate-limited"),
        (500, CODE_SENTINEL, "HTTP 500"),
    ] {
        server.fault(
            "PUT",
            "diagnostic-probe",
            FaultAction::StatusWithHeaders {
                status,
                headers: vec![
                    ("x-amz-bucket-region".into(), HEADER_SENTINEL.into()),
                    ("retry-after".into(), "2".into()),
                ],
                body: format!(
                    "<Error><Code>{code}</Code><Message>{MESSAGE_SENTINEL} {SECRET}</Message><Region>{REGION_SENTINEL}</Region></Error>"
                ),
            },
        );
        let failure = client
            .put("diagnostic-probe", Vec::new(), &Condition::None)
            .await
            .unwrap()
            .unwrap_err();
        if status == 412 {
            assert_eq!(failure, S3Failure::PreconditionFailed);
        } else if code == "NoSuchKey" {
            assert_eq!(failure, S3Failure::Missing);
        } else if status == 429 {
            assert_eq!(
                failure,
                S3Failure::RateLimited(Some(Duration::from_secs(2)))
            );
        }
        let diagnostic = format!("{failure:?} {}", failure.clone().into_error("writing"));
        assert!(diagnostic.contains(expected), "{diagnostic}");
        for sentinel in [
            CODE_SENTINEL,
            MESSAGE_SENTINEL,
            REGION_SENTINEL,
            HEADER_SENTINEL,
            SECRET,
        ] {
            assert!(!diagnostic.contains(sentinel), "{diagnostic}");
        }
    }

    server.fault(
        "PUT",
        "diagnostic-probe",
        FaultAction::StatusWithHeaders {
            status: 429,
            headers: vec![("retry-after".into(), "18446744073709551615".into())],
            body: String::new(),
        },
    );
    assert_eq!(
        client
            .put("diagnostic-probe", Vec::new(), &Condition::None)
            .await
            .unwrap()
            .unwrap_err(),
        S3Failure::RateLimited(Some(Duration::from_secs(30)))
    );
}

#[tokio::test]
async fn listings_refuse_invalid_utf8_prefixes_and_repeated_tokens() {
    let server = FakeS3::start().await.unwrap();
    let client = client(&server);
    let outside_prefix = "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>different%2Fstate.json</Key></Contents></ListBucketResult>";
    server.fault_exact("GET", "", FaultAction::Status(200, outside_prefix.into()));
    let failure = client.list("expected/").await.unwrap().unwrap_err();
    assert!(matches!(&failure, S3Failure::Permanent(_)));
    assert!(failure.into_error("listing").to_string().contains("prefix"));

    let mut invalid_utf8 = b"<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>expected/".to_vec();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(b"</Key></Contents></ListBucketResult>");
    server.fault_exact("GET", "", FaultAction::StatusBytes(200, invalid_utf8));
    let failure = client.list("expected/").await.unwrap().unwrap_err();
    assert!(matches!(&failure, S3Failure::Permanent(_)));
    assert!(failure.into_error("listing").to_string().contains("UTF-8"));

    let repeated = "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated><NextContinuationToken>repeat</NextContinuationToken></ListBucketResult>";
    server.fault_exact("GET", "", FaultAction::Status(200, repeated.into()));
    server.fault_exact("GET", "", FaultAction::Status(200, repeated.into()));
    let failure = client.list("expected/").await.unwrap().unwrap_err();
    assert!(matches!(&failure, S3Failure::Permanent(_)));
    assert!(
        failure
            .into_error("listing")
            .to_string()
            .contains("repeated")
    );
}

#[tokio::test]
async fn rate_limited_reads_and_listings_retry() {
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let lock = take(&store, None).await;
    store
        .put(&tenant(), &lock, &record("pet", "value"))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);

    let rate_limited = || FaultAction::StatusWithHeaders {
        status: 429,
        headers: vec![("retry-after".into(), "0".into())],
        body: "<Error><Code>SlowDown</Code></Error>".into(),
    };
    let state_key = path("cuenv", &tenant()).state_key();
    server.fault_exact("GET", &state_key, rate_limited());
    let request_start = server.requests().len();
    assert_eq!(store.list(&tenant()).await.unwrap().len(), 1);
    assert!(
        server.requests()[request_start..]
            .iter()
            .filter(|request| request.method == "GET" && request.key == state_key)
            .count()
            >= 2
    );

    server.fault_exact("GET", "", rate_limited());
    let request_start = server.requests().len();
    assert_eq!(
        store
            .legacy_project_inventory(&tenant())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        server.requests()[request_start..]
            .iter()
            .filter(|request| request.method == "GET" && request.key.is_empty())
            .count()
            >= 2
    );
}

#[tokio::test]
async fn history_snapshot_warning_does_not_log_provider_error_fields() {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("log capture is poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct RestoreSender(Option<cuenv_events::EventSender>);
    impl Drop for RestoreSender {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                cuenv_events::set_global_sender(sender);
            } else {
                cuenv_events::clear_global_sender();
            }
        }
    }

    const STATE_SENTINEL: &str = "private-history-state-sentinel";
    let server = FakeS3::start().await.unwrap();
    let store = store(&server);
    let tenant = TenantKey::try_new("example.com/history-redaction", "web").unwrap();
    let lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant,
            &LockRequest {
                lock: &lock,
                holder: "test",
                instance: None,
            },
        )
        .await
        .unwrap();
    store
        .put(&tenant, &lock, &record("pet", STATE_SENTINEL))
        .await
        .unwrap();
    let provider_error = format!(
        "<Error><Code>{STATE_SENTINEL}</Code><Message>{}</Message><Region>{STATE_SENTINEL}</Region></Error>",
        lock.lock_identifier
    );
    server.fault(
        "PUT",
        "/history/",
        FaultAction::Status(403, provider_error.clone()),
    );
    let tenant_path = path("cuenv", &tenant);
    let document = super::document::StateDocument::new(&tenant_path, 99);
    let diagnostic = store
        .snapshot_and_prune(&tenant_path, &document)
        .await
        .unwrap_err()
        .to_string();
    for secret in [STATE_SENTINEL, lock.lock_identifier.as_str(), SECRET] {
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
    server.fault("PUT", "/history/", FaultAction::Status(403, provider_error));

    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = LogWriter(Arc::clone(&output));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    let _tracing = tracing::subscriber::set_default(subscriber);
    let events = cuenv_events::EventBus::new();
    let mut receiver = events.subscribe();
    let _restore = RestoreSender(cuenv_events::global_sender());
    cuenv_events::set_global_sender(events.sender().unwrap());

    assert!(store.unlock(&tenant, &lock).await.unwrap());
    let warning = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.expect("event bus closed");
            if let cuenv_events::EventCategory::Output(cuenv_events::OutputEvent::Stderr {
                content,
            }) = event.category
                && content.contains("history-redaction")
            {
                break content;
            }
        }
    })
    .await
    .expect("history warning was not emitted");
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("could not write or prune a history snapshot"));
    assert!(warning.contains("could not write or prune the history snapshot"));
    for secret in [STATE_SENTINEL, lock.lock_identifier.as_str(), SECRET] {
        assert!(!logs.contains(secret), "{logs}");
        assert!(!warning.contains(secret), "{warning}");
    }
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

async fn rename_fixture(
    server: &FakeS3,
) -> Result<(ObjectStateStore, ObjectTenant, ObjectTenant), Box<dyn std::error::Error>> {
    let store = ObjectStateStore::try_new(configuration(server, "cuenv"))?;
    let writer = ProjectInstance::try_new("project", "cuenv")?;
    let lock = StateLock::generate();
    store
        .acquire_lock(
            &tenant(),
            &LockRequest {
                lock: &lock,
                holder: "seed",
                instance: Some(&writer),
            },
        )
        .await?;
    store.put(&tenant(), &lock, &record("pet", "value")).await?;
    assert!(store.unlock(&tenant(), &lock).await?);
    Ok((
        store,
        ObjectTenant {
            package: "cuenv".into(),
            tenant: tenant(),
        },
        ObjectTenant {
            package: "cuenv".into(),
            tenant: TenantKey::try_new("example.com/infra", "renamed")?,
        },
    ))
}

#[tokio::test]
async fn rename_refuses_missing_source_without_creating_state_or_markers()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let source = ObjectTenant {
        package: "cuenv".into(),
        tenant: tenant(),
    };
    let target = ObjectTenant {
        package: "cuenv".into(),
        tenant: TenantKey::try_new("example.com/infra", "renamed")?,
    };
    let before = server.keys(BUCKET);
    let error = store.rename(&source, &target, "rename").await.unwrap_err();
    assert!(error.to_string().contains("source has no state"));
    assert_eq!(server.keys(BUCKET), before);
    Ok(())
}

#[tokio::test]
async fn rename_never_recreates_a_source_deleted_after_preflight()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut reading = server.pause_next("GET", &source_key, BarrierPosition::AfterApply);
    let worker = tokio::spawn(async move { store.rename(&source, &target, "rename").await });
    reading.reached(Duration::from_secs(10)).await?;
    server.remove_object(BUCKET, &source_key);
    reading.release(Duration::from_secs(10)).await?;
    assert!(worker.await?.is_err());
    assert!(server.object(BUCKET, &source_key).is_none());
    assert!(server.object(BUCKET, &target_key).is_none());
    Ok(())
}

#[tokio::test]
async fn rename_refuses_unrepresentable_metadata_before_locking()
-> Result<(), Box<dyn std::error::Error>> {
    for field in ["document", "format", "tenant", "origin", "unknown_origin"] {
        let server = FakeS3::start().await?;
        let (store, source, target) = rename_fixture(&server).await?;
        let source_key = path("cuenv", &source.tenant).state_key();
        let mut original = document(&server, &source_key);
        match field {
            "document" => original["futureDocument"] = json!({"kept": true}),
            "format" => original["format"]["futureFormat"] = json!({"kept": true}),
            "tenant" => original["tenant"]["futureTenant"] = json!({"kept": true}),
            "origin" => {
                original["origin"] =
                    json!({"kind": "rename", "id": "old", "source": "prior", "future": true});
            }
            "unknown_origin" => {
                original["origin"] = json!({"kind": "future", "id": "old", "source": "prior"});
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&original)?;
        server.put_object(BUCKET, &source_key, bytes.clone());
        let keys = server.keys(BUCKET);
        let error = store.rename(&source, &target, "rename").await.unwrap_err();
        assert!(error.to_string().contains("metadata"), "{field}: {error}");
        assert_eq!(server.object(BUCKET, &source_key), Some(bytes));
        assert_eq!(server.keys(BUCKET), keys);
    }
    Ok(())
}

#[tokio::test]
async fn rename_refuses_a_missing_target_bound_by_a_migration_manifest()
-> Result<(), Box<dyn std::error::Error>> {
    for status in ["pending", "migrated", "rolled-back"] {
        let server = FakeS3::start().await?;
        let (store, source, target) = rename_fixture(&server).await?;
        let target_path = path("cuenv", &target.tenant);
        let manifest = super::MigrationManifest {
            tenants: vec![super::ManifestEntry {
                environment: None,
                status: status.into(),
                migration: "op".into(),
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            }],
            revision: None,
            write_nonce: None,
            serial: None,
        };
        server.put_object(
            BUCKET,
            &target_path.migration_manifest_key(),
            serde_json::to_vec(&manifest)?,
        );
        let before = server.keys(BUCKET);
        let source_key = path("cuenv", &source.tenant).state_key();
        let source_before = server.object(BUCKET, &source_key);
        assert!(
            store.rename(&source, &target, "rename").await.is_err(),
            "{status}"
        );
        assert_eq!(server.object(BUCKET, &source_key), source_before);
        assert_eq!(server.keys(BUCKET), before);
        assert!(server.object(BUCKET, &target_path.state_key()).is_none());
    }
    Ok(())
}

#[tokio::test]
async fn rename_copies_raw_record_and_writer_extensions_and_keeps_origin_history()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut original = document(&server, &source_key);
    original["records"]["random_pet.pet"]["futureRecord"] = json!({"kept": true});
    original["records"]["random_pet.pet"]["address"]["futureAddress"] = json!({"kept": true});
    original["lastWriter"]["futureWriter"] = json!({"kept": true});
    original["origin"] = json!({"kind": "rename", "id": "old", "source": "prior"});
    server.put_object(BUCKET, &source_key, serde_json::to_vec(&original)?);
    store.rename(&source, &target, "rename").await?;
    let moved = document(&server, &target_key);
    let tombstone = document(&server, &source_key);
    assert_eq!(moved["records"], original["records"]);
    assert_eq!(moved["lastWriter"], original["lastWriter"]);
    assert_eq!(tombstone["origin"], original["origin"]);
    assert_eq!(moved["origin"]["kind"], "rename");
    assert!(moved["lock"].is_null());
    Ok(())
}

#[tokio::test]
async fn rename_keeps_both_fences_when_source_tombstone_is_not_confirmed()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut copied = server.pause_next("PUT", &target_key, BarrierPosition::AfterApply);
    let worker = tokio::spawn(async move { store.rename(&source, &target, "rename").await });
    copied.reached(Duration::from_secs(10)).await?;
    server.fault_exact(
        "PUT",
        &source_key,
        FaultAction::Status(503, "unavailable".into()),
    );
    copied.release(Duration::from_secs(10)).await?;
    let error = worker.await?.unwrap_err();
    assert!(error.to_string().contains("reconcile them manually"));
    let source_state = document(&server, &source_key);
    let target_state = document(&server, &target_key);
    assert!(source_state["tombstone"].is_null());
    assert!(!source_state["lock"].is_null());
    assert!(!target_state["lock"].is_null());
    assert_eq!(source_state["records"], target_state["records"]);
    Ok(())
}

#[tokio::test]
async fn rename_keeps_target_fenced_when_source_release_is_lost()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut acquired = server.pause_next("PUT", &source_key, BarrierPosition::AfterApply);
    let mut tombstoned = server.pause_next("PUT", &source_key, BarrierPosition::AfterApply);
    let mut releasing = server.pause_next("PUT", &source_key, BarrierPosition::BeforeApply);
    let worker = tokio::spawn(async move { store.rename(&source, &target, "rename").await });
    acquired.reached(Duration::from_secs(10)).await?;
    acquired.release(Duration::from_secs(10)).await?;
    tombstoned.reached(Duration::from_secs(10)).await?;
    tombstoned.release(Duration::from_secs(10)).await?;
    releasing.reached(Duration::from_secs(10)).await?;
    let mut source_state = document(&server, &source_key);
    source_state["lock"] = serde_json::Value::Null;
    server.put_object(BUCKET, &source_key, serde_json::to_vec(&source_state)?);
    releasing.release(Duration::from_secs(10)).await?;
    let error = worker.await?.unwrap_err();
    assert!(error.to_string().contains("reconcile them manually"));
    assert!(!document(&server, &source_key)["tombstone"].is_null());
    assert!(!document(&server, &target_key)["lock"].is_null());
    Ok(())
}

#[tokio::test]
async fn rename_reports_a_lost_target_release_after_tombstoning_source()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut copied = server.pause_next("PUT", &target_key, BarrierPosition::AfterApply);
    let mut releasing = server.pause_next("PUT", &target_key, BarrierPosition::BeforeApply);
    let worker = tokio::spawn(async move { store.rename(&source, &target, "rename").await });
    copied.reached(Duration::from_secs(10)).await?;
    copied.release(Duration::from_secs(10)).await?;
    releasing.reached(Duration::from_secs(10)).await?;
    let mut target_state = document(&server, &target_key);
    target_state["lock"] = serde_json::Value::Null;
    server.put_object(BUCKET, &target_key, serde_json::to_vec(&target_state)?);
    releasing.release(Duration::from_secs(10)).await?;
    let error = worker.await?.unwrap_err();
    assert!(error.to_string().contains("reconcile them manually"));
    assert!(!document(&server, &source_key)["tombstone"].is_null());
    assert!(document(&server, &target_key)["lock"].is_null());
    Ok(())
}

#[tokio::test]
async fn rename_confirms_a_lost_tombstone_response_by_readback()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_key = path("cuenv", &target.tenant).state_key();
    let mut copied = server.pause_next("PUT", &target_key, BarrierPosition::AfterApply);
    let worker = tokio::spawn(async move { store.rename(&source, &target, "rename").await });
    copied.reached(Duration::from_secs(10)).await?;
    server.fault_exact("PUT", &source_key, FaultAction::DropAfterApply);
    copied.release(Duration::from_secs(10)).await?;
    worker.await??;
    assert!(!document(&server, &source_key)["tombstone"].is_null());
    assert!(document(&server, &target_key)["lock"].is_null());
    assert!(
        document(&server, &target_key)["records"]
            .as_object()
            .is_some_and(|records| !records.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn malformed_history_never_looks_like_an_unused_tenant()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "STATE_PRIVATE_HISTORY_KEY_SENTINEL";
    for suffix in [
        "00000000000000000000.json",
        "18446744073709551616.json",
        "42.json",
        "STATE_PRIVATE_HISTORY_KEY_SENTINEL.json",
    ] {
        let server = FakeS3::start().await?;
        let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
        let tenant_path = path("cuenv", &tenant());
        let key = format!("{}{suffix}", tenant_path.history_prefix());
        server.put_object(BUCKET, &key, b"{}".to_vec());
        let before = server.keys(BUCKET);
        let lock = StateLock::generate();
        let error = store
            .acquire_lock(
                &tenant(),
                &LockRequest {
                    lock: &lock,
                    holder: "new state",
                    instance: None,
                },
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains(SECRET));
        assert!(error.to_string().contains("invalid snapshot key"));
        assert_eq!(server.keys(BUCKET), before);
        assert!(server.object(BUCKET, &tenant_path.state_key()).is_none());
        assert!(store.history(&tenant()).await.is_err());
    }
    Ok(())
}

#[tokio::test]
async fn malformed_target_history_blocks_rename_and_import_before_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, target) = rename_fixture(&server).await?;
    let source_key = path("cuenv", &source.tenant).state_key();
    let target_path = path("cuenv", &target.tenant);
    let key = format!("{}invalid.json", target_path.history_prefix());
    server.put_object(BUCKET, &key, b"{}".to_vec());
    let before = server.keys(BUCKET);
    let source_before = server.object(BUCKET, &source_key);
    assert!(store.rename(&source, &target, "rename").await.is_err());
    assert_eq!(server.keys(BUCKET), before);
    assert_eq!(server.object(BUCKET, &source_key), source_before);
    let lock = StateLock::generate();
    let records = [record("pet", "value")];
    let error = store
        .import(
            &target.tenant,
            &super::TenantImport {
                lock: &lock,
                holder: "migration",
                resources: &records,
                last_writer: None,
                migration: "migration-id",
                source: "source",
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid snapshot key"));
    assert_eq!(server.keys(BUCKET), before);
    assert!(server.object(BUCKET, &target_path.state_key()).is_none());
    Ok(())
}

#[tokio::test]
async fn missing_state_in_any_manifest_status_is_never_recreated()
-> Result<(), Box<dyn std::error::Error>> {
    for status in ["pending", "migrated", "rolled-back"] {
        let server = FakeS3::start().await?;
        let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
        let tenant_path = path("cuenv", &tenant());
        let manifest = super::MigrationManifest {
            tenants: vec![super::ManifestEntry {
                environment: None,
                status: status.into(),
                migration: "migration-id".into(),
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            }],
            revision: None,
            write_nonce: None,
            serial: None,
        };
        server.put_object(
            BUCKET,
            &tenant_path.migration_manifest_key(),
            serde_json::to_vec(&manifest)?,
        );
        let before = server.keys(BUCKET);
        let lock = StateLock::generate();
        assert!(
            store
                .acquire_lock(
                    &tenant(),
                    &LockRequest {
                        lock: &lock,
                        holder: "new state",
                        instance: None,
                    }
                )
                .await
                .is_err(),
            "{status}"
        );
        assert_eq!(server.keys(BUCKET), before);
        assert!(server.object(BUCKET, &tenant_path.state_key()).is_none());
    }
    Ok(())
}

#[tokio::test]
async fn lock_enumeration_refuses_malformed_markers_without_disclosing_them()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "PRIVATE_LOCK_MARKER_SENTINEL";
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let key = format!("_locks/example.com/infra/_cuenv/web/_default/{SECRET} bad.json");
    server.put_object(BUCKET, &key, b"{}".to_vec());
    for result in [
        store.locks().await,
        store.module_locks("example.com/infra").await,
    ] {
        let error = result.unwrap_err();
        assert!(error.to_string().contains("invalid marker"));
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    Ok(())
}

#[tokio::test]
async fn lock_enumeration_refuses_unreadable_documents_instead_of_hiding_locks()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "PRIVATE_LOCK_RESPONSE_SENTINEL";
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    let _lock = take(&store, None).await;
    assert_eq!(store.locks().await?.len(), 1);
    server.fault_exact(
        "GET",
        &path("cuenv", &tenant()).state_key(),
        FaultAction::Status(403, SECRET.into()),
    );
    let error = store.locks().await.unwrap_err();
    assert!(error.to_string().contains("cannot inspect"));
    assert!(!format!("{error:?} {error}").contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn tenant_enumeration_rejects_malformed_state_keys_but_ignores_unrelated_objects()
-> Result<(), Box<dyn std::error::Error>> {
    const SECRET: &str = "PRIVATE_STATE_KEY_SENTINEL";
    let server = FakeS3::start().await?;
    let store = ObjectStateStore::try_new(configuration(&server, "cuenv"))?;
    server.put_object(BUCKET, "example.com/infra/_cuenv/unrelated.txt", Vec::new());
    assert!(store.tenants("example.com/infra").await?.is_empty());
    let invalid = format!("example.com/infra/_cuenv/{SECRET} invalid/_default/state.json");
    server.put_object(BUCKET, &invalid, b"{}".to_vec());
    let error = store.tenants("example.com/infra").await.unwrap_err();
    assert!(error.to_string().contains("invalid state key"));
    assert!(!format!("{error:?} {error}").contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn tenant_enumeration_refuses_state_that_vanishes_after_the_listing()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeS3::start().await?;
    let (store, source, _) = rename_fixture(&server).await?;
    let key = path("cuenv", &source.tenant).state_key();
    let mut reading = server.pause_next("GET", &key, BarrierPosition::BeforeApply);
    let worker = tokio::spawn(async move { store.tenants("example.com/infra").await });
    reading.reached(Duration::from_secs(10)).await?;
    server.remove_object(BUCKET, &key);
    reading.release(Duration::from_secs(10)).await?;
    let error = worker.await?.unwrap_err();
    assert!(error.to_string().contains("vanished"));
    Ok(())
}
