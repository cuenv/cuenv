//! End-to-end lifecycle test against real Terraform provider binaries.
//!
//! Ignored by default because it needs provider executables (and,
//! optionally, a libSQL server). Run with:
//!
//! ```text
//! CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER=/path/terraform-provider-random_v3.7.2_x5 \
//! CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER=/path/terraform-provider-local_v2.9.1_x5 \
//! CUENV_INFRASTRUCTURE_TEST_TURSO_URL=http://127.0.0.1:8080 \
//! cargo test -p cuenv-infrastructure --test provider_end_to_end -- --ignored --nocapture
//! ```
//!
//! Without `CUENV_INFRASTRUCTURE_TEST_TURSO_URL` the in-memory store is used.

use std::path::Path;
use std::sync::Arc;

use cuenv_infrastructure::{
    Action, EngineOptions, InfrastructureEngine, MemoryStateStore, Plan, PlanMode, StateStore,
    TenantKey, TursoConfiguration, TursoStateStore,
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

async fn plan_and_apply(
    store: &Arc<dyn StateStore>,
    tenant: &TenantKey,
    desired: &Desired<'_>,
    mode: PlanMode,
) -> TestResult<Plan> {
    let mut engine = InfrastructureEngine::new(
        tenant.clone(),
        Arc::clone(store),
        infrastructure(desired)?,
        EngineOptions {
            project_directory: std::env::temp_dir(),
            plugin_cache_directory: None,
        },
    );
    let lock = store.lock(tenant, "provider_end_to_end").await?;
    let plan = engine.plan(mode).await?;
    engine.apply(&plan, &mut |_| {}).await?;
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
    let tenant = TenantKey::new(format!("example.com/end-to-end-{run}@v0"), "web").unwrap();
    let neighbour = TenantKey::new(format!("example.com/end-to-end-{run}"), "api").unwrap();

    // Create both resources, dependency first.
    let plan = plan_and_apply(&store, &tenant, &desired(Some("hello"), 2), PlanMode::Apply).await?;
    assert_eq!(
        actions(&plan),
        vec![
            ("random_pet.pet".to_string(), Action::Create),
            ("local_file.greeting".to_string(), Action::Create),
        ]
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
    let rows = store.list(&tenant).await.unwrap();
    assert_eq!(rows.len(), 2);
    let pet = rows.iter().find(|row| row.address.name == "pet").unwrap();
    assert_eq!(
        pet.provider_source,
        "registry.terraform.io/hashicorp/random"
    );
    let pet_identifier = pet.state["id"].as_str().unwrap().to_string();
    assert_eq!(pet_identifier.split('-').count(), 2);

    // The same project name under another module sees nothing.
    assert!(store.list(&neighbour).await.unwrap().is_empty());

    // Re-planning unchanged configuration is a no-op.
    let plan = plan_and_apply(&store, &tenant, &desired(Some("hello"), 2), PlanMode::Apply).await?;
    assert!(
        !plan.has_changes(),
        "expected no changes: {:?}",
        actions(&plan)
    );

    // Changing file content forces replacement (local_file is immutable).
    let plan = plan_and_apply(&store, &tenant, &desired(Some("world"), 2), PlanMode::Apply).await?;
    assert_eq!(
        actions(&plan),
        vec![
            ("random_pet.pet".to_string(), Action::NoOp),
            ("local_file.greeting".to_string(), Action::Replace),
        ]
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "world");

    // Dropping a resource from configuration deletes it.
    let plan = plan_and_apply(&store, &tenant, &desired(None, 3), PlanMode::Apply).await?;
    assert_eq!(
        actions(&plan),
        vec![
            ("local_file.greeting".to_string(), Action::Delete),
            ("random_pet.pet".to_string(), Action::Replace),
        ]
    );
    assert!(!file.exists());
    let rows = store.list(&tenant).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].state["id"].as_str().unwrap(), pet_identifier);

    // Destroy removes everything the tenant owns.
    let plan = plan_and_apply(&store, &tenant, &desired(None, 3), PlanMode::Destroy).await?;
    assert_eq!(
        actions(&plan),
        vec![("random_pet.pet".to_string(), Action::Delete)]
    );
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a protocol 6 provider binary (CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER)"]
async fn protocol_6_provider_schema() {
    let binary = environment_path("CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER").unwrap();
    let client = cuenv_infrastructure::plugin::ProviderClient::launch(Path::new(&binary))
        .await
        .unwrap();
    assert_eq!(
        client.protocol(),
        cuenv_infrastructure::plugin::Protocol::Version6
    );
    let (schema, _) = client.schema().await.unwrap();
    let organization = schema
        .resources
        .get("tfe_organization")
        .expect("tfe_organization");
    assert!(organization.block.attributes.contains_key("email"));
    client.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads from registry.terraform.io"]
async fn installs_provider_from_registry() {
    let cache = tempfile::tempdir().unwrap();
    let installer =
        cuenv_infrastructure::registry::ProviderInstaller::new(cache.path().to_path_buf()).unwrap();
    let source = cuenv_infrastructure::registry::ProviderSource::parse("hashicorp/random").unwrap();
    let binary = installer.ensure(&source, "3.7.2").await.unwrap();
    assert!(binary.starts_with(cache.path()));
    // Second call is served from the cache.
    assert_eq!(installer.ensure(&source, "3.7.2").await.unwrap(), binary);
    let client = cuenv_infrastructure::plugin::ProviderClient::launch(&binary)
        .await
        .unwrap();
    assert_eq!(
        client.protocol(),
        cuenv_infrastructure::plugin::Protocol::Version5
    );
    client.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a libSQL server (CUENV_INFRASTRUCTURE_TEST_TURSO_URL)"]
async fn turso_store_round_trips_records() {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")
        .expect("CUENV_INFRASTRUCTURE_TEST_TURSO_URL");
    let store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })
    .unwrap();
    store.migrate().await.unwrap();
    let run = uuid::Uuid::new_v4().to_string();
    let tenant = TenantKey::new(format!("example.com/store-{run}"), "web").unwrap();
    let record = cuenv_infrastructure::ManagedResource {
        address: cuenv_infrastructure::ResourceAddress::new("random_pet", "pet"),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 2,
        state: json!({"id": "happy-otter", "nested": {"list": [1, 2]}}),
        // Lengths 1..=3 exercise every base64 padding case.
        private: vec![0, 255, 7, 42, 1],
        dependencies: vec!["other".into()],
    };
    store.put(&tenant, &record).await.unwrap();
    let mut updated = record.clone();
    updated.private = vec![9];
    store.put(&tenant, &updated).await.unwrap();
    assert_eq!(store.list(&tenant).await.unwrap(), vec![updated.clone()]);
    store.delete(&tenant, &updated.address).await.unwrap();
    assert!(store.list(&tenant).await.unwrap().is_empty());
}
