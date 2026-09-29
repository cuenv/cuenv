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
    Action, ApplyContext, Cancellation, ConditionalPut, EngineOptions, EngineSetup,
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
    assert_eq!(
        actions(&plan),
        vec![
            ("random_pet.pet".to_string(), Action::NoOp),
            ("local_file.greeting".to_string(), Action::Replace),
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
async fn turso_store_round_trips_records() -> TestResult {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?;
    let store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })?;
    store.migrate().await?;
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/store-{run}"), "web")?;
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
                expected: RecordVersion::Serial(1),
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
                expected: RecordVersion::Serial(2),
            },
        )
        .await?;
    assert_eq!(store.list(&tenant).await?[0].serial, 3);
    let absent = cuenv_infrastructure::ManagedResource {
        address: cuenv_infrastructure::ResourceAddress::new("random_pet", "other"),
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

/// One tenant driven through the fake provider, with its own journal and
/// flag directory.
struct Fake {
    directory: tempfile::TempDir,
    store: Arc<dyn StateStore>,
    tenant: TenantKey,
    /// The provider executable the engines launch.
    binary: String,
}

/// The outcome of one plan and apply.
struct Converged {
    plan: Plan,
    applied: Result<PlanSummary, InfrastructureError>,
}

impl Fake {
    async fn new() -> TestResult<Self> {
        let run = uuid::Uuid::new_v4().to_string();
        Ok(Self {
            directory: tempfile::tempdir()?,
            store: store().await?,
            tenant: TenantKey::new(format!("example.com/fake-{run}"), "web")?,
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
                let applied = engine
                    .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
                    .await;
                Ok(Converged { plan, applied })
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
    assert!(applied.is_err());
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
    let mut warnings = Vec::new();
    let mut collect = |event| {
        if let cuenv_infrastructure::ApplyEvent::Warning(warning) = event {
            warnings.push(warning);
        }
    };
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
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("replacement was not created")),
        "{warnings:?}"
    );
    // The delete is recorded; the next apply creates the replacement.
    assert!(fake.store.list(&fake.tenant).await?.is_empty());
    fake.store.unlock(&fake.tenant, &lock).await?;
    engine.shutdown().await;
    Ok(())
}
