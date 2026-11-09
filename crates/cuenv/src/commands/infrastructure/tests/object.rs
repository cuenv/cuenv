//! The S3 state commands and checks, over the fake S3 server.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use cuenv_infrastructure::fake::{BarrierPosition, FakeS3, FaultAction};
use cuenv_infrastructure::{
    AddressingChoice, Credentials, LockRequest, ManagedResource, ManifestEntry, MigrationManifest,
    ObjectStateStore, ObjectStoreConfiguration, ProjectInstance, ResourceAddress, StateLock,
    StateStore, TenantKey, UnrecordedStore,
};

use super::super::{
    ConfirmationPolicy, FenceRelease, Identity, InfrastructureAction, MigrateRequest,
    MigrationDirection, MigrationMaintenance, MigrationPhysicalIdentities, MigrationScope,
    RenameSource, SeparateState, SourceEnvironment, SourceUpgrade, StateAction, UnlockScope,
};
use super::{Harness, Script};
use crate::cli::OutputFormat;

const MODULE: &str = "example.com/infrastructure";

fn seed_manifest(server: &FakeS3, tenant: &TenantKey, manifest: &MigrationManifest) {
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(None, "cuenv", tenant)
        .unwrap();
    server.put_object(
        "state",
        &path.migration_manifest_key(),
        serde_json::to_vec(manifest).unwrap(),
    );
}

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

#[derive(Clone, Copy)]
struct SeedResources<'resources> {
    writer: &'resources ProjectInstance,
    resources: &'resources [&'resources str],
}

/// Record `resources` for `tenant` as a run from `writer` would.
async fn seed(store: &dyn StateStore, tenant: &TenantKey, input: SeedResources<'_>) {
    let lock = StateLock::generate();
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &lock,
                holder: "seed",
                instance: Some(input.writer),
            },
        )
        .await
        .unwrap();
    for address in input.resources {
        store
            .put(tenant, &lock, &super::managed(address))
            .await
            .unwrap();
    }
    store.unlock(tenant, &lock).await.unwrap();
}

fn apply(identity: Identity) -> InfrastructureAction {
    InfrastructureAction::Apply {
        confirmation: ConfirmationPolicy::AssumeYes,
        expected_plan: None,
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
        SeedResources {
            writer: &harness.instance,
            resources: &["random_pet.pet"],
        },
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
async fn rename_refuses_pending_or_unreadable_recovery_files_before_state_mutation() {
    for pending_on_source in [true, false] {
        let server = FakeS3::start().await.unwrap();
        let harness = harness(&server);
        let old = TenantKey::try_new(MODULE, "old").unwrap();
        seed(
            harness.store.as_ref(),
            &old,
            SeedResources {
                writer: &harness.instance,
                resources: &["random_pet.pet"],
            },
        )
        .await;
        let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
        let pending_tenant = if pending_on_source {
            &old
        } else {
            &harness.tenant
        };
        let file = unrecorded
            .save(
                pending_tenant,
                &cuenv_infrastructure::ConditionalPut {
                    resource: &super::managed("random_pet.pet"),
                    expected: cuenv_infrastructure::RecordVersion::Absent,
                },
            )
            .unwrap();
        let before_keys = server.keys("state");
        let source_key =
            cuenv_infrastructure::state::object::key::TenantPath::try_new(None, "cuenv", &old)
                .unwrap()
                .state_key();
        let before_source = server.object("state", &source_key);
        let error = harness
            .run(InfrastructureAction::State(StateAction::Rename(
                RenameSource {
                    project: Some("old".into()),
                    package: None,
                    environment: SourceEnvironment::Selected,
                },
            )))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unrecorded changes"), "{error}");
        assert!(file.exists());
        assert_eq!(server.keys("state"), before_keys);
        assert_eq!(server.object("state", &source_key), before_source);
    }

    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    std::fs::write(
        harness.directory.path().join("unrecorded"),
        b"not a directory",
    )
    .unwrap();
    let error = harness
        .run(InfrastructureAction::State(StateAction::Rename(
            RenameSource {
                project: Some("old".into()),
                package: None,
                environment: SourceEnvironment::Selected,
            },
        )))
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    assert!(server.keys("state").is_empty());
}

#[tokio::test]
async fn new_identity_starts_empty_on_purpose() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    let old = TenantKey::try_new(MODULE, "old").unwrap();
    seed(
        harness.store.as_ref(),
        &old,
        SeedResources {
            writer: &harness.instance,
            resources: &["random_pet.pet"],
        },
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
        SeedResources {
            writer: &ProjectInstance::try_new("app", "ops").unwrap(),
            resources: &["random_pet.pet"],
        },
    )
    .await;
    // State written from another directory is not this project's.
    let elsewhere = TenantKey::try_new(MODULE, "other").unwrap();
    seed(
        harness.store.as_ref(),
        &elsewhere,
        SeedResources {
            writer: &ProjectInstance::try_new("services/other", "cuenv").unwrap(),
            resources: &["random_pet.pet"],
        },
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
    seed(
        harness.store.as_ref(),
        &old,
        SeedResources {
            writer: &harness.instance,
            resources: &[],
        },
    )
    .await;
    harness.run(apply(Identity::Detect)).await.unwrap();
}

#[tokio::test]
async fn restore_writes_a_snapshot_back_and_refuses_pending_changes() {
    let server = FakeS3::start().await.unwrap();
    let harness = harness(&server);
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &harness.instance,
            resources: &["random_pet.pet"],
        },
    )
    .await;
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &harness.instance,
            resources: &["random_pet.cat"],
        },
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
        SeedResources {
            writer: &harness.instance,
            resources: &["random_pet.pet"],
        },
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
    seed_manifest(
        &server,
        &harness.tenant,
        &MigrationManifest {
            tenants: vec![ManifestEntry {
                environment: None,
                status: "pending".into(),
                migration: "m1".into(),
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            }],
            ..MigrationManifest::default()
        },
    );
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
    super::super::dispatch::dispatch(
        &action,
        &harness.context(),
        migrating_inputs(harness, server),
    )
    .await
}

fn migrate(direction: MigrationDirection) -> InfrastructureAction {
    InfrastructureAction::State(StateAction::Migrate(MigrateRequest {
        maintenance: MigrationMaintenance::Confirmed,
        physical_identities: Some(MigrationPhysicalIdentities {
            source: uuid::Uuid::from_u128(1),
            target: uuid::Uuid::from_u128(2),
        }),
        scope: MigrationScope::All,
        direction,
        source_upgrade: SourceUpgrade::Refuse,
    }))
}

#[tokio::test]
async fn a_write_after_target_commit_is_not_recorded_as_the_migration_baseline() {
    use std::time::Duration;

    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;

    let target = object_store(&server, "cuenv");
    let mut imported = server.pause_next("PUT", "state.json", BarrierPosition::AfterApply);
    let racing_write = async {
        let request = imported.reached(Duration::from_secs(10)).await.unwrap();
        let mut committed = server.pause_next("PUT", &request.key, BarrierPosition::AfterApply);
        imported.release(Duration::from_secs(10)).await.unwrap();
        committed.reached(Duration::from_secs(10)).await.unwrap();
        seed(
            target.as_ref(),
            &harness.tenant,
            SeedResources {
                writer: &writer,
                resources: &["random_pet.concurrent"],
            },
        )
        .await;
        committed.release(Duration::from_secs(10)).await.unwrap();
    };
    let (forward, ()) = tokio::join!(
        run_migrating(&harness, &server, migrate(MigrationDirection::Forward)),
        racing_write,
    );
    forward.unwrap();

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)).await;
    assert!(
        result.is_err(),
        "rollback must reject the later target write"
    );
    assert!(
        target
            .list(&harness.tenant)
            .await
            .unwrap()
            .iter()
            .any(|record| { record.address.to_string() == "random_pet.concurrent" })
    );
    assert!(
        harness.current_lock().await.is_some(),
        "source remains fenced"
    );
}

#[tokio::test]
async fn rollback_detects_a_write_between_its_initial_read_and_lock() {
    use std::time::Duration;

    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();

    let target = object_store(&server, "cuenv");
    let mut read = server.pause_next("GET", "state.json", BarrierPosition::AfterApply);
    let racing_write = async {
        read.reached(Duration::from_secs(10)).await.unwrap();
        seed(
            target.as_ref(),
            &harness.tenant,
            SeedResources {
                writer: &writer,
                resources: &["random_pet.concurrent"],
            },
        )
        .await;
        read.release(Duration::from_secs(10)).await.unwrap();
    };
    let (rollback, ()) = tokio::join!(
        run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)),
        racing_write,
    );
    assert!(
        rollback.is_err(),
        "rollback must reject the concurrent target write"
    );
    assert!(
        target
            .list(&harness.tenant)
            .await
            .unwrap()
            .iter()
            .any(|record| { record.address.to_string() == "random_pet.concurrent" })
    );
    assert!(
        harness.current_lock().await.is_some(),
        "source remains fenced"
    );
}

#[tokio::test]
async fn rollback_refuses_unmarked_legacy_baselines_before_touching_the_target() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let target = object_store(&server, "cuenv");
    let mut manifest = target.manifest(&harness.tenant).await.unwrap().unwrap();
    manifest.tenants[0].commit_evidence = None;
    seed_manifest(&server, &harness.tenant, &manifest);

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)).await;
    assert!(result.is_err());
    assert_eq!(target.list(&harness.tenant).await.unwrap().len(), 1);
    assert!(harness.current_lock().await.is_some());
}

#[tokio::test]
async fn forward_refuses_an_unmarked_existing_target_before_fencing_the_source() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    let target = object_store(&server, "cuenv");
    let lock = StateLock::generate();
    let migration = "0123456789abcdef";
    assert!(
        target
            .import(
                &harness.tenant,
                &cuenv_infrastructure::TenantImport {
                    lock: &lock,
                    holder: "old migration",
                    resources: &[],
                    last_writer: None,
                    migration,
                    source: "old source",
                },
            )
            .await
            .unwrap()
    );
    assert!(target.unlock(&harness.tenant, &lock).await.unwrap());
    seed_manifest(
        &server,
        &harness.tenant,
        &MigrationManifest {
            tenants: vec![ManifestEntry {
                environment: None,
                status: "pending".into(),
                migration: migration.into(),
                committed_serial: None,
                commit_evidence: None,
                binding: None,
            }],
            ..MigrationManifest::default()
        },
    );

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Forward)).await;
    assert!(result.is_err());
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn rollback_refuses_a_missing_target_without_releasing_the_source_fence() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(
        None,
        "cuenv",
        &harness.tenant,
    )
    .unwrap();
    server.fault(
        "GET",
        &path.state_key(),
        FaultAction::Status(404, String::new()),
    );

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)).await;
    assert!(result.is_err());
    assert!(harness.current_lock().await.is_some());
    assert_eq!(
        object_store(&server, "cuenv")
            .list(&harness.tenant)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn rollback_rejects_changed_physical_identity_without_state_changes() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(
        None,
        "cuenv",
        &harness.tenant,
    )
    .unwrap();
    let before = server.object("state", &path.state_key()).unwrap();
    let mut action = migrate(MigrationDirection::Rollback);
    if let InfrastructureAction::State(StateAction::Migrate(request)) = &mut action {
        request.physical_identities = Some(MigrationPhysicalIdentities {
            source: uuid::Uuid::from_u128(3),
            target: uuid::Uuid::from_u128(2),
        });
    }
    assert!(run_migrating(&harness, &server, action).await.is_err());
    assert_eq!(server.object("state", &path.state_key()), Some(before));
    assert!(harness.current_lock().await.is_some());
}

#[tokio::test]
async fn rollback_refuses_exhausted_manifest_before_changing_either_store() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();

    let target = object_store(&server, "cuenv");
    let mut manifest = target.manifest(&harness.tenant).await.unwrap().unwrap();
    manifest.serial = Some(u64::MAX - 1);
    seed_manifest(&server, &harness.tenant, &manifest);
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(
        None,
        "cuenv",
        &harness.tenant,
    )
    .unwrap();
    let before_state = server.object("state", &path.state_key()).unwrap();
    let before_manifest = server
        .object("state", &path.migration_manifest_key())
        .unwrap();
    let first_request = server.requests().len();

    let error = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("serial capacity"), "{error}");
    assert_eq!(
        server.object("state", &path.state_key()),
        Some(before_state)
    );
    assert_eq!(
        server.object("state", &path.migration_manifest_key()),
        Some(before_manifest)
    );
    assert!(harness.current_lock().await.is_some());
    assert!(
        server.requests()[first_request..]
            .iter()
            .all(|request| request.method != "PUT")
    );
}

#[tokio::test]
async fn all_environment_rollback_rejects_extra_target_state_before_any_change() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let target = object_store(&server, "cuenv");
    let extra = TenantKey::with_environment(MODULE, "app", "unexpected").unwrap();
    seed(
        target.as_ref(),
        &extra,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.extra"],
        },
    )
    .await;
    let before = target.list(&harness.tenant).await.unwrap();
    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)).await;
    assert!(result.is_err());
    assert_eq!(target.list(&harness.tenant).await.unwrap(), before);
    assert_eq!(target.list(&extra).await.unwrap().len(), 1);
    assert!(harness.current_lock().await.is_some());
}

#[tokio::test]
async fn all_environment_rollback_uses_the_recorded_set_when_source_state_disappears() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let held = harness.current_lock().await.unwrap();
    let fence = StateLock {
        lock_identifier: held.identifier().unwrap().to_string(),
    };
    harness
        .store
        .delete(
            &harness.tenant,
            &fence,
            &ResourceAddress::new("random_pet", "original"),
        )
        .await
        .unwrap();
    let access = cuenv_infrastructure::state::LegacyMigrationAccess {
        tenant: &harness.tenant,
        fence: &fence,
    };
    assert!(matches!(
        harness
            .store
            .set_legacy_migration_writer(
                &cuenv_infrastructure::state::LegacyMigrationWriterRequest {
                    access,
                    writer: None,
                }
            )
            .await
            .unwrap(),
        cuenv_infrastructure::state::MigrationMutation::Applied
    ));
    assert!(harness.store.unlock(&harness.tenant, &fence).await.unwrap());
    assert!(
        harness
            .store
            .environments(MODULE, "app")
            .await
            .unwrap()
            .is_empty()
    );
    let target = object_store(&server, "cuenv");
    let before = target.list(&harness.tenant).await.unwrap();
    assert!(
        run_migrating(&harness, &server, migrate(MigrationDirection::Rollback))
            .await
            .is_err()
    );
    assert_eq!(target.list(&harness.tenant).await.unwrap(), before);
}

#[tokio::test]
async fn rollback_keeps_the_target_protected_when_source_unlock_fails() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    harness.counting.fail_unlock.store(true, Ordering::SeqCst);

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::Rollback)).await;
    assert!(result.is_err());
    assert!(harness.current_lock().await.is_some());
    let target = object_store(&server, "cuenv");
    let held = target.current_lock(&harness.tenant).await.unwrap().unwrap();
    assert!(matches!(
        held.identity,
        cuenv_infrastructure::LockIdentity::Migration { .. }
    ));
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(
        None,
        "cuenv",
        &harness.tenant,
    )
    .unwrap();
    let raw = server.object("state", &path.state_key()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(!document["tombstone"].is_null());
    assert!(document["records"].get("random_pet.original").is_some());
    assert!(document["lock"].is_object());
}

#[tokio::test]
async fn copy_back_rejects_unknown_target_fields_before_changing_the_source() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.original"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();
    let before = harness.store.list(&harness.tenant).await.unwrap();
    let path = cuenv_infrastructure::state::object::key::TenantPath::try_new(
        None,
        "cuenv",
        &harness.tenant,
    )
    .unwrap();
    let raw = server.object("state", &path.state_key()).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    document["records"]["random_pet.original"]["futureExtension"] =
        serde_json::json!({"value": "preserve-me"});
    document["serial"] = serde_json::json!(document["serial"].as_u64().unwrap() + 1);
    document["writer"] = serde_json::json!(uuid::Uuid::new_v4());
    server.put_object(
        "state",
        &path.state_key(),
        serde_json::to_vec(&document).unwrap(),
    );

    let result = run_migrating(&harness, &server, migrate(MigrationDirection::CopyBack)).await;
    assert!(result.is_err());
    assert_eq!(harness.store.list(&harness.tenant).await.unwrap(), before);
    assert!(harness.current_lock().await.is_some());
    let target = object_store(&server, "cuenv");
    assert!(matches!(
        target
            .current_lock(&harness.tenant)
            .await
            .unwrap()
            .unwrap()
            .identity,
        cuenv_infrastructure::LockIdentity::Migration { .. }
    ));
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
        SeedResources {
            writer: &writer,
            resources: &["random_pet.pet", "random_pet.cat"],
        },
    )
    .await;
    seed(
        harness.store.as_ref(),
        &prod,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.fox"],
        },
    )
    .await;

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
        assert!(fence.identifier().unwrap().starts_with("migrated-"));
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
        lock_identifier: Some(fence.identifier().unwrap().to_string()),
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
async fn rollback_refuses_sequential_target_changes_until_copied_back() {
    let server = FakeS3::start().await.unwrap();
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.pet", "random_pet.cat"],
        },
    )
    .await;
    run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
        .await
        .unwrap();

    // A run against the new backend changes the state.
    let target = object_store(&server, "cuenv");
    let lock = StateLock::generate();
    let target_writer = ProjectInstance::try_new("deploy/new", "cuenv").unwrap();
    target
        .acquire_lock(
            &harness.tenant,
            &LockRequest {
                lock: &lock,
                holder: "apply",
                instance: Some(&target_writer),
            },
        )
        .await
        .unwrap();
    target
        .put(&harness.tenant, &lock, &super::managed("random_pet.owl"))
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
    let mut expected_records = target.list(&harness.tenant).await.unwrap();
    expected_records.sort_by(|left, right| left.address.cmp(&right.address));
    let expected_writer = target.last_writer(&harness.tenant).await.unwrap();

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
    let mut copied_records = harness.store.list(&harness.tenant).await.unwrap();
    copied_records.sort_by(|left, right| left.address.cmp(&right.address));
    assert_eq!(copied_records, expected_records);
    assert_eq!(
        harness.store.last_writer(&harness.tenant).await.unwrap(),
        expected_writer
    );
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn migration_refuses_a_target_that_already_holds_state() {
    let server = FakeS3::start().await.unwrap();
    let target = object_store(&server, "cuenv");
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
    seed(
        harness.store.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.pet"],
        },
    )
    .await;
    seed(
        target.as_ref(),
        &harness.tenant,
        SeedResources {
            writer: &writer,
            resources: &["random_pet.cat"],
        },
    )
    .await;
    let source_before = harness.store.list(&harness.tenant).await.unwrap();
    let target_before = target.list(&harness.tenant).await.unwrap();
    let error = super::super::dispatch::dispatch(
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
        "preflight must not acquire a source fence"
    );
    assert_eq!(source_before.len(), 1);
    assert_eq!(source_before[0].address.to_string(), "random_pet.pet");
    assert_eq!(target_before.len(), 1);
    assert_eq!(target_before[0].address.to_string(), "random_pet.cat");
    assert_eq!(
        harness.store.list(&harness.tenant).await.unwrap(),
        source_before
    );
    assert_eq!(target.list(&harness.tenant).await.unwrap(), target_before);
}

#[tokio::test]
async fn migration_refuses_an_incomplete_target_inventory_before_writes() {
    let mut invalid_utf8 = b"<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>".to_vec();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(b"</Key></Contents></ListBucketResult>");
    for fault in [
        FaultAction::Status(
            200,
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated></ListBucketResult>".into(),
        ),
        FaultAction::Status(
            200,
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>bad%zz</Key></Contents></ListBucketResult>".into(),
        ),
        FaultAction::StatusBytes(200, invalid_utf8),
    ] {
        let server = FakeS3::start().await.unwrap();
        let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
        let writer = ProjectInstance::try_new("app", "cuenv").unwrap();
        seed(
            harness.store.as_ref(),
            &harness.tenant,
            SeedResources {
                writer: &writer,
                resources: &["random_pet.pet"],
            },
        )
        .await;
        let source_before = harness.store.list(&harness.tenant).await.unwrap();
        server.fault_exact("GET", "", fault);
        let request_start = server.requests().len();

        let error = run_migrating(&harness, &server, migrate(MigrationDirection::Forward))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot list migration target"));
        assert!(harness.current_lock().await.is_none());
        assert_eq!(
            harness.store.list(&harness.tenant).await.unwrap(),
            source_before
        );
        assert!(
            server.requests()[request_start..]
                .iter()
                .all(|request| request.method == "GET"),
            "a failed target inventory must not write or delete any object"
        );
    }
}
