//! The S3 state commands and checks, over the fake S3 server.

use std::sync::Arc;

use cuenv_infrastructure::fake::FakeS3;
use cuenv_infrastructure::{
    AddressingChoice, Credentials, LockRequest, ManagedResource, ManifestEntry, MigrationManifest,
    ObjectStateStore, ObjectStoreConfiguration, ProjectInstance, ResourceAddress, StateLock,
    StateStore, TenantKey, UnrecordedStore,
};

use super::super::{
    ConfirmationPolicy, FenceRelease, Identity, InfrastructureAction, MigrateRequest,
    MigrationDirection, MigrationScope, RenameSource, SeparateState, SourceEnvironment,
    SourceUpgrade, StateAction, UnlockScope,
};
use super::{Harness, Script, managed};
use crate::cli::OutputFormat;

const MODULE: &str = "example.com/infrastructure";

fn configuration(server: &FakeS3, package: &str) -> ObjectStoreConfiguration {
    ObjectStoreConfiguration {
        endpoint: Some(server.endpoint.clone()),
        bucket: "state".into(),
        region: "us-east-1".into(),
        root: None,
        addressing: AddressingChoice::Auto,
        credentials: Credentials {
            access_key_id: "access".into(),
            secret_access_key: "secret".into(),
            session_token: None,
        },
        package: package.into(),
        history: 5,
        minimum_write_interval: None,
        probe_tenant: None,
    }
}

fn object_store(server: &FakeS3, package: &str) -> Arc<ObjectStateStore> {
    Arc::new(ObjectStateStore::try_new(configuration(server, package)).unwrap())
}

/// A JSON harness over the object store of package `cuenv`.
fn harness(server: &FakeS3) -> Harness {
    Harness::with_store(
        OutputFormat::Json,
        Script::Line("yes"),
        object_store(server, "cuenv"),
    )
}

/// Record `resources` for `tenant` as a run from `writer` would.
async fn seed(
    store: &dyn StateStore,
    tenant: &TenantKey,
    writer: &ProjectInstance,
    resources: &[&str],
) {
    let lock = StateLock::generate();
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &lock,
                holder: "seed",
                instance: Some(writer),
            },
        )
        .await
        .unwrap();
    for address in resources {
        store.put(tenant, &lock, &managed(address)).await.unwrap();
    }
    store.unlock(tenant, &lock).await.unwrap();
}

fn apply(identity: Identity) -> InfrastructureAction {
    InfrastructureAction::Apply {
        confirmation: ConfirmationPolicy::AssumeYes,
        separate_state: SeparateState::Refuse,
        identity,
    }
}

#[tokio::test]
async fn a_renamed_project_is_refused_until_its_state_is_moved() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    let old = TenantKey::try_new(MODULE, "old").unwrap();
    seed(
        harness.store.as_ref(),
        &old,
        &harness.instance,
        &["random_pet.pet"],
    )
    .await;

    // `plan` reports and carries on.
    harness.run(InfrastructureAction::Plan).await.unwrap();
    let error = harness.run(apply(Identity::Detect)).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("refusing to start"), "{message}");
    assert!(message.contains("project 'old'"), "{message}");
    let help = error.help().unwrap();
    assert!(
        help.contains("cuenv infrastructure state rename --from-project old"),
        "{help}"
    );
    assert!(help.contains("--new-identity"), "{help}");

    harness
        .run(InfrastructureAction::State(StateAction::Rename(
            RenameSource {
                project: Some("old".into()),
                package: None,
                environment: SourceEnvironment::Selected,
            },
        )))
        .await
        .unwrap();
    assert_eq!(harness.store.list(&harness.tenant).await.unwrap().len(), 1);
    let moved = harness.store.list(&old).await.unwrap_err().to_string();
    assert!(moved.contains("moved to"), "{moved}");
    // The tenant exists now, so nothing is detected: the run gets as far as
    // planning the configuration (which here no longer declares the pet).
    let error = harness.run(apply(Identity::Detect)).await.unwrap_err();
    assert!(!error.to_string().contains("refusing to start"), "{error}");
}

#[tokio::test]
async fn new_identity_starts_empty_on_purpose() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    let old = TenantKey::try_new(MODULE, "old").unwrap();
    seed(
        harness.store.as_ref(),
        &old,
        &harness.instance,
        &["random_pet.pet"],
    )
    .await;
    harness.run(apply(Identity::New)).await.unwrap();
    // Once the tenant exists, detection no longer runs.
    harness.run(apply(Identity::Detect)).await.unwrap();
    assert_eq!(harness.store.list(&old).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_renamed_package_is_detected_and_other_directories_are_not() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    // The package `ops` no longer exists in this directory.
    let ops = object_store(&server, "ops");
    seed(
        ops.as_ref(),
        &harness.tenant,
        &ProjectInstance::try_new("app", "ops").unwrap(),
        &["random_pet.pet"],
    )
    .await;
    // State written from another directory is not this project's.
    let elsewhere = TenantKey::try_new(MODULE, "other").unwrap();
    seed(
        harness.store.as_ref(),
        &elsewhere,
        &ProjectInstance::try_new("services/other", "cuenv").unwrap(),
        &["random_pet.pet"],
    )
    .await;
    let error = harness.run(apply(Identity::Detect)).await.unwrap_err();
    let help = error.help().unwrap();
    assert!(help.contains("--from-package ops"), "{help}");
    assert!(!help.contains("--from-project"), "{help}");
}

#[tokio::test]
async fn an_emptied_or_never_applied_identity_is_not_a_rename() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    let old = TenantKey::try_new(MODULE, "old").unwrap();
    seed(harness.store.as_ref(), &old, &harness.instance, &[]).await;
    harness.run(apply(Identity::Detect)).await.unwrap();
}

#[tokio::test]
async fn restore_writes_a_snapshot_back_and_refuses_pending_changes() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &harness.instance,
        &["random_pet.pet"],
    )
    .await;
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &harness.instance,
        &["random_pet.cat"],
    )
    .await;
    let restore = |serial| {
        InfrastructureAction::State(StateAction::Restore {
            serial,
            confirmation: ConfirmationPolicy::AssumeYes,
        })
    };
    let first = harness
        .store
        .object_features()
        .unwrap()
        .history(&harness.tenant)
        .await
        .unwrap()[0]
        .serial;

    // A change saved on this machine was saved against the current state.
    let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
    let current = harness.store.list(&harness.tenant).await.unwrap();
    unrecorded
        .save(
            &harness.tenant,
            &cuenv_infrastructure::ConditionalPut {
                resource: &current[0],
                expected: cuenv_infrastructure::RecordVersion::of(Some(&current[0])),
            },
        )
        .unwrap();
    let error = harness.run(restore(Some(first))).await.unwrap_err();
    assert!(error.to_string().contains("unrecorded changes"), "{error}");
    for pending in unrecorded.list(&harness.tenant).unwrap() {
        std::fs::remove_file(&pending.file).unwrap();
    }

    let missing = harness.run(restore(Some(9_999))).await.unwrap_err();
    assert!(
        missing.help().unwrap().contains("Snapshots kept"),
        "{missing:?}"
    );
    harness.run(restore(Some(first))).await.unwrap();
    let result = harness.output.take_result().unwrap();
    assert_eq!(result["restoredSerial"], first);
    let restored: Vec<String> = harness
        .store
        .list(&harness.tenant)
        .await
        .unwrap()
        .iter()
        .map(|record| record.address.to_string())
        .collect();
    assert_eq!(restored, ["random_pet.pet"]);
}

#[tokio::test]
async fn restore_and_rename_need_s3_state() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let error = harness
        .run(InfrastructureAction::State(StateAction::Restore {
            serial: None,
            confirmation: ConfirmationPolicy::AssumeYes,
        }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("needs S3 state"), "{error}");
}

#[tokio::test]
async fn pull_prints_records_writer_and_location() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &harness.instance,
        &["random_pet.pet"],
    )
    .await;
    harness
        .run(InfrastructureAction::State(StateAction::Pull))
        .await
        .unwrap();
    let result = harness.result();
    assert_eq!(
        result["stateLocation"],
        "s3://state/example.com/infrastructure/_cuenv/app/_default/state.json"
    );
    assert_eq!(result["lastWriter"]["instance"], "app:cuenv");
    assert_eq!(result["records"][0]["address"]["name"], "pet");
}

#[tokio::test]
async fn a_tenant_awaiting_migration_is_refused() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    object_store(&server, "cuenv")
        .write_manifest(
            &harness.tenant,
            &MigrationManifest {
                tenants: vec![ManifestEntry {
                    environment: None,
                    status: "pending".into(),
                    migration: "m1".into(),
                    committed_serial: None,
                }],
            },
        )
        .await
        .unwrap();
    let error = harness.run(InfrastructureAction::Plan).await.unwrap_err();
    assert!(error.to_string().contains("not been migrated"), "{error}");
    assert!(error.help().unwrap().contains("state migrate"));
}

/// Engine inputs over the harness's memory store that migrate to `server`.
fn migrating_inputs(harness: &Harness, server: &FakeS3) -> super::EngineInputs {
    let mut inputs = harness.inputs();
    inputs.infrastructure.migrate_state_to = Some(
        serde_json::from_value(serde_json::json!({"s3": {
            "bucket": "state",
            "region": "us-east-1",
            "endpoint": server.endpoint,
            "accessKeyId": "access",
            "secretAccessKey": "secret",
        }}))
        .unwrap(),
    );
    inputs
}

/// Run `action` with the inputs that migrate to `server`.
async fn run_migrating(
    harness: &Harness,
    server: &FakeS3,
    action: InfrastructureAction,
) -> Result<(), crate::cli::CliError> {
    super::dispatch(
        &action,
        &harness.context(),
        migrating_inputs(harness, server),
    )
    .await
}

fn migrate(direction: MigrationDirection) -> InfrastructureAction {
    InfrastructureAction::State(StateAction::Migrate(MigrateRequest {
        scope: MigrationScope::All,
        direction,
        source_upgrade: SourceUpgrade::Refuse,
    }))
}

#[tokio::test]
async fn migration_copies_every_environment_and_fences_the_source() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let prod = TenantKey::with_environment(MODULE, "app", "Prod").unwrap();
    let writer = ProjectInstance::try_new("deploy/app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &writer,
        &["random_pet.pet", "random_pet.cat"],
    )
    .await;
    seed(harness.store.as_ref(), &prod, &writer, &["random_pet.fox"]).await;

    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    // Running it again continues the same migration and changes nothing.
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();

    let target = object_store(&server, "cuenv");
    for (tenant, count) in [(&harness.tenant, 2), (&prod, 1)] {
        let copied = target.list(tenant).await.unwrap();
        assert_eq!(copied.len(), count);
        let source = harness.store.list(tenant).await.unwrap();
        assert!(
            copied
                .iter()
                .all(|record| source.iter().any(|kept| kept.same_content(record)))
        );
        assert_eq!(
            target.last_writer(tenant).await.unwrap().unwrap().instance,
            writer,
            "the source's last writer carries over"
        );
        assert_eq!(target.current_lock(tenant).await.unwrap(), None);
        let fence = harness.store.current_lock(tenant).await.unwrap().unwrap();
        assert!(fence.lock_identifier.starts_with("migrated-"));
        assert!(fence.holder.contains("--rollback"), "{}", fence.holder);
    }
    let manifest = target.manifest(&harness.tenant).await.unwrap().unwrap();
    assert!(
        manifest
            .tenants
            .iter()
            .all(|entry| entry.status == "migrated")
    );
    assert_eq!(manifest.tenants.len(), 2);

    // The fence is not released by accident.
    let fence = harness
        .store
        .current_lock(&harness.tenant)
        .await
        .unwrap()
        .unwrap();
    let unlock = |fence_release| InfrastructureAction::Unlock {
        lock_identifier: Some(fence.lock_identifier.clone()),
        scope: UnlockScope::default(),
        fence: fence_release,
    };
    let error = harness.run(unlock(FenceRelease::Refuse)).await.unwrap_err();
    assert!(error.to_string().contains("migration fence"), "{error}");
    assert!(harness.current_lock().await.is_some());

    // Rolling back releases the fences and retires the copies.
    run_migrating(&harness, &server, migrate(MigrationDirection::Rollback))
        .await
        .unwrap();
    for tenant in [&harness.tenant, &prod] {
        assert_eq!(harness.store.current_lock(tenant).await.unwrap(), None);
        let retired = target.list(tenant).await.unwrap_err().to_string();
        assert!(retired.contains("moved to"), "{retired}");
    }
}

#[tokio::test]
async fn a_rollback_never_loses_changes_made_in_the_target() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &writer,
        &["random_pet.pet", "random_pet.cat"],
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();

    // A run against the new backend changes the state.
    let target = object_store(&server, "cuenv");
    let lock = target.lock(&harness.tenant, "apply").await.unwrap();
    target
        .put(&harness.tenant, &lock, &managed("random_pet.owl"))
        .await
        .unwrap();
    target
        .delete(
            &harness.tenant,
            &lock,
            &ResourceAddress::new("random_pet", "cat"),
        )
        .await
        .unwrap();
    target.unlock(&harness.tenant, &lock).await.unwrap();

    let error = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("changed in the target"),
        "{error}"
    );
    assert!(error.help().unwrap().contains("--copy-back"));
    assert!(harness.current_lock().await.is_some(), "still fenced");

    run_migrating(&harness, &server, migrate(MigrationDirection::CopyBack))
        .await
        .unwrap();
    let mut addresses: Vec<String> = harness
        .store
        .list(&harness.tenant)
        .await
        .unwrap()
        .iter()
        .map(|record: &ManagedResource| record.address.to_string())
        .collect();
    addresses.sort();
    assert_eq!(addresses, ["random_pet.owl", "random_pet.pet"]);
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn migration_refuses_a_target_that_already_holds_state() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        &writer,
        &["random_pet.pet"],
    )
    .await;
    seed(
        object_store(&server, "cuenv").as_ref(),
        &harness.tenant,
        &writer,
        &["random_pet.cat"],
    )
    .await;
    let error = super::dispatch(
        &migrate(MigrationDirection::Forward),
        &harness.context(),
        migrating_inputs(&harness, &server),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("refusing to overwrite"),
        "{error}"
    );
    assert!(
        harness.current_lock().await.is_none(),
        "the source is not fenced"
    );
}
