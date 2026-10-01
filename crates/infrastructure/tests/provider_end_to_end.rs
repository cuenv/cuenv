//! End-to-end tests against real Terraform provider binaries.
//!
//! Ignored by default because they need provider executables (and,
//! optionally, a libSQL server). Run with:
//!
//! ```text
//! CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER=/path/terraform-provider-random_v3.7.2_x5 \
//! CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER=/path/terraform-provider-local_v2.9.1_x5 \
//! CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER=/path/terraform-provider-tfe_v0.81.0_x5 \
//! CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER=/path/terraform-provider-fake \
//! CUENV_INFRASTRUCTURE_TEST_TURSO_URL=http://127.0.0.1:8080 \
//! cargo test -p cuenv-infrastructure --test provider_end_to_end -- --ignored --nocapture
//! ```
//!
//! Without `CUENV_INFRASTRUCTURE_TEST_TURSO_URL` the in-memory store is used.
//!
//! The fake provider lives in `tests/fake_provider`; build it with
//! `go build -o terraform-provider-fake .` in that directory. It reproduces
//! provider behaviours real providers show only occasionally: nested
//! attributes and blocks with computed children, JSON-encoded planned and
//! upgraded states, semantic equality, creates that fail part way, deletes
//! that fail or return the object, and creates that run until stopped.
//!
//! `installs_provider_from_registry` downloads from registry.terraform.io.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cuenv_infrastructure::{
    Action, ApplyContext, ApplyEvent, Cancellation, ConditionalPut, EngineOptions, EngineSetup,
    InfrastructureEngine, InfrastructureError, LockRequest, MemoryStateStore, OwnerClaim,
    OwnerClaimMode, Plan, PlanMode, PlanSummary, ProjectInstance, RecordVersion, StateLock,
    StateStore, TenantKey, TursoConfiguration, TursoStateStore,
};
use cuenv_manifest::manifest::Infrastructure;
use serde_json::json;

type TestResult<Success = ()> = Result<Success, Box<dyn std::error::Error>>;

fn environment_path(name: &str) -> TestResult<String> {
    std::env::var(name).map_err(|_| format!("{name} must point at a provider binary").into())
}

/// Desired configuration: a pet, plus a file holding `file_content` when set.
struct Desired<'path> {
    file: &'path Path,
    file_content: Option<&'path str>,
    pet_length: u32,
}

fn infrastructure(desired: &Desired<'_>) -> TestResult<Infrastructure> {
    let mut resources = json!({
        "pet": {"type": "random_pet", "configuration": {"length": desired.pet_length, "separator": "-"}},
    });
    if let Some(content) = desired.file_content {
        resources["greeting"] = json!({
            "type": "local_file",
            "dependsOn": ["pet"],
            "configuration": {"filename": desired.file.to_string_lossy(), "content": content},
        });
    }
    Ok(serde_json::from_value(json!({
        "state": {"turso": {"url": "http://unused"}},
        "providers": {
            "random": {"source": "hashicorp/random", "path": environment_path("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?},
            "local": {"source": "hashicorp/local", "path": environment_path("CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER")?},
        },
        "resources": resources,
    }))?)
}

async fn store() -> TestResult<Arc<dyn StateStore>> {
    let store: Arc<dyn StateStore> = match std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL") {
        Ok(url) => Arc::new(TursoStateStore::new(TursoConfiguration {
            url,
            authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
        })?),
        Err(_) => Arc::new(MemoryStateStore::new()),
    };
    store.migrate().await?;
    Ok(store)
}

/// Removes a tenant's rows (resources, lock and owner) from the shared Turso
/// database when dropped, so a test that fails halfway, or never destroys
/// what it created, leaves nothing behind. Does nothing with the in-memory
/// store.
struct TenantCleanup {
    tenant: TenantKey,
}

impl TenantCleanup {
    fn new(tenant: &TenantKey) -> Self {
        Self {
            tenant: tenant.clone(),
        }
    }

    async fn purge(url: &str, token: Option<&str>, tenant: &TenantKey) -> TestResult {
        let arguments: Vec<serde_json::Value> = [
            tenant.module_path(),
            tenant.project(),
            tenant.environment().unwrap_or(""),
        ]
        .iter()
        .map(|value| json!({"type": "text", "value": value}))
        .collect();
        let mut requests: Vec<serde_json::Value> = [
            "cuenv_infrastructure_resources",
            "cuenv_infrastructure_locks",
            "cuenv_infrastructure_owners",
        ]
        .iter()
        .map(|table| {
            json!({"type": "execute", "stmt": {
                "sql": format!(
                    "DELETE FROM {table} WHERE module_path = ? AND project = ? AND environment = ?"
                ),
                "args": arguments,
            }})
        })
        .collect();
        requests.push(json!({"type": "close"}));
        let mut request = reqwest::Client::new()
            .post(format!("{}/v2/pipeline", url.trim_end_matches('/')))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&json!({"requests": requests}))?);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await?.error_for_status()?;
        Ok(())
    }
}

impl Drop for TenantCleanup {
    fn drop(&mut self) {
        let Ok(url) = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL") else {
            return;
        };
        let token = std::env::var("TURSO_AUTH_TOKEN").ok();
        let tenant = self.tenant.clone();
        // A runtime of its own: the test's runtime may be shutting down.
        let cleaned = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?
                .block_on(Self::purge(&url, token.as_deref(), &tenant))
                .map_err(|error| error.to_string())
        })
        .join();
        // A cleanup that fails leaves rows behind, which the next run's
        // tenants do not collide with; the test's own result stands.
        drop(cleaned);
    }
}

fn engine_options(project_directory: &Path, cancellation: &Cancellation) -> EngineOptions {
    EngineOptions {
        project_directory: project_directory.to_path_buf(),
        plugin_cache_directory: None,
        withheld_environment_variables: vec!["TURSO_AUTH_TOKEN".to_string()],
        provider_environment_variables: BTreeMap::new(),
        unrecorded_directory: Some(project_directory.join("unrecorded")),
        cancellation: cancellation.clone(),
    }
}

/// One plan and apply of [`plan_and_apply`].
struct Convergence<'convergence> {
    store: &'convergence Arc<dyn StateStore>,
    tenant: &'convergence TenantKey,
    desired: &'convergence Desired<'convergence>,
    mode: PlanMode,
}

async fn plan_and_apply(convergence: &Convergence<'_>) -> TestResult<Plan> {
    let Convergence {
        store,
        tenant,
        desired,
        mode,
    } = convergence;
    let project = tempfile::tempdir()?;
    let mut engine = InfrastructureEngine::new(EngineSetup {
        tenant: (*tenant).clone(),
        store: Arc::clone(store),
        infrastructure: infrastructure(desired)?,
        options: engine_options(project.path(), &Cancellation::default()),
    });
    let lock = store.lock(tenant, "provider_end_to_end").await?;
    let plan = engine.plan(*mode).await?;
    // Planning twice against unchanged state yields the same digest.
    assert_eq!(engine.plan(*mode).await?.digest(), plan.digest());
    engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
        .await?;
    // Applying the same plan again is refused: its records are stale now.
    if plan.has_work() {
        assert!(matches!(
            engine
                .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
                .await,
            Err(InfrastructureError::PlanOutdated { .. })
        ));
    }
    store.unlock(tenant, &lock).await?;
    engine.shutdown().await;
    Ok(plan)
}

fn actions(plan: &Plan) -> Vec<(String, Action)> {
    plan.changes
        .iter()
        .map(|change| (change.address.to_string(), change.action))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Terraform provider binaries; see module documentation"]
async fn managed_resource_lifecycle() -> TestResult {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("greeting.txt");
    let desired = |file_content, pet_length| Desired {
        file: &file,
        file_content,
        pet_length,
    };
    let store = store().await?;
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/end-to-end-{run}@v0"), "web")?;
    let neighbour = TenantKey::new(format!("example.com/end-to-end-{run}"), "api")?;
    let _cleanup = (TenantCleanup::new(&tenant), TenantCleanup::new(&neighbour));

    // Create both resources, dependency first.
    let plan = plan_and_apply(&Convergence {
        store: &store,
        tenant: &tenant,
        desired: &desired(Some("hello"), 2),
        mode: PlanMode::Apply,
    })
    .await?;
    assert_eq!(
        actions(&plan),
        vec![
            ("random_pet.pet".to_string(), Action::Create),
            ("local_file.greeting".to_string(), Action::Create),
        ]
    );
    assert_eq!(std::fs::read_to_string(&file)?, "hello");
    let rows = store.list(&tenant).await?;
    assert_eq!(rows.len(), 2);
    let pet = rows
        .iter()
        .find(|row| row.address.name == "pet")
        .ok_or("pet not recorded")?;
    assert_eq!(
        pet.provider_source,
        "registry.terraform.io/hashicorp/random"
    );
    let pet_identifier = pet.state["id"].as_str().ok_or("no id")?.to_string();
    assert_eq!(pet_identifier.split('-').count(), 2);

    // The same project name under another module sees nothing.
    assert!(store.list(&neighbour).await?.is_empty());

    // Re-planning unchanged configuration is a no-op with nothing to write.
    let plan = plan_and_apply(&Convergence {
        store: &store,
        tenant: &tenant,
        desired: &desired(Some("hello"), 2),
        mode: PlanMode::Apply,
    })
    .await?;
    assert!(!plan.has_work(), "expected no work: {:?}", actions(&plan));

    // Changing file content forces replacement (local_file is immutable).
    let plan = plan_and_apply(&Convergence {
        store: &store,
        tenant: &tenant,
        desired: &desired(Some("world"), 2),
        mode: PlanMode::Apply,
    })
    .await?;
    // Changes appear in the order apply runs them; unchanged resources last.
    assert_eq!(
        actions(&plan),
        vec![
            ("local_file.greeting".to_string(), Action::Replace),
            ("random_pet.pet".to_string(), Action::NoOp),
        ]
    );
    assert_eq!(std::fs::read_to_string(&file)?, "world");

    // Dropping a resource from configuration deletes it.
    let plan = plan_and_apply(&Convergence {
        store: &store,
        tenant: &tenant,
        desired: &desired(None, 3),
        mode: PlanMode::Apply,
    })
    .await?;
    assert_eq!(
        actions(&plan),
        vec![
            ("local_file.greeting".to_string(), Action::Delete),
            ("random_pet.pet".to_string(), Action::Replace),
        ]
    );
    assert!(!file.exists());
    let rows = store.list(&tenant).await?;
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].state["id"].as_str(), Some(pet_identifier.as_str()));

    // Destroy removes everything the tenant owns.
    let plan = plan_and_apply(&Convergence {
        store: &store,
        tenant: &tenant,
        desired: &desired(None, 3),
        mode: PlanMode::Destroy,
    })
    .await?;
    assert_eq!(
        actions(&plan),
        vec![("random_pet.pet".to_string(), Action::Delete)]
    );
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a protocol 6 provider binary (CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER)"]
async fn protocol_6_provider_schema() -> TestResult {
    let binary = environment_path("CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER")?;
    let cancellation = Cancellation::default();
    let client = cuenv_infrastructure::plugin::ProviderClient::launch(
        &cuenv_infrastructure::plugin::LaunchOptions {
            binary: Path::new(&binary),
            withheld_environment_variables: &[],
            provider_environment_variables: &BTreeMap::new(),
            cancellation: &cancellation,
        },
    )
    .await?;
    assert_eq!(
        client.protocol(),
        cuenv_infrastructure::plugin::Protocol::Version6
    );
    let (schema, _) = client.schema().await?;
    let organization = schema
        .resources
        .get("tfe_organization")
        .ok_or("tfe_organization missing")?;
    assert!(organization.block.attributes.contains_key("email"));
    client.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads from registry.terraform.io"]
async fn installs_provider_from_registry() -> TestResult {
    let cache = tempfile::tempdir()?;
    let installer =
        cuenv_infrastructure::registry::ProviderInstaller::new(cache.path().to_path_buf())?;
    let source = cuenv_infrastructure::registry::ProviderSource::parse("hashicorp/random")?;
    let binary = installer.ensure(&source, "3.7.2").await?;
    assert!(binary.starts_with(cache.path()));
    // Second call is served from the cache.
    assert_eq!(installer.ensure(&source, "3.7.2").await?, binary);
    let cancellation = Cancellation::default();
    let client = cuenv_infrastructure::plugin::ProviderClient::launch(
        &cuenv_infrastructure::plugin::LaunchOptions {
            binary: &binary,
            withheld_environment_variables: &[],
            provider_environment_variables: &BTreeMap::new(),
            cancellation: &cancellation,
        },
    )
    .await?;
    assert_eq!(
        client.protocol(),
        cuenv_infrastructure::plugin::Protocol::Version5
    );
    client.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a libSQL server (CUENV_INFRASTRUCTURE_TEST_TURSO_URL)"]
async fn turso_recovery_refuses_recreated_records_at_the_same_serial() -> TestResult {
    let state = TursoStateStore::new(TursoConfiguration {
        url: std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })?;
    state.migrate().await?;
    let backend_identity = state
        .recovery_identity()
        .ok_or("missing backend identity")?;
    for environment in [None, Some("Dev")] {
        let module = format!("example.com/recovery-generation-{}", uuid::Uuid::new_v4());
        let tenant = if let Some(environment) = environment {
            TenantKey::with_environment(module, "web", environment)?
        } else {
            TenantKey::new(module, "web")?
        };
        let _cleanup = TenantCleanup::new(&tenant);
        let directory = tempfile::tempdir()?;
        let unrecorded = cuenv_infrastructure::UnrecordedStore::at(directory.path())
            .with_backend_identity(&backend_identity)?;
        let lock = state.lock(&tenant, "generation recovery").await?;
        let original = cuenv_infrastructure::ManagedResource {
            address: cuenv_infrastructure::ResourceAddress::new("random_pet", "pet"),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: json!({"id": "original"}),
            private: Vec::new(),
            dependencies: Vec::new(),
            tainted: false,
            identity: None,
            serial: 0,
            generation: uuid::Uuid::nil(),
        };
        state.put(&tenant, &lock, &original).await?;
        let previous = state.list(&tenant).await?[0].clone();
        let pending = cuenv_infrastructure::ManagedResource {
            state: json!({"id": "pending"}),
            ..original.clone()
        };
        unrecorded.save(
            &tenant,
            &ConditionalPut {
                resource: &pending,
                expected: RecordVersion::of(Some(&previous)),
            },
        )?;
        state.delete(&tenant, &lock, &original.address).await?;
        state
            .put(
                &tenant,
                &lock,
                &cuenv_infrastructure::ManagedResource {
                    state: json!({"id": "new"}),
                    ..original.clone()
                },
            )
            .await?;
        let recreated = state.list(&tenant).await?[0].clone();
        assert_eq!(previous.serial, recreated.serial);
        assert_ne!(previous.generation, recreated.generation);
        let recovered = unrecorded
            .recover(
                &state,
                &tenant,
                &cuenv_infrastructure::RecoverOptions {
                    lock: &lock,
                    overrides: cuenv_infrastructure::RecoverOverrides::default(),
                },
            )
            .await;
        assert!(matches!(
            recovered,
            Err(InfrastructureError::StateChanged { .. })
        ));
        assert_eq!(state.list(&tenant).await?, vec![recreated]);
        assert_eq!(unrecorded.list(&tenant)?.len(), 1);
        state.delete(&tenant, &lock, &original.address).await?;
        // Identical content from an independent insertion cannot acknowledge
        // an absent-record recovery whose response may have been lost.
        let independent_directory = tempfile::tempdir()?;
        let independent_saved =
            cuenv_infrastructure::UnrecordedStore::at(independent_directory.path())
                .with_backend_identity(&backend_identity)?;
        independent_saved.save(
            &tenant,
            &ConditionalPut {
                resource: &original,
                expected: RecordVersion::Absent,
            },
        )?;
        let pending = independent_saved.list(&tenant)?[0].record.clone();
        state.put(&tenant, &lock, &pending).await?;
        let independent = state.list(&tenant).await?[0].clone();
        assert!(independent.same_content(&pending));
        assert_ne!(independent.generation, pending.generation);
        let recovered = independent_saved
            .recover(
                &state,
                &tenant,
                &cuenv_infrastructure::RecoverOptions {
                    lock: &lock,
                    overrides: cuenv_infrastructure::RecoverOverrides::default(),
                },
            )
            .await;
        assert!(matches!(
            recovered,
            Err(InfrastructureError::StateChanged { .. })
        ));
        assert_eq!(state.list(&tenant).await?, vec![independent]);
        assert_eq!(independent_saved.list(&tenant)?.len(), 1);
        state.delete(&tenant, &lock, &original.address).await?;
        state.unlock(&tenant, &lock).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a libSQL server (CUENV_INFRASTRUCTURE_TEST_TURSO_URL)"]
async fn turso_store_round_trips_records() -> TestResult {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?;
    let store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })?;
    store.migrate().await?;
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/store-{run}"), "web")?;
    let _cleanup = TenantCleanup::new(&tenant);
    let record = cuenv_infrastructure::ManagedResource {
        address: cuenv_infrastructure::ResourceAddress::new("random_pet", "pet"),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 2,
        state: json!({"id": "happy-otter", "nested": {"list": [1, 2]}}),
        // Lengths 1..=3 exercise every base64 padding case.
        private: vec![0, 255, 7, 42, 1],
        dependencies: vec!["other".into()],
        tainted: true,
        identity: Some(json!({"name": "a-b"})),
        serial: 0,
        generation: uuid::Uuid::nil(),
    };
    // The caller chooses the lock identifier before acquiring it.
    let chosen = StateLock::generate();
    let lock = store
        .acquire_lock(
            &tenant,
            &LockRequest {
                lock: &chosen,
                holder: "round trip",
            },
        )
        .await?;
    assert_eq!(lock, chosen);
    assert_eq!(
        store
            .current_lock(&tenant)
            .await?
            .ok_or("no lock")?
            .lock_identifier,
        chosen.lock_identifier
    );
    store.put(&tenant, &lock, &record).await?;
    let mut updated = record.clone();
    updated.private = vec![9];
    store.put(&tenant, &lock, &updated).await?;
    let stored = store.list(&tenant).await?;
    assert_eq!(stored.len(), 1);
    assert!(stored[0].same_content(&updated));
    assert_eq!(stored[0].serial, 2);

    // Conditional writes compare the serial.
    let mut newer = updated.clone();
    newer.private = vec![10];
    let stale = store
        .put_if_unchanged(
            &tenant,
            &lock,
            &ConditionalPut {
                resource: &newer,
                expected: RecordVersion::Generation {
                    generation: stored[0].generation,
                    serial: 1,
                },
            },
        )
        .await;
    assert!(
        matches!(&stale, Err(InfrastructureError::StateChanged { address, .. }) if address == "random_pet.pet"),
        "{stale:?}"
    );
    store
        .put_if_unchanged(
            &tenant,
            &lock,
            &ConditionalPut {
                resource: &newer,
                expected: RecordVersion::of(Some(&stored[0])),
            },
        )
        .await?;
    assert_eq!(store.list(&tenant).await?[0].serial, 3);
    let absent = cuenv_infrastructure::ManagedResource {
        address: cuenv_infrastructure::ResourceAddress::new("random_pet", "other"),
        generation: uuid::Uuid::new_v4(),
        ..newer.clone()
    };
    let conditional_absent = ConditionalPut {
        resource: &absent,
        expected: RecordVersion::Absent,
    };
    store
        .put_if_unchanged(&tenant, &lock, &conditional_absent)
        .await?;
    // Repeating the same write (a retry whose response was lost) succeeds;
    // a different record expecting no record is refused.
    store
        .put_if_unchanged(&tenant, &lock, &conditional_absent)
        .await?;
    let different = cuenv_infrastructure::ManagedResource {
        private: vec![11],
        ..absent.clone()
    };
    assert!(matches!(
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &different,
                    expected: RecordVersion::Absent,
                },
            )
            .await,
        Err(InfrastructureError::StateChanged { .. })
    ));
    store.delete(&tenant, &lock, &absent.address).await?;

    // The owner record is claimed once and transferred explicitly.
    let root = ProjectInstance::new(".", "web")?;
    let copy = ProjectInstance::new("_copy", "web")?;
    assert_eq!(store.owner(&tenant).await?, None);
    let claim = |instance, mode| OwnerClaim { instance, mode };
    let owner = store
        .claim_owner(&tenant, &lock, &claim(&root, OwnerClaimMode::IfUnowned))
        .await?;
    assert_eq!(owner.instance, root);
    let kept = store
        .claim_owner(&tenant, &lock, &claim(&copy, OwnerClaimMode::IfUnowned))
        .await?;
    assert_eq!(kept.instance, root);
    assert!(kept.require(&tenant, &copy).is_err());
    let adopted = store
        .claim_owner(&tenant, &lock, &claim(&copy, OwnerClaimMode::Transfer))
        .await?;
    assert_eq!(adopted.instance, copy);
    assert_eq!(
        store.owner(&tenant).await?.ok_or("no owner")?.instance,
        copy
    );

    store.delete(&tenant, &lock, &updated.address).await?;
    store.unlock(&tenant, &lock).await?;
    assert!(matches!(
        store.put(&tenant, &lock, &updated).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    assert!(matches!(
        store
            .claim_owner(&tenant, &lock, &claim(&root, OwnerClaimMode::Transfer))
            .await,
        Err(InfrastructureError::LockLost { .. })
    ));
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// Fake provider
// ---------------------------------------------------------------------------

fn ordered_resources(suffix: &str) -> serde_json::Value {
    json!({
        "parent": {"type": "fake_ordered", "configuration": {"name": format!("parent-{suffix}")}},
        "dependent": {"type": "fake_ordered", "dependsOn": ["parent"], "configuration": {
            "name": format!("dependent-{suffix}"), "parent_name": format!("parent-{suffix}")
        }},
    })
}

/// The fake provider refuses to delete a parent while its dependent is
/// attached. Terraform deletes before it updates (the creators-to-destroyers
/// edge), so removing a parent and the dependent's reference to it in one
/// change stops at the refused delete: nothing changes, nothing is lost, and
/// the change goes through in two steps.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_removing_a_parent_and_detaching_its_dependent_in_one_change_stops_safely()
-> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    let detached = json!({
        "dependent": {"type": "fake_ordered", "configuration": {"name": "dependent-old"}},
    });
    let run = fake.converge(&detached, PlanMode::Apply).await?;
    assert_eq!(
        actions(&run.plan),
        vec![
            ("fake_ordered.parent".to_string(), Action::Delete),
            ("fake_ordered.dependent".to_string(), Action::Update),
        ]
    );
    let error = run.applied.expect_err("the parent's delete is refused");
    assert_eq!(failed_addresses(&error), ["fake_ordered.parent"]);
    assert_eq!(skipped_addresses(&error), ["fake_ordered.dependent"]);
    assert!(fake.journal().is_empty(), "{}", fake.journal());
    let rows = fake.store.list(&fake.tenant).await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        fake.record("dependent").await?.dependencies,
        vec!["fake_ordered.parent"]
    );

    // Step one: keep the parent and detach the dependent.
    let kept = json!({
        "parent": {"type": "fake_ordered", "configuration": {"name": "parent-old"}},
        "dependent": {"type": "fake_ordered", "configuration": {"name": "dependent-old"}},
    });
    let result = fake.converge(&kept, PlanMode::Apply).await?.applied?;
    assert_eq!(result.update, 1);
    assert!(fake.record("dependent").await?.dependencies.is_empty());
    // Step two: remove the parent.
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    let result = fake.converge(&detached, PlanMode::Apply).await?.applied?;
    assert_eq!(result.delete, 1);
    assert_eq!(
        fake.journal().lines().collect::<Vec<_>>(),
        vec!["ordered: Delete name=parent-old"]
    );
    fake.converge(&detached, PlanMode::Destroy).await?.applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_replacing_a_parent_and_detaching_its_dependent_in_one_change_stops_safely()
-> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    let detached = json!({
        "parent": {"type": "fake_ordered", "configuration": {"name": "parent-new"}},
        "dependent": {"type": "fake_ordered", "configuration": {"name": "dependent-old"}},
    });
    let run = fake.converge(&detached, PlanMode::Apply).await?;
    let error = run.applied.expect_err("the parent's delete is refused");
    assert_eq!(failed_addresses(&error), ["fake_ordered.parent"]);
    // The parent was not deleted, so it is not reported as deleted and not
    // recreated, and its create did not run.
    assert!(uncreated_in_error(&error).is_empty());
    assert!(uncreated_addresses(&run.events).is_empty());
    assert!(fake.journal().is_empty(), "{}", fake.journal());
    assert_eq!(fake.record("parent").await?.state["name"], "parent-old");

    // Step one: detach the dependent from the parent that stays.
    let kept = json!({
        "parent": {"type": "fake_ordered", "configuration": {"name": "parent-old"}},
        "dependent": {"type": "fake_ordered", "configuration": {"name": "dependent-old"}},
    });
    fake.converge(&kept, PlanMode::Apply).await?.applied?;
    // Step two: replace the parent.
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    let result = fake.converge(&detached, PlanMode::Apply).await?.applied?;
    assert_eq!(result.replace, 1);
    assert_eq!(
        fake.journal().lines().collect::<Vec<_>>(),
        vec![
            "ordered: Delete name=parent-old",
            "ordered: Create name=parent-new",
        ]
    );
    fake.converge(&detached, PlanMode::Destroy).await?.applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_failed_detachment_retains_old_edges_for_safe_destroy() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    fake.set_flag("fail-ordered-update")?;
    let desired = json!({
        "parent": {"type": "fake_ordered", "configuration": {"name": "parent-old"}},
        "dependent": {"type": "fake_ordered", "configuration": {"name": "dependent-old"}},
    });
    assert!(
        fake.converge(&desired, PlanMode::Apply)
            .await?
            .applied
            .is_err()
    );
    let rows = fake.store.list(&fake.tenant).await?;
    let dependent = rows
        .iter()
        .find(|row| row.address.name == "dependent")
        .ok_or("missing dependent")?;
    assert_eq!(dependent.dependencies, vec!["fake_ordered.parent"]);
    assert_eq!(dependent.state["parent_name"], "parent-old");
    fake.clear_flag("fail-ordered-update")?;
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    fake.converge(&desired, PlanMode::Destroy).await?.applied?;
    assert_eq!(
        fake.journal().lines().collect::<Vec<_>>(),
        vec![
            "ordered: Delete name=dependent-old",
            "ordered: Delete name=parent-old",
        ]
    );
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_dependent_replacements_destroy_in_reverse_and_create_forward() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    let journal_file = fake.directory.path().join("journal.log");
    std::fs::write(&journal_file, "")?;
    let replaced = fake
        .converge(&ordered_resources("new"), PlanMode::Apply)
        .await?;
    assert_eq!(replaced.applied?.replace, 2);
    assert_eq!(
        fake.journal().lines().collect::<Vec<_>>(),
        vec![
            "ordered: Delete name=dependent-old",
            "ordered: Delete name=parent-old",
            "ordered: Create name=parent-new",
            "ordered: Create name=dependent-new",
        ]
    );
    let rows = fake.store.list(&fake.tenant).await?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.state["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("-new"))
    }));
    fake.converge(&ordered_resources("new"), PlanMode::Destroy)
        .await?
        .applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_failed_dependent_delete_keeps_the_parent_and_both_records() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    fake.set_flag("fail-ordered-delete")?;
    std::fs::write(fake.directory.path().join("journal.log"), "")?;
    let replaced = fake
        .converge(&ordered_resources("new"), PlanMode::Apply)
        .await?;
    assert!(replaced.applied.is_err());
    let rows = fake.store.list(&fake.tenant).await?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.state["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("-old"))
    }));
    assert!(!fake.journal().contains("Delete name=parent-old"));
    assert!(!fake.journal().contains("Create name=parent-new"));
    assert_eq!(
        std::fs::read_to_string(fake.directory.path().join("ordered-parent"))?,
        "parent-old"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider binary"]
async fn fake_stop_during_dependent_delete_records_it_and_can_resume() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&ordered_resources("old"), PlanMode::Apply)
        .await?
        .applied?;
    fake.set_flag("slow-ordered-delete")?;
    let cancellation = Cancellation::default();
    let mut engine = fake.engine(&ordered_resources("new"), &cancellation)?;
    let lock = fake
        .store
        .lock(&fake.tenant, "replacement interrupt")
        .await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    let mut on_event = |_| {};
    let (applied, ()) = tokio::join!(
        engine.apply(&plan, ApplyContext { lock: &lock }, &mut on_event),
        async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancellation.stop();
        }
    );
    assert!(matches!(
        applied,
        Err(InfrastructureError::Interrupted {
            completed: 0,
            total: 2
        })
    ));
    let rows = fake.store.list(&fake.tenant).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].address.name, "parent");
    assert_eq!(rows[0].state["name"], "parent-old");
    assert!(!fake.directory.path().join("ordered-dependent").exists());
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    fake.converge(&ordered_resources("new"), PlanMode::Apply)
        .await?
        .applied?;
    let resumed = fake.store.list(&fake.tenant).await?;
    assert_eq!(resumed.len(), 2);
    assert!(resumed.iter().all(|row| {
        row.state["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("-new"))
    }));
    Ok(())
}

/// One tenant driven through the fake provider, with its own journal and
/// flag directory.
struct Fake {
    directory: tempfile::TempDir,
    store: Arc<dyn StateStore>,
    tenant: TenantKey,
    /// The provider executable the engines launch.
    binary: String,
    _cleanup: TenantCleanup,
}

/// The outcome of one plan and apply.
struct Converged {
    plan: Plan,
    applied: Result<PlanSummary, InfrastructureError>,
    events: Vec<ApplyEvent>,
}

impl Fake {
    async fn new() -> TestResult<Self> {
        let run = uuid::Uuid::new_v4().to_string();
        let tenant = TenantKey::new(format!("example.com/fake-{run}"), "web")?;
        Ok(Self {
            directory: tempfile::tempdir()?,
            store: store().await?,
            _cleanup: TenantCleanup::new(&tenant),
            tenant,
            binary: environment_path("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?,
        })
    }

    /// Launch the fake provider through a link named `name` from now on;
    /// the provider changes behaviour by the name it is started as.
    #[cfg(unix)]
    fn launch_as(&mut self, name: &str) -> TestResult {
        let link = self.directory.path().join(name);
        if !link.exists() {
            std::os::unix::fs::symlink(
                environment_path("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?,
                &link,
            )?;
        }
        self.binary = link.to_string_lossy().into_owned();
        Ok(())
    }

    fn launch_default(&mut self) -> TestResult {
        self.binary = environment_path("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?;
        Ok(())
    }

    fn engine(
        &self,
        resources: &serde_json::Value,
        cancellation: &Cancellation,
    ) -> TestResult<InfrastructureEngine> {
        let infrastructure: Infrastructure = serde_json::from_value(json!({
            "state": {"turso": {"url": "http://unused"}},
            "providers": {"fake": {
                "source": "example.com/test/fake",
                "path": self.binary,
                "configuration": {"directory": self.directory.path().to_string_lossy()},
            }},
            "resources": resources,
        }))?;
        Ok(InfrastructureEngine::new(EngineSetup {
            tenant: self.tenant.clone(),
            store: Arc::clone(&self.store),
            infrastructure,
            options: engine_options(self.directory.path(), cancellation),
        }))
    }

    async fn plan(&self, resources: &serde_json::Value) -> TestResult<Plan> {
        let mut engine = self.engine(resources, &Cancellation::default())?;
        let plan = engine.plan(PlanMode::Apply).await;
        engine.shutdown().await;
        Ok(plan?)
    }

    async fn converge(
        &self,
        resources: &serde_json::Value,
        mode: PlanMode,
    ) -> TestResult<Converged> {
        let mut engine = self.engine(resources, &Cancellation::default())?;
        let lock = self.store.lock(&self.tenant, "fake").await?;
        let planned = engine.plan(mode).await;
        let converged = match planned {
            Ok(plan) => {
                let mut events = Vec::new();
                let applied = engine
                    .apply(&plan, ApplyContext { lock: &lock }, &mut |event| {
                        events.push(event);
                    })
                    .await;
                Ok(Converged {
                    plan,
                    applied,
                    events,
                })
            }
            Err(error) => Err(error),
        };
        self.store.unlock(&self.tenant, &lock).await?;
        engine.shutdown().await;
        Ok(converged?)
    }

    fn set_flag(&self, name: &str) -> TestResult {
        std::fs::write(self.directory.path().join(name), b"")?;
        Ok(())
    }

    fn clear_flag(&self, name: &str) -> TestResult {
        std::fs::remove_file(self.directory.path().join(name))?;
        Ok(())
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.directory.path().join("journal.log")).unwrap_or_default()
    }

    async fn record(&self, name: &str) -> TestResult<cuenv_infrastructure::ManagedResource> {
        Ok(self
            .store
            .list(&self.tenant)
            .await?
            .into_iter()
            .find(|row| row.address.name == name)
            .ok_or_else(|| format!("{name} is not recorded"))?)
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_nested_attributes_and_blocks_with_computed_children_converge() -> TestResult {
    let fake = Fake::new().await?;
    let resources = |labels: &[&str]| {
        let rules: Vec<_> = labels.iter().map(|label| json!({"label": label})).collect();
        json!({"thing": {"type": "fake_nested", "configuration": {
            "name": "nested",
            "rules": rules,
            "rule": [{"label": "block"}],
        }}})
    };
    let created = fake.converge(&resources(&["a"]), PlanMode::Apply).await?;
    created.applied?;
    assert_eq!(created.plan.changes[0].action, Action::Create);
    let record = fake.record("thing").await?;
    assert_eq!(record.state["rules"][0]["rule_id"], "attribute-a");
    assert_eq!(record.state["rule"][0]["rule_id"], "block-block");

    // Computed children are carried from prior state: no perpetual diff.
    let plan = fake.plan(&resources(&["a"])).await?;
    assert!(!plan.has_changes(), "{:?}", actions(&plan));

    let updated = fake
        .converge(&resources(&["a", "b"]), PlanMode::Apply)
        .await?;
    updated.applied?;
    assert_eq!(updated.plan.changes[0].action, Action::Update);
    let record = fake.record("thing").await?;
    assert_eq!(record.state["rules"][1]["rule_id"], "attribute-b");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_json_planned_states_are_applied_as_planned() -> TestResult {
    let fake = Fake::new().await?;
    let resources =
        |name: &str| json!({"thing": {"type": "fake_jsonplan", "configuration": {"name": name}}});
    fake.converge(&resources("one"), PlanMode::Apply)
        .await?
        .applied?;
    let updated = fake.converge(&resources("two"), PlanMode::Apply).await?;
    updated.applied?;
    assert_eq!(updated.plan.changes[0].action, Action::Update);
    let journal = fake.journal();
    assert!(
        journal.contains("planned_state sent as JSON=true"),
        "{journal}"
    );
    assert!(
        !journal.contains("received planned_state null=true"),
        "{journal}"
    );
    assert_eq!(fake.record("thing").await?.state["name"], "two");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_semantically_equal_configuration_is_no_change() -> TestResult {
    let fake = Fake::new().await?;
    let resources =
        |name: &str| json!({"thing": {"type": "fake_semantic", "configuration": {"name": name}}});
    fake.converge(&resources("Hello"), PlanMode::Apply)
        .await?
        .applied?;
    let plan = fake.plan(&resources("HELLO")).await?;
    assert!(!plan.has_changes(), "{:?}", actions(&plan));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_taint_survives_a_failed_delete_of_its_replacement() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_taintdel", "configuration": {"name": "t"}}});

    // The create fails part way: the object is recorded, tainted.
    let failed = fake.converge(&resources, PlanMode::Apply).await?;
    assert!(failed.applied.is_err());
    assert!(fake.record("thing").await?.tainted);

    // Replacing it fails at the delete: it must stay tainted.
    fake.set_flag("create-ok")?;
    fake.set_flag("fail-delete")?;
    let failed = fake.converge(&resources, PlanMode::Apply).await?;
    assert_eq!(failed.plan.changes[0].action, Action::Replace);
    assert!(failed.applied.is_err());
    assert!(fake.record("thing").await?.tainted);

    fake.clear_flag("fail-delete")?;
    let replaced = fake.converge(&resources, PlanMode::Apply).await?;
    replaced.applied?;
    let record = fake.record("thing").await?;
    assert!(!record.tainted);
    assert_eq!(record.state["id"], "taintdel-1");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_json_upgraded_states_are_read_not_dropped() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_plain", "configuration": {"name": "p"}}});
    fake.converge(&resources, PlanMode::Apply).await?.applied?;
    fake.set_flag("upgrade-json")?;
    let plan = fake.plan(&resources).await?;
    assert!(!plan.has_changes(), "{:?}", actions(&plan));
    let journal = fake.journal();
    assert!(
        journal.contains("UpgradeResourceState sent as JSON=true"),
        "{journal}"
    );
    assert!(!journal.contains("current_state null=true"), "{journal}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_deletes_returning_the_object_are_errors_and_keep_it() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_undead", "configuration": {"name": "u"}}});
    fake.converge(&resources, PlanMode::Apply).await?.applied?;
    let destroyed = fake.converge(&resources, PlanMode::Destroy).await?;
    let error = destroyed.applied.err().ok_or("destroy should fail")?;
    assert!(
        error
            .to_string()
            .contains("returned an object after deleting"),
        "{error}"
    );
    assert_eq!(fake.record("thing").await?.state["name"], "u");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_stop_ends_the_operation_in_flight_and_records_it() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_slow", "configuration": {"name": "s"}}});
    let cancellation = Cancellation::default();
    let mut engine = fake.engine(&resources, &cancellation)?;
    let lock = fake.store.lock(&fake.tenant, "fake").await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    let started = std::time::Instant::now();
    let mut ignore_events = |_: cuenv_infrastructure::ApplyEvent| {};
    let (applied, ()) = tokio::join!(
        engine.apply(&plan, ApplyContext { lock: &lock }, &mut ignore_events),
        async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            cancellation.stop();
        }
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(
        matches!(
            applied,
            Err(InfrastructureError::Interrupted {
                completed: 0,
                total: 1
            })
        ),
        "{applied:?}"
    );
    assert!(fake.journal().contains("slow: Create stopped"));
    // Whatever the stopped create returned is recorded, tainted.
    let record = fake.record("thing").await?;
    assert_eq!(record.state["id"], "partial");
    assert!(record.tainted);
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_terminate_kills_providers_at_once() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_slow", "configuration": {"name": "s"}}});
    let cancellation = Cancellation::default();
    let mut engine = fake.engine(&resources, &cancellation)?;
    let lock = fake.store.lock(&fake.tenant, "fake").await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    assert_eq!(cancellation.live_provider_count(), 1);
    let started = std::time::Instant::now();
    let mut ignore_events = |_: cuenv_infrastructure::ApplyEvent| {};
    let (applied, ()) = tokio::join!(
        engine.apply(&plan, ApplyContext { lock: &lock }, &mut ignore_events),
        async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            cancellation.terminate_providers();
        }
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(
        matches!(applied, Err(InfrastructureError::InterruptedUnknownOutcome { ref address, .. })
        if address == "fake_slow.thing"),
        "{applied:?}"
    );
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    assert_eq!(cancellation.live_provider_count(), 0);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_providers_run_in_their_own_process_group() -> TestResult {
    let binary = environment_path("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?;
    let cancellation = Cancellation::default();
    let client = cuenv_infrastructure::plugin::ProviderClient::launch(
        &cuenv_infrastructure::plugin::LaunchOptions {
            binary: Path::new(&binary),
            withheld_environment_variables: &[],
            provider_environment_variables: &BTreeMap::new(),
            cancellation: &cancellation,
        },
    )
    .await?;
    let process = client
        .process_identifier()
        .ok_or("provider has no process")?;
    let stat = std::fs::read_to_string(format!("/proc/{process}/stat"))?;
    // Fields after the parenthesised command name: state, parent, group.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .ok_or("malformed stat")?
        .1
        .split_whitespace()
        .collect();
    assert_eq!(fields.get(2).copied(), Some(process.to_string().as_str()));

    cancellation.terminate_providers();
    client.shutdown().await;
    assert!(!Path::new(&format!("/proc/{process}")).exists());
    Ok(())
}

/// A provider must not outlive cuenv, even one killed with `SIGKILL`: the
/// kernel kills it when the thread that launched it goes away.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
fn fake_providers_die_with_the_thread_that_launched_them() -> TestResult {
    let binary = environment_path("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?;
    let cancellation = Cancellation::default();
    let launcher = cancellation.clone();
    let process = std::thread::spawn(move || -> Result<u32, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async {
            let client = cuenv_infrastructure::plugin::ProviderClient::launch(
                &cuenv_infrastructure::plugin::LaunchOptions {
                    binary: Path::new(&binary),
                    withheld_environment_variables: &[],
                    provider_environment_variables: &BTreeMap::new(),
                    cancellation: &launcher,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
            let process = client.process_identifier().ok_or("no process")?;
            // Neither dropped nor shut down: only the kernel can end it.
            std::mem::forget(client);
            Ok(process)
        })
    })
    .join()
    .map_err(|_| "launcher thread panicked")??;
    let alive = || {
        std::fs::read_to_string(format!("/proc/{process}/stat")).is_ok_and(|stat| {
            !stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    cancellation.remove_socket_directories();
    assert!(!alive(), "provider {process} outlived its launcher");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_destroys_are_planned_and_can_be_refused() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_protect", "configuration": {"name": "p"}}});
    fake.converge(&resources, PlanMode::Apply).await?.applied?;

    // The provider refuses the destroy plan: nothing is deleted, whether
    // destroying or dropping the resource from configuration.
    fake.set_flag("protect")?;
    for (resources, mode) in [
        (&resources, PlanMode::Destroy),
        (&json!({}), PlanMode::Apply),
    ] {
        let error = fake
            .converge(resources, mode)
            .await
            .err()
            .ok_or("a protected destroy must be refused")?;
        assert!(
            error.to_string().contains("deletion protection is enabled"),
            "{error}"
        );
    }
    assert!(!fake.journal().contains("protect: Delete"));
    assert_eq!(fake.record("thing").await?.state["name"], "p");

    // Allowed: the private data of the destroy plan reaches the delete.
    fake.clear_flag("protect")?;
    fake.converge(&resources, PlanMode::Destroy)
        .await?
        .applied?;
    let journal = fake.journal();
    assert!(
        journal.contains(r#"protect: Delete name=p private="planned""#),
        "{journal}"
    );
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_inconsistent_apply_results_are_errors_and_taint_creates() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_drift", "configuration": {"name": "d"}}});
    let created = fake.converge(&resources, PlanMode::Apply).await?;
    let error = created
        .applied
        .err()
        .ok_or("an inconsistent result must fail the apply")?
        .to_string();
    assert!(
        error.contains("inconsistent result after the create of fake_drift.thing"),
        "{error}"
    );
    assert!(error.contains("name: planned value changed"), "{error}");
    assert!(!error.contains("d-drifted"), "{error}");
    // What the provider returned is recorded, tainted, so it is replaced.
    let record = fake.record("thing").await?;
    assert!(record.tainted);
    assert_eq!(record.state["name"], "d-drifted");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_dynamic_values_keep_their_type() -> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_dyn", "configuration": {"name": "x"}}});
    fake.converge(&resources, PlanMode::Apply).await?.applied?;
    // Sent back as the list it is, the value shows no difference.
    let plan = fake.plan(&resources).await?;
    assert!(!plan.has_changes(), "{:?}", actions(&plan));
    let record = fake.record("thing").await?;
    assert_eq!(record.state["data"]["type"], json!(["list", "string"]));
    let journal = fake.journal();
    assert!(
        journal.contains("received data of type basetypes.ListValue"),
        "{journal}"
    );
    assert!(!journal.contains("TupleValue"), "{journal}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_state_from_a_newer_schema_version_is_refused() -> TestResult {
    let mut fake = Fake::new().await?;
    let resources = json!({"thing": {"type": "fake_version", "configuration": {"name": "v"}}});
    fake.launch_as("terraform-provider-fake-v1")?;
    fake.converge(&resources, PlanMode::Apply).await?.applied?;
    assert_eq!(fake.record("thing").await?.schema_version, 1);

    // An older provider must not be handed (and silently downgrade) it.
    fake.launch_default()?;
    let error = fake
        .plan(&resources)
        .await
        .err()
        .ok_or("state from a newer schema must be refused")?
        .to_string();
    assert!(error.contains("resource schema version 1"), "{error}");
    assert!(error.contains("only knows version 0"), "{error}");
    assert!(
        !fake.journal().contains("version: UpgradeResourceState"),
        "the provider was called"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_set_configuration_holds_each_value_once() -> TestResult {
    let fake = Fake::new().await?;
    let resources =
        json!({"thing": {"type": "fake_tags", "configuration": {"tags": ["a", "b", "a"]}}});
    fake.converge(&resources, PlanMode::Apply).await?.applied?;
    assert!(fake.journal().contains("tags: Create with 2 tags"));
    let plan = fake.plan(&resources).await?;
    assert!(!plan.has_changes(), "{:?}", actions(&plan));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_stop_between_the_halves_of_a_replacement_skips_the_create() -> TestResult {
    let fake = Fake::new().await?;
    let resources =
        |name: &str| json!({"thing": {"type": "fake_repl", "configuration": {"name": name}}});
    fake.converge(&resources("a"), PlanMode::Apply)
        .await?
        .applied?;
    fake.set_flag("slow-delete")?;

    let cancellation = Cancellation::default();
    let mut engine = fake.engine(&resources("b"), &cancellation)?;
    let lock = fake.store.lock(&fake.tenant, "fake").await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    assert_eq!(plan.changes[0].action, Action::Replace);
    let mut events = Vec::new();
    let mut collect = |event| events.push(event);
    let (applied, ()) = tokio::join!(
        engine.apply(&plan, ApplyContext { lock: &lock }, &mut collect),
        async {
            // Stop while the delete half is running.
            tokio::time::sleep(Duration::from_secs(1)).await;
            cancellation.stop();
        }
    );
    assert!(
        matches!(
            applied,
            Err(InfrastructureError::Interrupted {
                completed: 0,
                total: 1
            })
        ),
        "{applied:?}"
    );
    let journal = fake.journal();
    assert!(
        journal.contains("repl: Delete name=a finished"),
        "{journal}"
    );
    assert!(!journal.contains("repl: Create name=b"), "{journal}");
    assert_eq!(uncreated_addresses(&events), ["fake_repl.thing"]);
    // The delete is recorded; the next apply creates the replacement.
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Apply order and failure semantics (fake_obj: real objects as files)
// ---------------------------------------------------------------------------

// The helpers between these markers use the error and event types that
// report incomplete applies.
// BEGIN INCOMPLETE APPLY API
fn failed_addresses(error: &InfrastructureError) -> Vec<String> {
    match error {
        InfrastructureError::ApplyIncomplete(incomplete) => incomplete
            .failures
            .iter()
            .map(|failure| failure.address.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

fn skipped_addresses(error: &InfrastructureError) -> Vec<String> {
    match error {
        InfrastructureError::ApplyIncomplete(incomplete) => {
            incomplete.skipped.iter().map(ToString::to_string).collect()
        }
        _ => Vec::new(),
    }
}

fn uncreated_in_error(error: &InfrastructureError) -> Vec<String> {
    match error {
        InfrastructureError::ApplyIncomplete(incomplete) => incomplete
            .deleted_not_recreated
            .iter()
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn uncreated_addresses(events: &[ApplyEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ApplyEvent::DeletedNotRecreated { address } => Some(address.to_string()),
            _ => None,
        })
        .collect()
}
// END INCOMPLETE APPLY API

fn obj(configuration: &serde_json::Value) -> serde_json::Value {
    json!({"type": "fake_obj", "configuration": configuration})
}

fn obj_after(parent: &str, configuration: &serde_json::Value) -> serde_json::Value {
    json!({"type": "fake_obj", "dependsOn": [parent], "configuration": configuration})
}

impl Fake {
    /// The journal lines of the fake_obj operations that changed something.
    fn changes(&self) -> Vec<String> {
        self.journal()
            .lines()
            .filter(|line| {
                ["obj: Create", "obj: Update", "obj: Delete"]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            })
            .map(str::to_string)
            .collect()
    }

    fn clear_journal(&self) -> TestResult {
        std::fs::write(self.directory.path().join("journal.log"), "")?;
        Ok(())
    }

    /// Whether the fake_obj object `key` exists.
    fn exists(&self, key: &str) -> bool {
        self.directory.path().join(format!("obj-{key}")).exists()
    }

    async fn addresses(&self) -> TestResult<Vec<String>> {
        Ok(self
            .store
            .list(&self.tenant)
            .await?
            .into_iter()
            .map(|row| row.address.to_string())
            .collect())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_an_unrelated_failure_does_not_leave_a_replacement_destroyed() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "aaa": obj(&json!({"key": "aaa", "mode": "x"})),
        "zzz": obj(&json!({"key": "zzz", "version": "1"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    fake.set_flag("fail-update-aaa")?;
    let second = json!({
        "aaa": obj(&json!({"key": "aaa", "mode": "y"})),
        "zzz": obj(&json!({"key": "zzz", "version": "2"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let error = run.applied.expect_err("aaa's update fails");
    // zzz does not depend on aaa: it was replaced and recreated.
    assert!(fake.exists("zzz"), "zzz was destroyed and not recreated");
    assert_eq!(fake.record("zzz").await?.state["version"], "2");
    assert_eq!(fake.record("aaa").await?.state["mode"], "x");
    assert_eq!(failed_addresses(&error), ["fake_obj.aaa"]);
    assert!(uncreated_in_error(&error).is_empty());
    assert!(uncreated_addresses(&run.events).is_empty());

    fake.clear_flag("fail-update-aaa")?;
    fake.converge(&second, PlanMode::Apply).await?.applied?;
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_failing_create_does_not_strand_another_replacement() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "aaa": obj(&json!({"key": "aaa", "version": "1"})),
        "zzz": obj(&json!({"key": "zzz", "version": "1"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    fake.set_flag("fail-create-aaa")?;
    let second = json!({
        "aaa": obj(&json!({"key": "aaa", "version": "2"})),
        "zzz": obj(&json!({"key": "zzz", "version": "2"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let error = run.applied.expect_err("aaa's create fails");
    // zzz was replaced in full; aaa is reported as deleted but not recreated.
    assert!(fake.exists("zzz"), "zzz was destroyed and not recreated");
    assert_eq!(fake.record("zzz").await?.state["version"], "2");
    assert!(!fake.exists("aaa"));
    assert_eq!(failed_addresses(&error), ["fake_obj.aaa"]);
    assert_eq!(uncreated_in_error(&error), ["fake_obj.aaa"]);
    assert_eq!(uncreated_addresses(&run.events), ["fake_obj.aaa"]);

    // The next apply creates what is missing.
    fake.clear_flag("fail-create-aaa")?;
    fake.converge(&second, PlanMode::Apply).await?.applied?;
    assert!(fake.exists("aaa"));
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_replacement_whose_new_prerequisite_fails_to_create_is_left_deleted() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(
        &json!({"r": obj(&json!({"key": "R", "version": "1"}))}),
        PlanMode::Apply,
    )
    .await?
    .applied?;
    fake.clear_journal()?;
    fake.set_flag("fail-create-P2")?;
    let second = json!({
        "p2": obj(&json!({"key": "P2"})),
        "r": obj_after("p2", &json!({"key": "R", "version": "2", "parent": "P2"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let error = run.applied.expect_err("p2's create fails");
    // The old object of R goes before the create of its new prerequisite P2,
    // which failed: R is deleted and not recreated, loudly.
    assert!(!fake.exists("R"), "R was not deleted before P2's create");
    assert!(!fake.exists("P2"));
    assert_eq!(failed_addresses(&error), ["fake_obj.p2"]);
    assert!(skipped_addresses(&error).is_empty());
    assert_eq!(uncreated_in_error(&error), ["fake_obj.r"]);
    assert_eq!(uncreated_addresses(&run.events), ["fake_obj.r"]);
    assert!(fake.addresses().await?.is_empty());

    // The next successful apply creates what is missing.
    fake.clear_flag("fail-create-P2")?;
    fake.converge(&second, PlanMode::Apply).await?.applied?;
    assert_eq!(fake.record("r").await?.state["version"], "2");
    assert!(fake.exists("R") && fake.exists("P2"));
    assert!(
        !fake
            .converge(&second, PlanMode::Apply)
            .await?
            .plan
            .has_work()
    );
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_renaming_a_key_with_the_same_identity_deletes_the_old_object_first() -> TestResult {
    let fake = Fake::new().await?;
    fake.converge(&json!({"old": obj(&json!({"key": "K"}))}), PlanMode::Apply)
        .await?
        .applied?;
    fake.clear_journal()?;
    let renamed = json!({"new": obj(&json!({"key": "K"}))});
    let run = fake.converge(&renamed, PlanMode::Apply).await?;
    // The preview is the order of events: the orphan goes first.
    assert_eq!(
        actions(&run.plan),
        vec![
            ("fake_obj.old".to_string(), Action::Delete),
            ("fake_obj.new".to_string(), Action::Create),
        ]
    );
    run.applied?;
    let changes = fake.changes();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes[0].starts_with("obj: Delete key=K"), "{changes:?}");
    assert!(changes[1].starts_with("obj: Create key=K"), "{changes:?}");
    assert!(fake.exists("K"));
    assert_eq!(fake.addresses().await?, ["fake_obj.new"]);
    assert!(
        !fake
            .converge(&renamed, PlanMode::Apply)
            .await?
            .plan
            .has_work()
    );
    fake.converge(&renamed, PlanMode::Destroy).await?.applied?;
    assert!(!fake.exists("K"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_removed_child_is_deleted_before_its_parent_is_updated() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "p": obj(&json!({"key": "P"})),
        "c": obj_after("p", &json!({"key": "C", "parent": "P"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    // The update to "solo" needs a parent without children.
    let second = json!({"p": obj(&json!({"key": "P", "mode": "solo"}))});
    let run = fake.converge(&second, PlanMode::Apply).await?;
    assert_eq!(
        actions(&run.plan),
        vec![
            ("fake_obj.c".to_string(), Action::Delete),
            ("fake_obj.p".to_string(), Action::Update),
        ]
    );
    run.applied?;
    let changes = fake.changes();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes[0].starts_with("obj: Delete key=C"), "{changes:?}");
    assert!(changes[1].starts_with("obj: Update key=P"), "{changes:?}");
    assert_eq!(fake.addresses().await?, ["fake_obj.p"]);
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_vanished_object_is_recreated_under_the_same_generation() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({"a": obj(&json!({"key": "A"}))});
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    let before = fake.record("a").await?;
    std::fs::remove_file(fake.directory.path().join("obj-A"))?;
    let run = fake.converge(&first, PlanMode::Apply).await?;
    assert_eq!(
        actions(&run.plan),
        vec![("fake_obj.a".to_string(), Action::Create)]
    );
    run.applied?;
    let recreated = fake.record("a").await?;
    // The record is the same insertion, rewritten once more.
    assert_eq!(recreated.generation, before.generation);
    assert_eq!(recreated.serial, before.serial + 1);
    // A replacement removes the record and inserts a new one.
    let second = json!({"a": obj(&json!({"key": "A", "version": "2"}))});
    fake.converge(&second, PlanMode::Apply).await?.applied?;
    assert_ne!(fake.record("a").await?.generation, before.generation);
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_replacement_delete_is_sent_the_changes_private_data() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "p": obj(&json!({"key": "P", "version": "1"})),
        "c": obj_after("p", &json!({"key": "C", "parent": "P"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    // `p` is replaced and its dependent `c` removed.
    let second = json!({"p": obj(&json!({"key": "P", "version": "2"}))});
    let run = fake.converge(&second, PlanMode::Apply).await?;
    assert_eq!(
        actions(&run.plan),
        vec![
            ("fake_obj.c".to_string(), Action::Delete),
            ("fake_obj.p".to_string(), Action::Replace),
        ]
    );
    run.applied?;
    assert_eq!(
        fake.changes(),
        vec![
            // A plain delete is sent its destroy plan's private data.
            r#"obj: Delete key=C private="planned-destroy-C""#.to_string(),
            // The delete half of a replacement is sent the private data of
            // the change, which is the create plan's (version 2), not the
            // data refreshed from the old object (version 1).
            r#"obj: Delete key=P private="planned-create-P-v2""#.to_string(),
            "obj: Create key=P parent=".to_string(),
        ]
    );
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_type_change_does_not_create_a_false_dependency_cycle() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "x": obj_after("y", &json!({"key": "X"})),
        "y": obj(&json!({"key": "Y"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    // A child of X blocks its deletion once.
    std::fs::write(
        fake.directory.path().join("obj-blocker"),
        br#"{"parent":"X","mode":""}"#,
    )?;
    // `x` becomes another type (a new address; the old record is an orphan)
    // and `y` now depends on the new `x`.
    let second = json!({
        "x": {"type": "fake_obj2", "configuration": {"key": "X2"}},
        "y": {"type": "fake_obj", "dependsOn": ["x"], "configuration": {"key": "Y"}},
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let error = run.applied.expect_err("the old x cannot be deleted yet");
    assert_eq!(failed_addresses(&error), ["fake_obj.x"]);
    // Dependencies are stored as full addresses, so the two records named x
    // stay distinct.
    assert_eq!(fake.record("y").await?.dependencies, ["fake_obj2.x"]);
    std::fs::remove_file(fake.directory.path().join("obj-blocker"))?;
    // With bare names this destroy saw a cycle between "x" and "y".
    fake.converge(&json!({}), PlanMode::Destroy)
        .await?
        .applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    assert!(!fake.exists("X") && !fake.exists("X2") && !fake.exists("Y"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_dependency_cycle_in_state_does_not_wedge_destroy() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "a": obj(&json!({"key": "A"})),
        "b": obj(&json!({"key": "B"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    // Damage the stored dependencies so that each depends on the other.
    let lock = fake.store.lock(&fake.tenant, "damage").await?;
    for (name, dependency) in [("a", "fake_obj.b"), ("b", "fake_obj.a")] {
        let mut row = fake.record(name).await?;
        row.dependencies = vec![dependency.to_string()];
        fake.store.put(&fake.tenant, &lock, &row).await?;
    }
    fake.store.unlock(&fake.tenant, &lock).await?;
    fake.clear_journal()?;

    // Stored dependencies are history: the cycle is reported as a warning,
    // not refused, so the project can still be destroyed.
    let plan = fake.plan(&json!({})).await?;
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("cycle")),
        "{:?}",
        plan.warnings
    );
    let run = fake.converge(&json!({}), PlanMode::Destroy).await?;
    run.applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    assert!(!fake.exists("A") && !fake.exists("B"));
    Ok(())
}

/// The scenario of the rename tests: `old` becomes `new` (the same real
/// object, key K) and `r`, which depended on `old`, follows. `after` is the
/// configuration `r` has once renamed.
async fn rename_with_a_dependent(after: serde_json::Value) -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "old": obj(&json!({"key": "K"})),
        "r": obj_after("old", &json!({"key": "R", "version": "1"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    let renamed = json!({
        "new": obj(&json!({"key": "K"})),
        "r": obj_after("new", &after),
    });
    let run = fake.converge(&renamed, PlanMode::Apply).await?;
    let plan = actions(&run.plan);
    run.applied
        .map_err(|error| format!("{error}; plan {plan:?}; journal {:?}", fake.changes()))?;
    // The old object is deleted before the new one takes its identity.
    let changes = fake.changes();
    let position = |prefix: &str| changes.iter().position(|line| line.starts_with(prefix));
    let deleted = position("obj: Delete key=K").ok_or("K was never deleted")?;
    let created = position("obj: Create key=K").ok_or("K was never created")?;
    assert!(deleted < created, "{changes:?}");
    assert!(fake.exists("K"), "the object both resources name is gone");
    assert_eq!(fake.addresses().await?, ["fake_obj.new", "fake_obj.r"]);
    assert_eq!(fake.record("r").await?.dependencies, ["fake_obj.new"]);
    assert!(
        !fake
            .converge(&renamed, PlanMode::Apply)
            .await?
            .plan
            .has_work()
    );
    fake.converge(&renamed, PlanMode::Destroy).await?.applied?;
    assert!(!fake.exists("K") && !fake.exists("R"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_renaming_a_resource_whose_dependent_only_follows_deletes_the_old_one_first()
-> TestResult {
    rename_with_a_dependent(json!({"key": "R", "version": "1"})).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_renaming_a_resource_whose_dependent_is_updated_deletes_the_old_one_first()
-> TestResult {
    rename_with_a_dependent(json!({"key": "R", "version": "1", "mode": "m"})).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_renaming_a_resource_whose_dependent_is_replaced_deletes_the_old_one_first()
-> TestResult {
    rename_with_a_dependent(json!({"key": "R", "version": "2"})).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_type_change_with_a_dependent_deletes_the_old_object_first() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "x": obj(&json!({"key": "X"})),
        "y": obj_after("x", &json!({"key": "Y"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.clear_journal()?;
    // `x` keeps its key but becomes another type: a new address, and the old
    // one an orphan, over the same real object.
    let second = json!({
        "x": {"type": "fake_obj2", "configuration": {"key": "X"}},
        "y": obj_after("x", &json!({"key": "Y"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let plan = actions(&run.plan);
    run.applied
        .map_err(|error| format!("{error}; plan {plan:?}; journal {:?}", fake.changes()))?;
    let changes = fake.changes();
    assert!(changes[0].starts_with("obj: Delete key=X"), "{changes:?}");
    assert!(changes[1].starts_with("obj: Create key=X"), "{changes:?}");
    assert!(fake.exists("X"));
    assert_eq!(fake.addresses().await?, ["fake_obj.y", "fake_obj2.x"]);
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_partial_failure_leaves_no_dependency_cycle_that_wedges_destroy() -> TestResult {
    let fake = Fake::new().await?;
    let first = json!({
        "x": obj_after("y", &json!({"key": "X"})),
        "y": obj(&json!({"key": "Y"})),
        "z": obj(&json!({"key": "Z"})),
    });
    fake.converge(&first, PlanMode::Apply).await?.applied?;
    fake.set_flag("fail-update-Z")?;
    // x now depends on z and y on x: swapping y and z around x. z's update
    // fails, so x cannot be rewritten, so y must not be either.
    let second = json!({
        "x": obj_after("z", &json!({"key": "X"})),
        "y": obj_after("x", &json!({"key": "Y", "mode": "m"})),
        "z": obj(&json!({"key": "Z", "mode": "m"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let error = run.applied.expect_err("z's update fails");
    assert_eq!(failed_addresses(&error), ["fake_obj.z"]);
    let mut skipped = skipped_addresses(&error);
    skipped.sort();
    assert_eq!(skipped, ["fake_obj.x", "fake_obj.y"]);
    assert_eq!(fake.record("x").await?.dependencies, ["fake_obj.y"]);
    assert!(fake.record("y").await?.dependencies.is_empty());

    // The project can still be destroyed.
    fake.converge(&json!({}), PlanMode::Destroy)
        .await?
        .applied?;
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_stopped_replacement_waits_only_for_its_own_prerequisites() -> TestResult {
    let fake = Fake::new().await?;
    let versions = |version: &str, mode: &str| {
        json!({
            "p": obj(&json!({"key": "P", "mode": mode})),
            "x": obj_after("p", &json!({"key": "X", "version": version})),
            "y": obj(&json!({"key": "Y", "version": version})),
        })
    };
    fake.converge(&versions("1", ""), PlanMode::Apply)
        .await?
        .applied?;
    fake.clear_journal()?;
    // x and y are replaced and p updated. x's create needs p's update, which
    // needs x's delete. After x's delete the run stops at the next change:
    // that must be p's update (x's prerequisite), not y's unrelated delete.
    let cancellation = Cancellation::default();
    let mut engine = fake.engine(&versions("2", "m"), &cancellation)?;
    let lock = fake.store.lock(&fake.tenant, "fake").await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    let mut events = Vec::new();
    let mut started = 0;
    let stopper = cancellation.clone();
    let applied = engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |event| {
            if matches!(event, ApplyEvent::Started { .. }) {
                started += 1;
                if started == 2 {
                    stopper.stop();
                }
            }
            events.push(event);
        })
        .await;
    assert!(
        matches!(applied, Err(InfrastructureError::Interrupted { .. })),
        "{applied:?}"
    );
    let changes = fake.changes();
    assert!(
        changes.iter().all(|line| !line.contains("key=Y")),
        "y was touched before x's prerequisite: {changes:?}"
    );
    assert!(
        changes
            .iter()
            .any(|line| line.starts_with("obj: Update key=P")),
        "{changes:?}"
    );
    assert_eq!(uncreated_addresses(&events), ["fake_obj.x"]);
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    // The next apply finishes the job.
    fake.converge(&versions("2", "m"), PlanMode::Apply)
        .await?
        .applied?;
    assert!(fake.exists("X") && fake.exists("Y"));
    fake.converge(&versions("2", "m"), PlanMode::Destroy)
        .await?
        .applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_replacement_deleted_but_unrecorded_is_reported_deleted_not_recreated() -> TestResult
{
    let fake = Fake::new().await?;
    let versions = |version: &str| json!({"x": obj(&json!({"key": "X", "version": version}))});
    fake.converge(&versions("1"), PlanMode::Apply)
        .await?
        .applied?;
    fake.clear_journal()?;
    // The lock is lost while the delete runs: the provider deletes the
    // object, and recording that fails.
    let mut engine = fake.engine(&versions("2"), &Cancellation::default())?;
    let lock = fake.store.lock(&fake.tenant, "fake").await?;
    let plan = engine.plan(PlanMode::Apply).await?;
    let mut events = Vec::new();
    let store = Arc::clone(&fake.store);
    let tenant = fake.tenant.clone();
    let applied = engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |event| {
            if matches!(event, ApplyEvent::Started { .. }) {
                let store = Arc::clone(&store);
                let tenant = tenant.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async move {
                        if let Ok(Some(current)) = store.current_lock(&tenant).await {
                            let _ = store.force_unlock(&tenant, &current.lock_identifier).await;
                        }
                    });
                });
            }
            events.push(event);
        })
        .await;
    engine.shutdown().await;
    assert!(applied.is_err(), "{applied:?}");
    assert!(!fake.exists("X"), "the provider deleted the object");
    assert_eq!(uncreated_addresses(&events), ["fake_obj.x"], "{events:?}");
    // The next apply creates it again.
    fake.converge(&versions("2"), PlanMode::Apply)
        .await?
        .applied?;
    assert!(fake.exists("X"));
    fake.converge(&versions("2"), PlanMode::Destroy)
        .await?
        .applied?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_a_tainted_replacement_plans_its_create_with_the_old_objects_private_data()
-> TestResult {
    let fake = Fake::new().await?;
    let resources = json!({"t": obj(&json!({"key": "T", "version": "1"}))});
    fake.set_flag("taint-create-T")?;
    let failed = fake.converge(&resources, PlanMode::Apply).await?;
    assert!(failed.applied.is_err());
    let tainted = fake.record("t").await?;
    assert!(tainted.tainted);
    assert!(
        !tainted.private.is_empty(),
        "the record holds no private data"
    );
    fake.clear_flag("taint-create-T")?;
    fake.clear_journal()?;
    let replaced = fake.converge(&resources, PlanMode::Apply).await?;
    assert_eq!(replaced.plan.changes[0].action, Action::Replace);
    replaced.applied?;
    // Terraform hands the create plan the private data of the object it
    // replaces.
    let journal = fake.journal();
    assert!(
        journal.contains(r#"obj: PlanCreate key=T priorPrivate="created-T""#),
        "{journal}"
    );
    fake.converge(&resources, PlanMode::Destroy)
        .await?
        .applied?;
    Ok(())
}

/// The state and the project directory a run with the local provider acts on.
struct LocalProject<'project> {
    store: &'project Arc<dyn StateStore>,
    tenant: &'project TenantKey,
    directory: &'project Path,
}

/// Plan and apply `resources` with the local provider.
async fn local_converge(
    local: &LocalProject<'_>,
    resources: serde_json::Value,
    mode: PlanMode,
) -> TestResult<Converged> {
    let LocalProject {
        store,
        tenant,
        directory: project,
    } = local;
    let infrastructure: Infrastructure = serde_json::from_value(json!({
        "state": {"turso": {"url": "http://unused"}},
        "providers": {"local": {
            "source": "hashicorp/local",
            "path": environment_path("CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER")?,
        }},
        "resources": resources,
    }))?;
    let mut engine = InfrastructureEngine::new(EngineSetup {
        tenant: (*tenant).clone(),
        store: Arc::clone(store),
        infrastructure,
        options: engine_options(project, &Cancellation::default()),
    });
    let lock = store.lock(tenant, "local rename").await?;
    let plan = engine.plan(mode).await?;
    let applied = engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
        .await;
    store.unlock(tenant, &lock).await?;
    engine.shutdown().await;
    Ok(Converged {
        plan,
        applied,
        events: Vec::new(),
    })
}

/// Renaming a `local_file` that has a dependent (the dependent's `dependsOn`
/// follows the rename) must keep the file both resources manage.
async fn local_file_rename_with_a_dependent(reader_after: &str) -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = store().await?;
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/local-rename-dependent-{run}"), "web")?;
    let _cleanup = TenantCleanup::new(&tenant);
    let file = directory.path().join("site.conf");
    let reader = directory.path().join("reader.conf");
    let local_file = |path: &Path, content: &str, depends: &[&str]| {
        json!({"type": "local_file", "dependsOn": depends, "configuration": {
            "filename": path.to_string_lossy(),
            "content": content,
        }})
    };
    let before = json!({
        "old": local_file(&file, "hello", &[]),
        "reader": local_file(&reader, "r1", &["old"]),
    });
    let local = LocalProject {
        store: &store,
        tenant: &tenant,
        directory: directory.path(),
    };
    local_converge(&local, before, PlanMode::Apply)
        .await?
        .applied?;
    let after = json!({
        "new": local_file(&file, "hello", &[]),
        "reader": local_file(&reader, reader_after, &["new"]),
    });
    let renamed = local_converge(&local, after.clone(), PlanMode::Apply).await?;
    let plan = actions(&renamed.plan);
    renamed
        .applied
        .map_err(|error| format!("{error}: {plan:?}"))?;
    assert!(
        file.exists(),
        "the rename deleted the file it manages: {plan:?}"
    );
    assert_eq!(std::fs::read_to_string(&file)?, "hello");
    assert_eq!(std::fs::read_to_string(&reader)?, reader_after);
    let rows = store.list(&tenant).await?;
    assert_eq!(
        rows.iter()
            .map(|row| row.address.to_string())
            .collect::<Vec<_>>(),
        ["local_file.new", "local_file.reader"]
    );
    assert!(
        !local_converge(&local, after, PlanMode::Apply)
            .await?
            .plan
            .has_work()
    );
    local_converge(&local, json!({}), PlanMode::Destroy)
        .await?
        .applied?;
    assert!(!file.exists() && !reader.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the local provider (CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER)"]
async fn local_file_rename_with_a_following_dependent_keeps_the_file() -> TestResult {
    local_file_rename_with_a_dependent("r1").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the local provider (CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER)"]
async fn local_file_rename_with_a_replaced_dependent_keeps_the_file() -> TestResult {
    local_file_rename_with_a_dependent("r2").await
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the local provider (CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER)"]
async fn local_file_rename_keeps_the_file_both_resources_manage() -> TestResult {
    let directory = tempfile::tempdir()?;
    let store = store().await?;
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/local-rename-{run}"), "web")?;
    let _cleanup = TenantCleanup::new(&tenant);
    let file = directory.path().join("greeting.txt");
    let resources = |name: &str| {
        json!({name: {"type": "local_file", "configuration": {
            "filename": file.to_string_lossy(),
            "content": "hi",
        }}})
    };
    let converge = |resources: serde_json::Value, mode: PlanMode| {
        let store = Arc::clone(&store);
        let tenant = tenant.clone();
        let project = directory.path().to_path_buf();
        async move {
            let infrastructure: Infrastructure = serde_json::from_value(json!({
                "state": {"turso": {"url": "http://unused"}},
                "providers": {"local": {
                    "source": "hashicorp/local",
                    "path": environment_path("CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER")?,
                }},
                "resources": resources,
            }))?;
            let mut engine = InfrastructureEngine::new(EngineSetup {
                tenant: tenant.clone(),
                store: Arc::clone(&store),
                infrastructure,
                options: engine_options(&project, &Cancellation::default()),
            });
            let lock = store.lock(&tenant, "local rename").await?;
            let plan = engine.plan(mode).await?;
            let applied = engine
                .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
                .await;
            store.unlock(&tenant, &lock).await?;
            engine.shutdown().await;
            TestResult::Ok(Converged {
                plan,
                applied,
                events: Vec::new(),
            })
        }
    };
    converge(resources("old"), PlanMode::Apply).await?.applied?;
    assert_eq!(std::fs::read_to_string(&file)?, "hi");

    // Same file, new key: the old resource is deleted before the new one is
    // created, so the file the two share still exists afterwards.
    let renamed = converge(resources("new"), PlanMode::Apply).await?;
    assert_eq!(
        actions(&renamed.plan),
        vec![
            ("local_file.old".to_string(), Action::Delete),
            ("local_file.new".to_string(), Action::Create),
        ]
    );
    renamed.applied?;
    assert!(file.exists(), "the rename deleted the file it manages");
    assert_eq!(std::fs::read_to_string(&file)?, "hi");
    let rows = store.list(&tenant).await?;
    assert_eq!(
        rows.iter()
            .map(|row| row.address.to_string())
            .collect::<Vec<_>>(),
        ["local_file.new"]
    );
    converge(json!({}), PlanMode::Destroy).await?.applied?;
    assert!(!file.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider (CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER)"]
async fn fake_an_identity_handoff_deletes_the_old_object_before_the_new_one_takes_its_identity()
-> TestResult {
    let fake = Fake::new().await?;
    // `a` holds the object K.
    fake.converge(&json!({"a": obj(&json!({"key": "K"}))}), PlanMode::Apply)
        .await?
        .applied?;
    fake.clear_journal()?;
    // `c` takes K over, and `a` is replaced to hold another object, after `c`.
    let second = json!({
        "c": obj(&json!({"key": "K"})),
        "a": obj_after("c", &json!({"key": "A2"})),
    });
    let run = fake.converge(&second, PlanMode::Apply).await?;
    let plan = actions(&run.plan);
    assert!(plan.contains(&("fake_obj.a".to_string(), Action::Replace)));
    assert!(plan.contains(&("fake_obj.c".to_string(), Action::Create)));
    run.applied?;
    // The old object of `a` went first; creating `c` before it would fail with
    // "already exists", or lose K to the delete.
    let changes = fake.changes();
    assert_eq!(changes.len(), 3, "{changes:?}");
    assert!(changes[0].starts_with("obj: Delete key=K"), "{changes:?}");
    assert!(changes[1].starts_with("obj: Create key=K"), "{changes:?}");
    assert!(changes[2].starts_with("obj: Create key=A2"), "{changes:?}");
    assert!(fake.exists("K") && fake.exists("A2"));
    assert_eq!(fake.addresses().await?, ["fake_obj.a", "fake_obj.c"]);
    assert_eq!(fake.record("c").await?.state["key"], "K");
    assert!(
        !fake
            .converge(&second, PlanMode::Apply)
            .await?
            .plan
            .has_work()
    );
    fake.converge(&second, PlanMode::Destroy).await?.applied?;
    assert!(!fake.exists("K") && !fake.exists("A2"));
    Ok(())
}
