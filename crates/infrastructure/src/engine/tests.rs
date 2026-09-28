use super::*;
use crate::state::MemoryStateStore;
use serde_json::json;

fn graph(edges: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    edges
        .iter()
        .map(|(name, dependencies)| {
            (
                (*name).to_owned(),
                dependencies
                    .iter()
                    .map(|dependency| (*dependency).to_owned())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn topological_order_puts_dependencies_first() {
    let order = topological_order(&graph(&[
        ("network", &["subnet"]),
        ("subnet", &["account"]),
        ("account", &[]),
        ("unrelated", &[]),
    ]))
    .unwrap();
    let position = |name: &str| order.iter().position(|entry| entry == name).unwrap();
    assert!(position("account") < position("subnet"));
    assert!(position("subnet") < position("network"));
    assert_eq!(order.len(), 4);
}

fn record(resource_type: &str, name: &str, dependencies: &[&str]) -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new(resource_type, name),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 0,
        state: json!({"id": "stored", "secret": "hunter2"}),
        private: Vec::new(),
        dependencies: dependencies
            .iter()
            .map(|dependency| (*dependency).to_owned())
            .collect(),
        tainted: false,
        identity: None,
    }
}

#[test]
fn orphan_graph_keys_by_address_and_ignores_declared_dependencies() {
    let pet = record("random_pet", "shared", &[]);
    let identifier = record("random_id", "shared", &["shared", "still_declared"]);
    let graph = orphan_graph(&[&pet, &identifier]);
    assert_eq!(graph.len(), 2);
    assert_eq!(
        graph["random_id.shared"],
        vec!["random_pet.shared".to_string()]
    );
    assert!(graph["random_pet.shared"].is_empty());
    assert!(topological_order(&graph).is_ok());
}

#[test]
fn topological_order_rejects_cycles_and_unknown_dependencies() {
    assert!(topological_order(&graph(&[("first", &["second"]), ("second", &["first"])])).is_err());
    assert!(topological_order(&graph(&[("first", &["missing"])])).is_err());
}

#[test]
fn diagnostics_split_errors_from_warnings() {
    let mut warnings = Vec::new();
    let warning = Diagnostic {
        severity: Severity::Warning as i32,
        summary: "deprecated".into(),
        detail: String::new(),
        attribute: None,
    };
    check_diagnostics("context", std::slice::from_ref(&warning), &mut warnings).unwrap();
    assert_eq!(warnings, vec!["context: deprecated".to_string()]);

    let error = Diagnostic {
        severity: Severity::Error as i32,
        summary: "bad".into(),
        detail: "value too long".into(),
        attribute: Some(protocol_path(&[
            protocol::Selector::AttributeName("rules".into()),
            protocol::Selector::ElementKeyInt(0),
        ])),
    };
    let failure = check_diagnostics("context", &[warning, error], &mut warnings).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("bad: value too long (at rules[0])"),
        "{failure}"
    );
}

fn protocol_path(selectors: &[protocol::Selector]) -> protocol::AttributePath {
    protocol::AttributePath {
        steps: selectors
            .iter()
            .map(|selector| protocol::AttributePathStep {
                selector: Some(selector.clone()),
            })
            .collect(),
    }
}

fn change(address: ResourceAddress, action: Action) -> ResourceChange {
    ResourceChange {
        address,
        provider: "random".into(),
        action,
        before: Value::Null,
        after: Value::Null,
        sensitive: Vec::new(),
        requires_replace: Vec::new(),
        dependencies: Vec::new(),
        steps: Vec::new(),
        refreshed_record: None,
        stored: None,
    }
}

fn tenant() -> TenantKey {
    TenantKey::new("example.com/app", "web").unwrap()
}

fn plan_of(changes: Vec<ResourceChange>) -> Plan {
    Plan {
        tenant: tenant(),
        changes,
        warnings: Vec::new(),
    }
}

#[test]
fn render_plan_masks_sensitive_values() {
    let mut after = BTreeMap::new();
    after.insert("id".to_string(), Value::Unknown);
    after.insert("secret".to_string(), Value::String("hunter2".into()));
    let mut create = change(
        ResourceAddress::new("random_password", "database"),
        Action::Create,
    );
    create.after = Value::Object(after);
    create.sensitive = vec!["secret".into()];
    let text = render_plan(&plan_of(vec![create]));
    assert!(
        text.contains("+ random_password.database (create)"),
        "{text}"
    );
    assert!(text.contains("+ id = (known after apply)"), "{text}");
    assert!(text.contains("+ secret = (sensitive)"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("Plan: 1 to create"), "{text}");
}

fn refresh_only(name: &str) -> ResourceChange {
    let stored = record("random_pet", name, &[]);
    let mut refreshed = stored.clone();
    refreshed.state = json!({"id": "refreshed"});
    let mut unchanged = change(stored.address.clone(), Action::NoOp);
    unchanged.stored = Some(stored);
    unchanged.refreshed_record = Some(refreshed);
    unchanged
}

#[test]
fn refresh_only_records_count_as_work_but_not_as_changes() {
    let plan = plan_of(vec![
        refresh_only("pet"),
        change(ResourceAddress::new("random_pet", "idle"), Action::NoOp),
    ]);
    let summary = plan.summary();
    assert_eq!(summary.refresh, 1);
    assert_eq!(summary.unchanged, 1);
    assert!(!plan.has_changes());
    assert!(plan.has_work());
    assert!(plan.changes[0].refreshes_state());
    let text = render_plan(&plan);
    assert!(
        text.contains("random_pet.pet (refresh stored state"),
        "{text}"
    );
    assert!(text.contains("1 to refresh, 1 unchanged"), "{text}");

    let idle = plan_of(vec![change(
        ResourceAddress::new("random_pet", "idle"),
        Action::NoOp,
    )]);
    assert!(!idle.has_work());
}

#[test]
fn digest_covers_values_replacement_paths_and_stored_records() {
    let base = || {
        let mut update = change(ResourceAddress::new("random_pet", "pet"), Action::Update);
        update.before = Value::String("old".into());
        update.after = Value::String("new".into());
        update.stored = Some(record("random_pet", "pet", &[]));
        plan_of(vec![update])
    };
    assert_eq!(base().digest(), base().digest());
    assert_eq!(base().digest().as_str().len(), 64);

    let mut different_after = base();
    different_after.changes[0].after = Value::String("newer".into());
    assert_ne!(base().digest(), different_after.digest());

    let mut unknown_after = base();
    unknown_after.changes[0].after = Value::Unknown;
    let mut literal_after = base();
    literal_after.changes[0].after = Value::String("(known after apply)".into());
    assert_ne!(unknown_after.digest(), literal_after.digest());

    let mut replaced = base();
    replaced.changes[0].requires_replace = vec!["length".into()];
    assert_ne!(base().digest(), replaced.digest());

    // Another run wrote the stored record in between.
    let mut rewritten = base();
    if let Some(stored) = rewritten.changes[0].stored.as_mut() {
        stored.private = vec![1];
    }
    assert_ne!(base().digest(), rewritten.digest());

    let mut different_action = base();
    different_action.changes[0].action = Action::Replace;
    assert_ne!(base().digest(), different_action.digest());
}

#[test]
fn apply_results_are_checked_like_terraform() {
    let address = ResourceAddress::new("random_pet", "pet");
    let object = Value::Object(BTreeMap::from([(
        "id".to_string(),
        Value::String("x".into()),
    )]));
    let with_unknown = Value::Object(BTreeMap::from([("id".to_string(), Value::Unknown)]));
    let problem = |kind, returned: &Value, failed| {
        apply_result_problem(&ApplyResult {
            address: &address,
            kind,
            returned,
            failed,
        })
    };
    assert!(problem(StepKind::Create, &with_unknown, false).is_some());
    assert!(problem(StepKind::Create, &with_unknown, true).is_some());
    assert!(problem(StepKind::Delete, &object, false).is_some());
    assert!(problem(StepKind::Delete, &object, true).is_none());
    assert!(problem(StepKind::Delete, &Value::Null, false).is_none());
    assert!(problem(StepKind::Create, &Value::Null, false).is_some());
    assert!(problem(StepKind::Update, &Value::Null, false).is_some());
    assert!(problem(StepKind::Update, &Value::Null, true).is_none());
    assert!(problem(StepKind::Update, &object, false).is_none());
}

fn object_type() -> Type {
    Type::from_json(&json!(["object", {
        "tags": ["set", "string"],
        "length": "number",
        "name": "string",
    }]))
    .unwrap()
}

fn value(json: &serde_json::Value) -> Value {
    Value::from_configuration_json(json, &object_type()).unwrap()
}

#[test]
fn replacement_paths_compare_with_schema_types() {
    let address = ResourceAddress::new("example_thing", "one");
    let prior = value(&json!({"tags": ["a", "b"], "length": 1, "name": "x"}));
    let planned = value(&json!({"tags": ["b", "a"], "length": 1.0, "name": "y"}));
    let paths = [
        protocol_path(&[protocol::Selector::AttributeName("tags".into())]),
        protocol_path(&[protocol::Selector::AttributeName("length".into())]),
        protocol_path(&[protocol::Selector::AttributeName("name".into())]),
    ];
    let changed = replacement_paths(&ReplacementCheck {
        address: &address,
        paths: &paths,
        prior: &prior,
        planned: &planned,
        value_type: &object_type(),
    })
    .unwrap();
    assert_eq!(changed, vec!["name".to_string()]);

    let mut unknown = planned.clone();
    if let Value::Object(attributes) = &mut unknown {
        attributes.insert("length".into(), Value::Unknown);
    }
    let changed = replacement_paths(&ReplacementCheck {
        address: &address,
        paths: &paths[1..2],
        prior: &prior,
        planned: &unknown,
        value_type: &object_type(),
    })
    .unwrap();
    assert_eq!(changed, vec!["length".to_string()]);
}

#[test]
fn replacement_paths_that_exist_nowhere_are_provider_errors() {
    let address = ResourceAddress::new("example_thing", "one");
    let prior = value(&json!({"name": "x"}));
    let error = replacement_paths(&ReplacementCheck {
        address: &address,
        paths: &[protocol_path(&[protocol::Selector::AttributeName(
            "missing".into(),
        )])],
        prior: &prior,
        planned: &prior,
        value_type: &object_type(),
    })
    .unwrap_err();
    assert!(error.to_string().contains("`missing`"), "{error}");
}

#[test]
fn provider_values_keep_message_pack_verbatim_and_convert_json() {
    let value_type = Type::from_json(&json!(["object", {"id": "string"}])).unwrap();
    // An unknown value carrying refinement bytes cuenv does not interpret.
    let refined: Vec<u8> = [&[0x81, 0xa2][..], b"id", &[0xd4, 0x00, 0x2a]].concat();
    let (decoded, bytes) = provider_value(
        Some(&protocol::DynamicValue {
            message_pack: refined.clone(),
            json: Vec::new(),
        }),
        &value_type,
    )
    .unwrap();
    assert_eq!(bytes, refined);
    assert_eq!(decoded.attribute("id"), Some(&Value::Unknown));

    let (decoded, bytes) = provider_value(
        Some(&protocol::DynamicValue {
            message_pack: Vec::new(),
            json: br#"{"id": "from-json"}"#.to_vec(),
        }),
        &value_type,
    )
    .unwrap();
    assert_eq!(
        decoded.attribute("id"),
        Some(&Value::String("from-json".into()))
    );
    assert_eq!(
        type_system::from_message_pack(&bytes, &value_type).unwrap(),
        decoded
    );

    let (decoded, bytes) = provider_value(None, &value_type).unwrap();
    assert!(decoded.is_null());
    assert_eq!(bytes, NULL_MESSAGE_PACK.to_vec());
}

fn name_block() -> Block {
    Block {
        attributes: BTreeMap::from([(
            "name".to_string(),
            crate::schema::Attribute {
                value_type: Type::String,
                nested: None,
                presence: crate::schema::Presence::Required,
                sensitive: false,
            },
        )]),
        blocks: BTreeMap::new(),
    }
}

fn named(name: &str) -> Value {
    Value::Object(BTreeMap::from([(
        "name".to_string(),
        Value::String(name.into()),
    )]))
}

#[test]
fn legacy_providers_get_invalid_plans_tolerated_but_never_null_ones() {
    let address = ResourceAddress::new("example_thing", "one");
    let block = name_block();
    let validity = |planned: &Value, legacy_type_system| {
        PlanValidity {
            address: &address,
            block: &block,
            prior: &Value::Null,
            configuration: &named("configured"),
            planned,
            legacy_type_system,
        }
        .check()
    };
    let drifted = named("drifted");
    let error = validity(&drifted, false).unwrap_err().to_string();
    assert!(error.contains("example_thing.one"), "{error}");
    assert!(
        error.contains("name: planned value does not match"),
        "{error}"
    );
    assert!(!error.contains("drifted"), "{error}");
    assert!(validity(&drifted, true).is_ok());
    assert!(validity(&Value::Null, true).is_err());
    assert!(validity(&named("configured"), false).is_ok());
}

fn infrastructure() -> Infrastructure {
    serde_json::from_value(json!({
        "state": {"turso": {"url": "http://unused"}},
        "providers": {},
        "resources": {},
    }))
    .unwrap()
}

fn engine(store: Arc<dyn StateStore>, unrecorded_directory: &Path) -> InfrastructureEngine {
    InfrastructureEngine::new(EngineSetup {
        tenant: tenant(),
        store,
        infrastructure: infrastructure(),
        options: EngineOptions {
            project_directory: unrecorded_directory.to_path_buf(),
            plugin_cache_directory: None,
            withheld_environment_variables: Vec::new(),
            unrecorded_directory: Some(unrecorded_directory.join("unrecorded")),
            cancellation: Cancellation::default(),
        },
    })
}

#[tokio::test]
async fn apply_writes_refresh_only_records_under_the_lock() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let lock = store.lock(&tenant(), "test").await.unwrap();
    let plan = plan_of(vec![refresh_only("pet")]);
    let mut events = Vec::new();
    engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |event| {
            events.push(event);
        })
        .await
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [ApplyEvent::Refreshed { address }] if address.name == "pet"
    ));
    let rows = store.list(&tenant()).await.unwrap();
    assert_eq!(rows[0].state, json!({"id": "refreshed"}));
}

#[tokio::test]
async fn failed_refresh_only_writes_are_plain_state_errors() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let stale = StateLock {
        lock_identifier: "not-held".into(),
    };
    let error = engine
        .apply(
            &plan_of(vec![refresh_only("pet")]),
            ApplyContext { lock: &stale },
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, InfrastructureError::LockLost { .. }),
        "{error}"
    );
    assert!(
        engine
            .unrecorded_store()
            .unwrap()
            .list(&tenant())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn apply_stops_between_resources_once_stop_is_requested() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let lock = store.lock(&tenant(), "test").await.unwrap();
    engine.cancellation().stop();
    let error = engine
        .apply(
            &plan_of(vec![refresh_only("pet")]),
            ApplyContext { lock: &lock },
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            InfrastructureError::Interrupted {
                completed: 0,
                total: 0
            }
        ),
        "{error}"
    );
    assert!(store.list(&tenant()).await.unwrap().is_empty());
}

#[tokio::test]
async fn planning_refuses_while_unrecorded_changes_are_pending() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    engine
        .unrecorded_store()
        .unwrap()
        .save(&tenant(), &record("random_pet", "pet", &[]))
        .unwrap();
    let error = engine.plan(PlanMode::Apply).await.unwrap_err();
    assert!(
        matches!(
            error,
            InfrastructureError::UnrecordedChangesPending { count: 1, .. }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("cuenv infrastructure state recover"),
        "{error}"
    );
}

#[tokio::test]
async fn planning_after_a_stop_request_is_interrupted() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    assert!(
        engine
            .plan(PlanMode::Apply)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    engine.cancellation().stop();
    assert!(matches!(
        engine.plan(PlanMode::Apply).await,
        Err(InfrastructureError::InterruptedWhilePlanning)
    ));
}

#[test]
fn unrecorded_changes_are_saved_and_never_leak_state_into_errors() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    let cause = InfrastructureError::state("connection refused");
    let saved = engine.save_unrecorded(&record("random_pet", "pet", &[]), &cause);
    let message = saved.to_string();
    assert!(
        matches!(saved, InfrastructureError::UnrecordedChange { .. }),
        "{message}"
    );
    assert!(message.contains("random_pet.pet"), "{message}");
    assert!(message.contains("state recover"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    assert_eq!(
        engine
            .unrecorded_store()
            .unwrap()
            .list(&tenant())
            .unwrap()
            .len(),
        1
    );

    // When even the local save fails, the error names the address only.
    let blocked = tempfile::tempdir().unwrap();
    std::fs::write(blocked.path().join("unrecorded"), b"not a directory").unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = self::engine(store, blocked.path());
    let lost = engine.save_unrecorded(&record("random_pet", "pet", &[]), &cause);
    let message = lost.to_string();
    assert!(
        matches!(lost, InfrastructureError::UnrecordedChangeLost { .. }),
        "{message}"
    );
    assert!(message.contains("random_pet.pet"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    assert!(!message.contains("connection refused"), "{message}");
}

#[test]
fn engine_setup_debug_omits_the_store_and_configuration_values() {
    let directory = tempfile::tempdir().unwrap();
    let setup = EngineSetup {
        tenant: tenant(),
        store: Arc::new(MemoryStateStore::new()),
        infrastructure: infrastructure(),
        options: EngineOptions {
            project_directory: directory.path().to_path_buf(),
            plugin_cache_directory: None,
            withheld_environment_variables: Vec::new(),
            unrecorded_directory: None,
            cancellation: Cancellation::default(),
        },
    };
    let rendered = format!("{setup:?}");
    assert!(rendered.contains("EngineSetup"), "{rendered}");
    assert!(rendered.contains("example.com/app"), "{rendered}");
}
