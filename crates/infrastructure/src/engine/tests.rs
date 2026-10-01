use super::*;
use crate::state::MemoryStateStore;
use async_trait::async_trait;
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
        // As stored by the first write.
        serial: 1,
        generation: uuid::Uuid::nil(),
    }
}

#[test]
fn dependencies_resolve_to_addresses_and_stay_distinct_across_types() {
    // `x` was a `fake_obj` that depended on `y`; it became a `fake_obj2` and
    // the old one's deletion failed, so both records exist, and `y` now
    // depends on the new one. Bare names would make that a cycle.
    let mut old_x = change(ResourceAddress::new("fake_obj", "x"), Action::Delete);
    old_x.stored = Some(record("fake_obj", "x", &["fake_obj.y"]));
    let mut new_x = change(ResourceAddress::new("fake_obj2", "x"), Action::Delete);
    new_x.stored = Some(record("fake_obj2", "x", &[]));
    let mut y = change(ResourceAddress::new("fake_obj", "y"), Action::Delete);
    y.stored = Some(record("fake_obj", "y", &["fake_obj2.x"]));
    let changes = [old_x, new_x, y];
    let schedule = Schedule::build(&changes).unwrap();
    assert_eq!(
        operation_labels(&schedule),
        vec![
            "delete fake_obj.x",
            "delete fake_obj.y",
            "delete fake_obj2.x"
        ]
    );
}

#[test]
fn bare_stored_dependencies_resolve_against_the_stored_records() {
    let mut child = change(ResourceAddress::new("random_id", "child"), Action::Delete);
    child.stored = Some(record("random_id", "child", &["parent", "gone"]));
    let mut parent = change(ResourceAddress::new("random_pet", "parent"), Action::Delete);
    parent.stored = Some(record("random_pet", "parent", &[]));
    let changes = [parent, child];
    let schedule = Schedule::build(&changes).unwrap();
    assert_eq!(
        operation_labels(&schedule),
        vec!["delete random_id.child", "delete random_pet.parent"]
    );
}

#[test]
fn topological_order_rejects_cycles_and_unknown_dependencies() {
    assert!(topological_order(&graph(&[("first", &["second"]), ("second", &["first"])])).is_err());
    assert!(topological_order(&graph(&[("first", &["missing"])])).is_err());
}

#[tokio::test]
async fn provider_install_source_is_validated_before_provider_launch() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    let provider_source = ProviderSource::parse("hashicorp/random").unwrap();

    let version_and_path = InfrastructureProvider {
        source: "hashicorp/random".into(),
        version: Some("3.7.2".into()),
        path: Some("provider".into()),
        configuration: serde_json::Map::new(),
    };
    let error = engine
        .resolve_binary("random", &version_and_path, &provider_source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sets both `path` and `version`"));

    let missing_source = InfrastructureProvider {
        source: "hashicorp/random".into(),
        version: None,
        path: None,
        configuration: serde_json::Map::new(),
    };
    let error = engine
        .resolve_binary("random", &missing_source, &provider_source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("needs an exact `version`"));

    let error = engine
        .ensure_provider("undeclared", &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not declared in infrastructure.providers")
    );
}

fn declaration(resource_type: &str, depends_on: &[&str]) -> ManagedResourceDeclaration {
    ManagedResourceDeclaration {
        resource_type: resource_type.into(),
        provider: None,
        depends_on: depends_on.iter().map(|name| (*name).to_owned()).collect(),
        configuration: serde_json::Map::new(),
    }
}

fn provider(version: Option<&str>, path: Option<&str>) -> InfrastructureProvider {
    InfrastructureProvider {
        source: "hashicorp/unused".into(),
        version: version.map(str::to_owned),
        path: path.map(str::to_owned),
        configuration: serde_json::Map::new(),
    }
}

fn message(error: InfrastructureError) -> String {
    match error {
        InfrastructureError::Configuration(message) => message,
        other => panic!("expected a configuration error, got {other}"),
    }
}

#[test]
fn configuration_preflight_checks_unused_providers_and_resource_references() {
    let mut configuration = infrastructure();
    configuration
        .providers
        .insert("unused".into(), provider(None, None));
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert!(
        error.contains(
            "infrastructure.providers.unused: provider 'unused' needs an exact `version`"
        ),
        "{error}"
    );

    configuration
        .providers
        .insert("unused".into(), provider(Some("~> 3.7"), None));
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert!(
        error.contains("infrastructure.providers.unused.version: invalid provider version"),
        "{error}"
    );

    configuration.providers.remove("unused");
    configuration
        .resources
        .insert("pet".into(), declaration("random_pet", &[]));
    configuration.resources.get_mut("pet").unwrap().provider = Some("missing".into());
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert!(
        error.contains(
            "infrastructure.resources.pet.provider: no provider named 'missing' in infrastructure.providers"
        ),
        "{error}"
    );
}

#[test]
fn configuration_preflight_reports_every_problem_with_its_full_field_path() {
    let mut configuration = infrastructure();
    configuration.providers.insert(
        "both".into(),
        provider(Some("1.0.0"), Some("/bin/provider")),
    );
    configuration
        .providers
        .insert("neither".into(), provider(None, None));
    configuration
        .providers
        .insert("blank".into(), provider(None, Some("")));
    configuration.resources.insert(
        "pet".into(),
        declaration("undeclared_pet", &["ghost", "pet"]),
    );
    let error = message(validate_configuration(&configuration, Some("dev")).unwrap_err());
    for expected in [
        "infrastructure.environments.dev.providers.both: provider 'both' sets both `path` and `version`",
        "infrastructure.environments.dev.providers.neither: provider 'neither' needs an exact `version`",
        "infrastructure.environments.dev.providers.blank.path: must not be empty",
        "infrastructure.environments.dev.resources.pet.type: no provider named 'undeclared'",
        "infrastructure.environments.dev.resources.pet.dependsOn[0]: resource 'pet' depends on unknown resource 'ghost'",
        "infrastructure.environments.dev.resources.pet.dependsOn[1]: resource 'pet' depends on itself",
    ] {
        assert!(
            error.contains(expected),
            "missing `{expected}` in:\n{error}"
        );
    }
    // The remedy and the environment rule are part of the message.
    assert!(
        error.contains("declare it there or set `provider`"),
        "{error}"
    );
    assert!(
        error.contains("top-level `infrastructure.providers` are not inherited by environments"),
        "{error}"
    );
    // Six problems, one line each, all in one error: the self-dependency
    // is reported although another dependency is unknown.
    assert!(error.starts_with("6 problems:"), "{error}");
}

#[test]
fn an_empty_provider_path_is_rejected_even_without_the_schema() {
    let mut configuration = infrastructure();
    configuration
        .providers
        .insert("local".into(), provider(None, Some("")));
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert_eq!(
        error,
        "infrastructure.providers.local.path: must not be empty; set a path to the provider \
         binary or remove it and set `version`"
    );
}

#[test]
fn dependency_cycles_are_reported_with_the_resources_path() {
    let mut configuration = infrastructure();
    configuration
        .providers
        .insert("random".into(), provider(Some("3.7.2"), None));
    configuration
        .resources
        .insert("a".into(), declaration("random_pet", &["b"]));
    configuration
        .resources
        .insert("b".into(), declaration("random_pet", &["a"]));
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert_eq!(
        error,
        "infrastructure.resources: dependency cycle between resources a, b: \
         infrastructure.resources.a.dependsOn[0] -> b, \
         infrastructure.resources.b.dependsOn[0] -> a; remove one of these dependsOn entries"
    );
}

#[test]
fn each_dependency_cycle_is_reported_alone_even_beside_unknown_dependencies() {
    let mut configuration = infrastructure();
    configuration
        .providers
        .insert("random".into(), provider(Some("3.7.2"), None));
    let resources = [
        ("a", &["b"][..]),
        ("b", &["a", "ghost"][..]),
        // Not part of any cycle, although it depends on one.
        ("bystander", &["a"][..]),
        ("c", &["d"][..]),
        ("d", &["e"][..]),
        ("e", &["c"][..]),
    ];
    for (name, depends_on) in resources {
        configuration
            .resources
            .insert(name.into(), declaration("random_pet", depends_on));
    }
    let error = message(validate_configuration(&configuration, None).unwrap_err());
    assert!(error.starts_with("3 problems:"), "{error}");
    assert!(
        error.contains(
            "infrastructure.resources.b.dependsOn[1]: resource 'b' depends on unknown resource 'ghost'"
        ),
        "{error}"
    );
    assert!(
        error.contains(
            "dependency cycle between resources a, b: infrastructure.resources.a.dependsOn[0] \
             -> b, infrastructure.resources.b.dependsOn[0] -> a;"
        ),
        "{error}"
    );
    assert!(
        error.contains(
            "dependency cycle between resources c, d, e: infrastructure.resources.c.dependsOn[0] \
             -> d, infrastructure.resources.d.dependsOn[0] -> e, \
             infrastructure.resources.e.dependsOn[0] -> c;"
        ),
        "{error}"
    );
    assert!(!error.contains("bystander"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_unused_provider_fails_before_any_declared_provider_launches() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("provider-started");
    let binary = directory.path().join("provider.sh");
    std::fs::write(
        &binary,
        "#!/bin/sh\nprintf started > \"$CUENV_TEST_PROVIDER_MARKER\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    let mut configuration = infrastructure();
    configuration.providers.insert(
        "random".into(),
        InfrastructureProvider {
            source: "hashicorp/random".into(),
            version: None,
            path: Some(binary.display().to_string()),
            configuration: serde_json::Map::new(),
        },
    );
    configuration.providers.insert(
        "unused".into(),
        InfrastructureProvider {
            source: "hashicorp/unused".into(),
            version: None,
            path: None,
            configuration: serde_json::Map::new(),
        },
    );
    configuration.resources.insert(
        "pet".into(),
        ManagedResourceDeclaration {
            resource_type: "random_pet".into(),
            provider: None,
            depends_on: Vec::new(),
            configuration: serde_json::Map::new(),
        },
    );

    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    engine.infrastructure = configuration;
    engine.options.provider_environment_variables.insert(
        "CUENV_TEST_PROVIDER_MARKER".into(),
        marker.display().to_string(),
    );
    let error = engine.plan(PlanMode::Apply).await.unwrap_err().to_string();

    assert!(
        error.contains("provider 'unused' needs an exact `version`"),
        "{error}"
    );
    assert!(
        !marker.exists(),
        "a provider launched before validation: {error}"
    );
}

fn mask_tab_secret(text: &str) -> String {
    text.replace("pass\tword", "*_*")
}

#[test]
fn diagnostics_redact_secrets_containing_control_characters_before_stripping() {
    // Stripping first would leave "password", which the registered secret
    // "pass<TAB>word" no longer matches.
    let diagnostic = Diagnostic {
        severity: Severity::Error as i32,
        summary: "login with pass\tword failed".into(),
        detail: "server echoed pass\tword back".into(),
        attribute: None,
    };
    let rendered = render_diagnostic_with(&diagnostic, mask_tab_secret);
    assert_eq!(rendered, "login with *_* failed: server echoed *_* back");
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
        environment_identity: environment_identity(&BTreeMap::new()),
    }
}

fn replacement_step(kind: StepKind) -> ApplyStep {
    ApplyStep {
        kind,
        prior: Vec::new(),
        planned: Vec::new(),
        configuration: Vec::new(),
        planned_private: Vec::new(),
        planned_value: Value::Null,
    }
}

/// The dependencies of a change: the names (or addresses) its stored record
/// depends on, and the ones its configuration depends on.
#[derive(Clone, Copy)]
struct Wiring<'names> {
    stored: &'names [&'names str],
    configured: &'names [&'names str],
}

fn wired<'names>(
    stored: &'names [&'names str],
    configured: &'names [&'names str],
) -> Wiring<'names> {
    Wiring { stored, configured }
}

/// Give `change` the steps its action needs.
fn with_steps(mut change: ResourceChange) -> ResourceChange {
    match change.action {
        Action::Replace => {
            change.steps = vec![
                replacement_step(StepKind::Delete),
                replacement_step(StepKind::Create),
            ];
        }
        Action::Update => change.steps = vec![replacement_step(StepKind::Update)],
        Action::Create => change.steps = vec![replacement_step(StepKind::Create)],
        Action::Delete => change.steps = vec![replacement_step(StepKind::Delete)],
        Action::Refresh => change.refreshed_record = change.stored.clone(),
        Action::NoOp => {}
    }
    change
}

/// A resource of any type: the dependencies in `wiring` are full addresses.
fn thing(address: &str, action: Action, wiring: Wiring<'_>) -> ResourceChange {
    let (resource_type, name) = address.split_once('.').unwrap();
    let mut change = change(ResourceAddress::new(resource_type, name), action);
    change.dependencies = wiring
        .configured
        .iter()
        .map(|text| (*text).to_string())
        .collect();
    if !matches!(action, Action::Create) {
        change.stored = Some(record(resource_type, name, wiring.stored));
    }
    with_steps(change)
}

/// A `random_pet` change. The dependencies in `wiring` are names; they are
/// recorded as full addresses, as the engine does.
fn pet(name: &str, action: Action, wiring: Wiring<'_>) -> ResourceChange {
    let addresses = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .map(|name| format!("random_pet.{name}"))
            .collect()
    };
    let stored = addresses(wiring.stored);
    let configured = addresses(wiring.configured);
    thing(
        &format!("random_pet.{name}"),
        action,
        wired(
            &stored.iter().map(String::as_str).collect::<Vec<_>>(),
            &configured.iter().map(String::as_str).collect::<Vec<_>>(),
        ),
    )
}

fn operation_labels(schedule: &Schedule<'_>) -> Vec<String> {
    schedule
        .operations
        .iter()
        .map(|scheduled| {
            format!(
                "{} {}",
                scheduled.operation.kind.label(),
                scheduled.operation.change.address
            )
        })
        .collect()
}

fn schedule_of(changes: &[ResourceChange]) -> Vec<String> {
    operation_labels(&Schedule::build(changes).unwrap())
}

fn short(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .map(|label| label.replace("random_pet.", ""))
        .collect()
}

#[test]
fn orphan_deletes_run_before_creates_and_updates() {
    // Renaming a resource key: the old object goes first, so the new one can
    // take its real-world identity.
    let changes = [
        pet("new", Action::Create, wired(&[], &[])),
        pet("old", Action::Delete, wired(&[], &[])),
    ];
    assert_eq!(short(&schedule_of(&changes)), ["delete old", "apply new"]);
}

#[test]
fn a_removed_child_is_deleted_before_its_parent_is_updated() {
    // Terraform's `creators` edge: the parent's update waits for the delete
    // of what hangs off it.
    let changes = [
        pet("parent", Action::Update, wired(&[], &[])),
        pet("child", Action::Delete, wired(&["parent"], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete child", "apply parent"]
    );
}

#[test]
fn a_replacement_create_follows_its_delete_at_once() {
    let changes = [
        pet("b", Action::Update, wired(&[], &[])),
        pet("a", Action::Replace, wired(&[], &[])),
        pet("c", Action::Create, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete a", "create a", "apply b", "apply c"]
    );
}

#[test]
fn dependents_are_deleted_before_a_replaced_parent_and_recreated_after_it() {
    let changes = [
        pet("parent", Action::Replace, wired(&[], &[])),
        pet("child", Action::Replace, wired(&["parent"], &["parent"])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        [
            "delete child",
            "delete parent",
            "create parent",
            "create child"
        ]
    );
}

#[test]
fn an_update_that_drops_a_dependency_runs_after_the_delete_of_the_old_parent() {
    // Terraform connects creators to the destroyers they may depend on: the
    // dependent's update waits for the delete of what its stored record
    // depends on. Detaching first would invert that, and would delay an
    // orphan delete behind a create (see the rename tests below).
    let removed = [
        pet("parent", Action::Delete, wired(&[], &[])),
        pet("dependent", Action::Update, wired(&["parent"], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&removed)),
        ["delete parent", "apply dependent"]
    );
    let replaced = [
        pet("parent", Action::Replace, wired(&[], &[])),
        pet("dependent", Action::Update, wired(&["parent"], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&replaced)),
        ["delete parent", "create parent", "apply dependent"]
    );
}

#[test]
fn updates_that_attach_a_new_resource_and_drop_an_old_one_are_ordered_not_refused() {
    let changes = [
        pet("parent", Action::Replace, wired(&[], &[])),
        pet("retained", Action::Update, wired(&["parent"], &["fresh"])),
        pet("fresh", Action::Create, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        [
            "delete parent",
            "create parent",
            "apply fresh",
            "apply retained"
        ]
    );
}

#[test]
fn the_creators_edge_orders_a_replaced_childs_delete_before_its_parent_update() {
    // `c` needs `p` updated first and `p`'s update must follow `c`'s delete
    // (Terraform's creators edge): the safety edge that would make `c`'s
    // delete wait for `p` is dropped, as in Terraform.
    let changes = [
        pet("c", Action::Replace, wired(&["p"], &["p"])),
        pet("p", Action::Update, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete c", "apply p", "create c"]
    );
}

#[test]
fn a_parents_update_waits_for_the_delete_of_what_hangs_off_it_even_when_hurried() {
    // Terraform's creators edge. `a` is replaced and its create waits for
    // `p`'s update, so once `a` is deleted the update is hurried; it must
    // still wait for the delete of `z`, a child of `p` that has nothing to do
    // with `a`.
    let changes = [
        pet("p", Action::Update, wired(&[], &[])),
        pet("a", Action::Replace, wired(&["p"], &["p"])),
        pet("z", Action::Replace, wired(&["p"], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete a", "delete z", "apply p", "create z", "create a"]
    );
}

#[test]
fn an_update_waits_for_the_delete_of_what_its_record_depends_on_even_when_hurried() {
    // Terraform connects creators to the destroyers they may depend on. `a`
    // is replaced and its create waits for `c`'s update, so once `a` is
    // deleted the update is hurried; `c`'s stored record depends on `d`,
    // which is replaced, so `d` is deleted first.
    let changes = [
        pet("a", Action::Replace, wired(&["c"], &["c"])),
        pet("c", Action::Update, wired(&["d"], &[])),
        pet("d", Action::Replace, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete a", "delete d", "apply c", "create d", "create a"]
    );
}

#[test]
fn renaming_a_resource_whose_dependent_only_refreshes_deletes_the_old_one_first() {
    // N1: the dependent follows the rename, so its record is rewritten. A
    // record rewrite detaches nothing; the old object goes before the new
    // one is created, because they may be the same real-world object.
    let changes = [
        pet("new", Action::Create, wired(&[], &[])),
        pet("old", Action::Delete, wired(&[], &[])),
        pet("r", Action::Refresh, wired(&["old"], &["new"])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete old", "apply new", "refresh r"]
    );
}

#[test]
fn renaming_a_resource_whose_dependent_is_updated_deletes_the_old_one_first() {
    // N1b
    let changes = [
        pet("new", Action::Create, wired(&[], &[])),
        pet("old", Action::Delete, wired(&[], &[])),
        pet("r", Action::Update, wired(&["old"], &["new"])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete old", "apply new", "apply r"]
    );
}

#[test]
fn renaming_a_resource_whose_dependent_is_replaced_deletes_the_old_one_first() {
    // N1c: the dependent's delete may not wait for the new object either,
    // because the old object's delete waits for the dependent's.
    let changes = [
        pet("new", Action::Create, wired(&[], &[])),
        pet("old", Action::Delete, wired(&[], &[])),
        pet("r", Action::Replace, wired(&["old"], &["new"])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete r", "delete old", "apply new", "create r"]
    );
}

#[test]
fn a_type_change_with_a_dependent_deletes_the_old_object_first() {
    // N3: the same name under another type is a new address and an orphan.
    for action in [Action::Refresh, Action::Update, Action::Replace] {
        let changes = [
            thing("fake_obj2.x", Action::Create, wired(&[], &[])),
            thing("fake_obj.x", Action::Delete, wired(&[], &[])),
            thing(
                "fake_obj.y",
                action,
                wired(&["fake_obj.x"], &["fake_obj2.x"]),
            ),
        ];
        let order = schedule_of(&changes);
        let position = |label: &str| order.iter().position(|entry| entry == label).unwrap();
        assert!(
            position("delete fake_obj.x") < position("apply fake_obj2.x"),
            "{action:?}: {order:?}"
        );
    }
}

#[test]
fn a_refresh_waits_for_the_refreshes_and_updates_it_is_configured_to_depend_on() {
    // N4: `y` (updated) is configured to depend on `x`, whose record is only
    // refreshed. If `x`'s refresh cannot run, `y` must not either, or the
    // stored records would depend on each other.
    let changes = [
        pet("x", Action::Refresh, wired(&["y"], &["z"])),
        pet("y", Action::Update, wired(&[], &["x"])),
        pet("z", Action::Update, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["apply z", "refresh x", "apply y"]
    );
}

/// A deterministic source of numbers below a bound.
fn random_source(mut seed: u64) -> impl FnMut(u64) -> u64 {
    move |bound| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    }
}

/// A few changes of the given actions, with random stored dependencies and
/// configured dependencies that point to earlier names only (so the
/// configuration has no cycle, while the stored records may).
fn random_changes(next: &mut dyn FnMut(u64) -> u64, actions: &[Action]) -> Vec<ResourceChange> {
    let count = 3 + usize::try_from(next(5)).unwrap();
    let names: Vec<String> = (0..count).map(|index| format!("n{index}")).collect();
    let mut changes = Vec::new();
    for (mine, name) in names.iter().enumerate() {
        let action = actions[usize::try_from(next(actions.len() as u64)).unwrap()];
        let mut stored: Vec<&str> = Vec::new();
        let mut configured: Vec<&str> = Vec::new();
        for (other, other_name) in names.iter().enumerate() {
            if other != mine && action != Action::Create && next(3) == 0 {
                stored.push(other_name);
            }
            if other < mine && action != Action::Delete && next(3) == 0 {
                configured.push(other_name);
            }
        }
        changes.push(pet(name, action, wired(&stored, &configured)));
    }
    changes
}

/// Changes in a line: action, address, stored and configured dependencies.
fn describe(changes: &[ResourceChange]) -> String {
    changes
        .iter()
        .map(|change| {
            let stored = change
                .stored
                .as_ref()
                .map_or_else(Vec::new, |stored| stored.dependencies.clone());
            format!(
                "{:?} {} stored={stored:?} configured={:?}",
                change.action, change.address, change.dependencies
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

const ALL_ACTIONS: [Action; 5] = [
    Action::Create,
    Action::Update,
    Action::Replace,
    Action::Delete,
    Action::Refresh,
];

#[test]
fn an_orphan_delete_never_waits_for_a_create_or_an_update() {
    // Whatever the mix of changes, every orphan delete, and every
    // replacement delete that must come before one, runs before anything
    // else.
    let mut next = random_source(0x2545_f491_4f6c_dd1d);
    for round in 0..400 {
        let changes = random_changes(&mut next, &ALL_ACTIONS);
        let schedule = Schedule::build(&changes).unwrap();
        let kinds: Vec<OperationKind> = schedule
            .operations
            .iter()
            .map(|scheduled| scheduled.operation.kind)
            .collect();
        for (index, kind) in kinds.iter().enumerate() {
            if *kind != OperationKind::Delete {
                continue;
            }
            assert!(
                kinds[..index].iter().all(|earlier| matches!(
                    earlier,
                    OperationKind::Delete | OperationKind::ReplaceDelete
                )),
                "round {round}: an orphan delete came after other work: {:?}",
                operation_labels(&schedule)
            );
        }
    }
}

#[test]
fn a_replacement_is_deleted_before_every_create_it_is_configured_to_depend_on() {
    // Whatever the mix of changes: the create of a new resource, or the
    // create half of a replacement, never comes before the delete of a
    // replacement that depends on it.
    let mut next = random_source(0x9e37_79b9_7f4a_7c15);
    let actions = [
        Action::Create,
        Action::Update,
        Action::Replace,
        Action::Delete,
    ];
    for round in 0..400 {
        let changes = random_changes(&mut next, &actions);
        let schedule = Schedule::build(&changes).unwrap();
        let position = |kind: OperationKind, address: &ResourceAddress| {
            schedule.operations.iter().position(|scheduled| {
                scheduled.operation.kind == kind && scheduled.operation.change.address == *address
            })
        };
        for replaced in changes
            .iter()
            .filter(|change| change.action == Action::Replace)
        {
            let delete = position(OperationKind::ReplaceDelete, &replaced.address).unwrap();
            for prerequisite in &changes {
                if !replaced
                    .dependencies
                    .contains(&prerequisite.address.to_string())
                {
                    continue;
                }
                let kind = match prerequisite.action {
                    Action::Create => OperationKind::Apply,
                    Action::Replace => OperationKind::ReplaceCreate,
                    _ => continue,
                };
                let create = position(kind, &prerequisite.address).unwrap();
                assert!(
                    delete < create,
                    "round {round}: {} was deleted after {} was created: {:?}; changes {}",
                    replaced.address,
                    prerequisite.address,
                    operation_labels(&schedule),
                    describe(&changes)
                );
            }
        }
    }
}

#[tokio::test]
async fn a_replacement_that_was_deleted_is_recreated_or_reported_whichever_operation_fails() {
    // The old object of a replacement may be destroyed before the create of
    // what it depends on fails, but it is never lost silently: the replacement
    // is recreated or reported as deleted and not recreated.
    let mut next = random_source(0x2545_f491_4f6c_dd1d);
    let actions = [
        Action::Create,
        Action::Update,
        Action::Replace,
        Action::Delete,
    ];
    for round in 0..300 {
        let changes = random_changes(&mut next, &actions);
        let schedule = Schedule::build(&changes).unwrap();
        let labels = operation_labels(&schedule);
        let failing: Vec<&str> = labels
            .iter()
            .filter(|label| !label.starts_with("delete ") && next(5) == 0)
            .map(String::as_str)
            .collect();
        let runner = ScriptedRunner::new().failing(&failing);
        let scripted = run_scripted(changes.clone(), &runner).await;
        let ran = runner.ran();
        let reported: Vec<String> = match &scripted.result {
            Err(InfrastructureError::ApplyIncomplete(incomplete)) => incomplete
                .deleted_not_recreated
                .iter()
                .map(|address| address.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        for entry in &ran {
            let Some(name) = entry.strip_prefix("delete ") else {
                continue;
            };
            let recreated = ran.contains(&format!("create {name}"));
            let kept = reported.iter().any(|reported| reported == name);
            let orphan = changes
                .iter()
                .any(|change| change.address.name == name && change.action == Action::Delete);
            assert!(
                recreated || kept || orphan,
                "round {round}: {name} was deleted and neither recreated nor reported \
                 (ran {ran:?}, failing {failing:?}); changes {}",
                describe(&changes)
            );
        }
    }
}

#[test]
fn a_replacement_is_deleted_before_the_creates_of_what_it_depends_on() {
    // The replaced object is destroyed anyway, so deleting it first costs
    // availability and never data. Creating the prerequisite first could
    // collide with the old object, or take over its real-world identity.
    let changes = [
        pet("r", Action::Replace, wired(&[], &["fresh"])),
        pet("fresh", Action::Create, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete r", "apply fresh", "create r"]
    );
}

#[test]
fn an_identity_handoff_deletes_the_old_object_before_the_new_prerequisite_is_created() {
    // `c` takes over what `a` held, and `a` is replaced to hold something
    // else, after `c`. The delete of `a` must come first: if `c` were created
    // before it, `a`'s delete would destroy the object `c` just made.
    let changes = [
        pet("a", Action::Replace, wired(&[], &["c"])),
        pet("c", Action::Create, wired(&[], &[])),
    ];
    let order = short(&schedule_of(&changes));
    let position = |label: &str| order.iter().position(|entry| entry == label).unwrap();
    assert!(position("delete a") < position("apply c"), "{order:?}");
    assert!(position("apply c") < position("create a"), "{order:?}");
}

#[test]
fn replacement_deletes_precede_the_creates_they_wait_behind_even_when_hurried() {
    // `a` and `x` are both replaced and both depend on the new `c`. After
    // the delete of `a`, the create of `a` and `c` are hurried, which must
    // not let `c` overtake the delete of `x`.
    let changes = [
        pet("a", Action::Replace, wired(&[], &["c"])),
        pet("x", Action::Replace, wired(&[], &["c"])),
        pet("c", Action::Create, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete a", "delete x", "apply c", "create a", "create x"]
    );
}

#[test]
fn a_replacement_delete_is_not_ordered_against_a_prerequisite_that_is_only_updated() {
    // Only creates collide with an object that is about to be destroyed: an
    // update keeps its own object, so neither waits for the other.
    let changes = [
        pet("r", Action::Replace, wired(&[], &["kept"])),
        pet("kept", Action::Update, wired(&[], &[])),
    ];
    let schedule = Schedule::build(&changes).unwrap();
    let order = short(&operation_labels(&schedule));
    let position = |label: &str| order.iter().position(|entry| entry == label).unwrap();
    let delete = position("delete r");
    let update = position("apply kept");
    assert!(
        !schedule.operations[delete].successors.contains(&update),
        "the update waits for the delete: {order:?}"
    );
    assert!(
        !schedule.operations[update].successors.contains(&delete),
        "the delete waits for the update: {order:?}"
    );
}

#[test]
fn a_replacement_is_deleted_before_the_create_half_of_a_replaced_prerequisite() {
    // `p` is replaced and `r` depends on it: `r`'s old object goes before the
    // new `p` is created, as before any other create.
    let changes = [
        pet("p", Action::Replace, wired(&[], &[])),
        pet("r", Action::Replace, wired(&[], &["p"])),
    ];
    let order = short(&schedule_of(&changes));
    let position = |label: &str| order.iter().position(|entry| entry == label).unwrap();
    assert!(position("delete r") < position("create p"), "{order:?}");
    assert!(position("create p") < position("create r"), "{order:?}");
}

#[test]
fn stored_dependency_cycles_among_deletes_are_ordered_with_a_warning() {
    // Stored dependencies are history: a partial failure can leave records
    // that depend on each other, and the project must still be destroyable.
    let changes = [
        pet("a", Action::Delete, wired(&["b"], &[])),
        pet("b", Action::Delete, wired(&["a"], &[])),
    ];
    let schedule = Schedule::build(&changes).unwrap();
    assert_eq!(
        short(&operation_labels(&schedule)),
        ["delete b", "delete a"]
    );
    assert_eq!(schedule.warnings.len(), 1, "{:?}", schedule.warnings);
    let warning = &schedule.warnings[0];
    assert!(warning.contains("cycle"), "{warning}");
    assert!(warning.contains("random_pet.a"), "{warning}");
    assert!(warning.contains("random_pet.b"), "{warning}");
    assert!(!warning.contains("configuration"), "{warning}");
    assert!(!warning.contains("  "), "{warning}");

    // A ring of three, and a delete hanging off it, still order: the edges
    // that do not close the cycle keep their place.
    let ring = [
        pet("a", Action::Delete, wired(&["b"], &[])),
        pet("b", Action::Delete, wired(&["c"], &[])),
        pet("c", Action::Delete, wired(&["a"], &[])),
        pet("leaf", Action::Delete, wired(&["a"], &[])),
    ];
    let schedule = Schedule::build(&ring).unwrap();
    assert_eq!(schedule.operations.len(), 4);
    assert_eq!(schedule.warnings.len(), 1, "{:?}", schedule.warnings);
    let labels = short(&operation_labels(&schedule));
    let position = |label: &str| labels.iter().position(|entry| entry == label).unwrap();
    assert!(position("delete leaf") < position("delete a"), "{labels:?}");
}

#[test]
fn a_cycle_through_configured_dependencies_is_refused_without_blaming_destroy() {
    let changes = [
        pet("a", Action::Create, wired(&[], &["b"])),
        pet("b", Action::Create, wired(&[], &["a"])),
    ];
    let error = message(Schedule::build(&changes).unwrap_err());
    assert!(error.contains("cycle"), "{error}");
    assert!(error.contains("a, b"), "{error}");
    assert!(!error.contains("change the configuration"), "{error}");
}

#[test]
fn the_schedule_does_not_depend_on_the_order_of_the_changes() {
    let changes = vec![
        pet("parent", Action::Replace, wired(&[], &[])),
        pet("child", Action::Replace, wired(&["parent"], &["parent"])),
        pet("old", Action::Delete, wired(&[], &[])),
        pet("fresh", Action::Create, wired(&[], &[])),
        pet("kept", Action::Update, wired(&["old"], &["fresh"])),
        pet("same", Action::NoOp, wired(&[], &[])),
        pet("rewritten", Action::Refresh, wired(&[], &[])),
    ];
    let expected = schedule_of(&changes);
    for rotation in 0..changes.len() {
        let mut rotated = changes.clone();
        rotated.rotate_left(rotation);
        assert_eq!(schedule_of(&rotated), expected, "rotation {rotation}");
    }
    let mut reversed = changes;
    reversed.reverse();
    assert_eq!(schedule_of(&reversed), expected);
}

#[test]
fn unchanged_resources_have_no_operation_and_are_listed_last() {
    let changes = [
        pet("quiet", Action::NoOp, wired(&[], &[])),
        pet("busy", Action::Create, wired(&[], &[])),
    ];
    let schedule = Schedule::build(&changes).unwrap();
    assert_eq!(operation_labels(&schedule), ["apply random_pet.busy"]);
    let order: Vec<&str> = schedule
        .change_order
        .iter()
        .map(|index| changes[*index].address.name.as_str())
        .collect();
    assert_eq!(order, ["busy", "quiet"]);
}

#[test]
fn a_plan_lists_its_changes_in_the_order_apply_runs_them() {
    let changes = vec![
        pet("new", Action::Create, wired(&[], &[])),
        pet("kept", Action::Update, wired(&["old"], &[])),
        pet("old", Action::Delete, wired(&[], &[])),
        pet("replaced", Action::Replace, wired(&[], &["new"])),
        pet("same", Action::NoOp, wired(&[], &[])),
    ];
    let ordered = in_schedule_order(changes).unwrap().changes;
    let schedule = Schedule::build(&ordered).unwrap();
    let mut first_runs: Vec<String> = Vec::new();
    for scheduled in &schedule.operations {
        let name = scheduled.operation.change.address.name.clone();
        if !first_runs.contains(&name) {
            first_runs.push(name);
        }
    }
    first_runs.push("same".to_string());
    let listed: Vec<String> = ordered
        .iter()
        .map(|change| change.address.name.clone())
        .collect();
    assert_eq!(listed, first_runs);
}

#[test]
fn a_long_chain_of_replacements_is_scheduled_in_dependency_order() {
    let length: usize = 1500;
    let name = |index: usize| format!("r{index:04}");
    let changes: Vec<ResourceChange> = (0..length)
        .map(|index| {
            let dependency = name(index.saturating_sub(1));
            let dependencies: Vec<&str> = if index == 0 {
                Vec::new()
            } else {
                vec![dependency.as_str()]
            };
            pet(
                &name(index),
                Action::Replace,
                wired(&dependencies, &dependencies),
            )
        })
        .collect();
    let labels = short(&schedule_of(&changes));
    assert_eq!(labels.len(), 2 * length);
    // Dependents go first and come back last.
    assert_eq!(labels[0], format!("delete {}", name(length - 1)));
    assert_eq!(labels[length - 1], "delete r0000");
    assert_eq!(labels[length], "create r0000");
    assert_eq!(
        labels[2 * length - 1],
        format!("create {}", name(length - 1))
    );
}

// ---------------------------------------------------------------------------
// Running a schedule
// ---------------------------------------------------------------------------

/// Runs operations without a provider: records them and fails the ones it
/// is told to.
struct ScriptedRunner {
    journal: std::sync::Mutex<Vec<String>>,
    /// Labels (`create random_pet.a`) failing as a provider failure.
    failing: BTreeSet<String>,
    /// Labels failing so the run cannot go on.
    aborting: BTreeSet<String>,
    /// Delete labels where the provider deleted the object and recording
    /// the deletion failed.
    delete_unrecorded: BTreeSet<String>,
    /// Create labels where the provider created the object and recording it
    /// failed (the record was saved locally).
    create_unrecorded: BTreeSet<String>,
    /// A label after which a stop is requested.
    stop_after: Option<(String, Cancellation)>,
}

impl ScriptedRunner {
    fn new() -> Self {
        Self {
            journal: std::sync::Mutex::new(Vec::new()),
            failing: BTreeSet::new(),
            aborting: BTreeSet::new(),
            delete_unrecorded: BTreeSet::new(),
            create_unrecorded: BTreeSet::new(),
            stop_after: None,
        }
    }

    fn failing(mut self, labels: &[&str]) -> Self {
        self.failing = labels.iter().map(|label| (*label).to_owned()).collect();
        self
    }

    fn ran(&self) -> Vec<String> {
        short(&self.journal.lock().unwrap())
    }
}

#[async_trait]
impl OperationRunner for ScriptedRunner {
    async fn run_operation(
        &self,
        operation: &ApplyOperation<'_>,
        _lock: &StateLock,
        _on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) -> std::result::Result<(), OperationFailure> {
        let label = format!("{} {}", operation.kind.label(), operation.change.address);
        self.journal.lock().unwrap().push(label.clone());
        if self.failing.contains(&label) {
            return Err(OperationFailure::Continue(
                InfrastructureError::Diagnostics {
                    context: label,
                    errors: vec!["boom".to_string()],
                },
            ));
        }
        if self.delete_unrecorded.contains(&label) {
            return Err(OperationFailure::AbortAfterDelete(
                InfrastructureError::state("the lock was lost"),
            ));
        }
        if self.create_unrecorded.contains(&label) {
            return Err(OperationFailure::Abort(
                InfrastructureError::UnrecordedChange {
                    address: operation.change.address.to_string(),
                    reason: "the state store is unavailable".to_string(),
                    saved_to: "/recovery/file".to_string(),
                },
            ));
        }
        if self.aborting.contains(&label) {
            return Err(OperationFailure::Abort(InfrastructureError::state(
                "the state store is gone",
            )));
        }
        if let Some((after, cancellation)) = &self.stop_after
            && *after == label
        {
            cancellation.stop();
        }
        Ok(())
    }
}

/// What a scripted apply produced.
struct Scripted {
    result: Result<PlanSummary>,
    events: Vec<ApplyEvent>,
}

impl Scripted {
    fn incomplete(&self) -> &IncompleteApply {
        match &self.result {
            Err(InfrastructureError::ApplyIncomplete(incomplete)) => incomplete,
            other => panic!("expected an incomplete apply, got {other:?}"),
        }
    }

    fn names(addresses: &[ResourceAddress]) -> Vec<&str> {
        addresses
            .iter()
            .map(|address| address.name.as_str())
            .collect()
    }

    fn uncreated(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| match event {
                ApplyEvent::DeletedNotRecreated { address } => Some(address.name.clone()),
                _ => None,
            })
            .collect()
    }
}

async fn run_scripted(
    changes: Vec<ResourceChange>,
    runner: &(impl OperationRunner + Sync),
) -> Scripted {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    let lock = StateLock {
        lock_identifier: "scripted".into(),
    };
    let plan = plan_of(in_schedule_order(changes).unwrap().changes);
    let mut events = Vec::new();
    let result = engine
        .converge(
            runner,
            Convergence {
                plan: &plan,
                lock: &lock,
                on_event: &mut |event| events.push(event),
            },
        )
        .await;
    Scripted { result, events }
}

/// Runs operations in a world where every object holds a unique real-world
/// identity (a file name, an address): creating an object whose identity is
/// held already fails, as it does with most providers, and deleting an
/// object frees what it holds.
struct IdentityRunner {
    /// Identity to the address of the resource whose object holds it.
    held: std::sync::Mutex<BTreeMap<String, String>>,
    /// The identity the object of an address takes when it is created.
    identities: BTreeMap<String, String>,
}

impl IdentityRunner {
    /// A world where `held` pairs (identity, address) exist already, and
    /// `identities` pairs (address, identity) say what creates take.
    fn new(held: &[(&str, &str)], identities: &[(&str, &str)]) -> Self {
        let pairs = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(first, second)| ((*first).to_string(), (*second).to_string()))
                .collect()
        };
        Self {
            held: std::sync::Mutex::new(pairs(held).into_iter().collect()),
            identities: pairs(identities).into_iter().collect(),
        }
    }

    fn holders(&self) -> Vec<(String, String)> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .map(|(identity, address)| (identity.clone(), address.clone()))
            .collect()
    }
}

#[async_trait]
impl OperationRunner for IdentityRunner {
    async fn run_operation(
        &self,
        operation: &ApplyOperation<'_>,
        _lock: &StateLock,
        _on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) -> std::result::Result<(), OperationFailure> {
        let address = operation.change.address.to_string();
        let mut held = self.held.lock().unwrap();
        match operation.kind {
            OperationKind::Delete | OperationKind::ReplaceDelete => {
                held.retain(|_, holder| *holder != address);
            }
            OperationKind::Apply | OperationKind::ReplaceCreate
                if operation.change.action != Action::Update =>
            {
                let identity = self.identities[&address].clone();
                if held.contains_key(&identity) {
                    return Err(OperationFailure::Continue(
                        InfrastructureError::Diagnostics {
                            context: format!("create {address}"),
                            errors: vec![format!("{identity} already exists")],
                        },
                    ));
                }
                held.insert(identity, address);
            }
            OperationKind::Apply | OperationKind::ReplaceCreate | OperationKind::Refresh => {}
        }
        Ok(())
    }
}

#[tokio::test]
async fn an_identity_handoff_between_resources_converges_when_identities_are_unique() {
    // `a` holds `shared.conf`. The new configuration gives it to `c`, and
    // `a` is replaced to hold `a2.conf`, after `c`. Creating `c` first would
    // fail, or, with a provider that overwrites, be destroyed by `a`'s delete
    // straight after: the delete of `a` has to come first.
    let runner = IdentityRunner::new(
        &[("shared.conf", "random_pet.a")],
        &[("random_pet.c", "shared.conf"), ("random_pet.a", "a2.conf")],
    );
    let scripted = run_scripted(
        vec![
            pet("a", Action::Replace, wired(&[], &["c"])),
            pet("c", Action::Create, wired(&[], &[])),
        ],
        &runner,
    )
    .await;
    assert!(scripted.result.is_ok(), "{:?}", scripted.result);
    assert_eq!(
        runner.holders(),
        [
            ("a2.conf".to_string(), "random_pet.a".to_string()),
            ("shared.conf".to_string(), "random_pet.c".to_string()),
        ]
    );
}

#[tokio::test]
async fn an_unrelated_failure_still_recreates_a_replacement() {
    let runner = ScriptedRunner::new().failing(&["apply random_pet.aaa"]);
    let scripted = run_scripted(
        vec![
            pet("aaa", Action::Update, wired(&[], &[])),
            pet("zzz", Action::Replace, wired(&[], &[])),
        ],
        &runner,
    )
    .await;
    let incomplete = scripted.incomplete();
    assert_eq!(incomplete.failures.len(), 1);
    assert_eq!(incomplete.failures[0].address.name, "aaa");
    assert!(incomplete.skipped.is_empty());
    assert!(incomplete.deleted_not_recreated.is_empty());
    assert_eq!(incomplete.completed, 1);
    assert!(scripted.uncreated().is_empty());
    assert!(
        runner.ran().contains(&"create zzz".to_string()),
        "{:?}",
        runner.ran()
    );
}

#[tokio::test]
async fn a_failed_create_is_reported_as_deleted_not_recreated_and_other_replacements_finish() {
    let runner = ScriptedRunner::new().failing(&["create random_pet.aaa"]);
    let scripted = run_scripted(
        vec![
            pet("aaa", Action::Replace, wired(&[], &[])),
            pet("zzz", Action::Replace, wired(&[], &[])),
        ],
        &runner,
    )
    .await;
    assert_eq!(
        runner.ran(),
        ["delete aaa", "create aaa", "delete zzz", "create zzz"]
    );
    let incomplete = scripted.incomplete();
    assert_eq!(Scripted::names(&incomplete.deleted_not_recreated), ["aaa"]);
    assert_eq!(incomplete.completed, 1);
    assert_eq!(scripted.uncreated(), ["aaa"]);
    assert!(
        incomplete
            .to_string()
            .contains("1 replacement(s) were deleted but not recreated"),
        "{incomplete}"
    );
}

#[tokio::test]
async fn a_replacement_whose_new_prerequisite_fails_to_create_is_left_deleted_and_reported() {
    let runner = ScriptedRunner::new().failing(&["apply random_pet.fresh"]);
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    let lock = StateLock {
        lock_identifier: "scripted".into(),
    };
    let plan = plan_of(
        in_schedule_order(vec![
            pet("r", Action::Replace, wired(&[], &["fresh"])),
            pet("fresh", Action::Create, wired(&[], &[])),
        ])
        .unwrap()
        .changes,
    );
    let mut events = Vec::new();
    let result = engine
        .converge(
            &runner,
            Convergence {
                plan: &plan,
                lock: &lock,
                on_event: &mut |event| events.push(event),
            },
        )
        .await;
    // The old object of `r` goes first, and `r`'s create waits for `fresh`,
    // which failed: `r` is reported as deleted and not recreated, not
    // skipped silently.
    assert_eq!(runner.ran(), ["delete r", "apply fresh"]);
    let Err(InfrastructureError::ApplyIncomplete(incomplete)) = result else {
        panic!("expected an incomplete apply");
    };
    assert!(incomplete.skipped.is_empty(), "{:?}", incomplete.skipped);
    assert_eq!(Scripted::names(&incomplete.deleted_not_recreated), ["r"]);
    assert!(
        events.iter().any(|event| matches!(
            event,
            ApplyEvent::DeletedNotRecreated { address } if address.name == "r"
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_replacement_whose_create_can_no_longer_follow_keeps_its_old_object() {
    // The create of `sibling` waits for the delete of `parent` (its record
    // depends on it), which waits for the delete of `child`, which fails.
    // `sibling`'s own delete is not ordered against `child`'s, but it must
    // not destroy the old object when the new one cannot follow.
    let runner = ScriptedRunner::new().failing(&["delete random_pet.child"]);
    let scripted = run_scripted(
        vec![
            pet("parent", Action::Replace, wired(&[], &[])),
            pet("child", Action::Delete, wired(&["parent"], &[])),
            pet("sibling", Action::Replace, wired(&["parent"], &[])),
        ],
        &runner,
    )
    .await;
    assert_eq!(runner.ran(), ["delete child"]);
    let incomplete = scripted.incomplete();
    assert_eq!(Scripted::names(&incomplete.skipped), ["parent", "sibling"]);
    assert!(incomplete.deleted_not_recreated.is_empty());
    assert!(scripted.uncreated().is_empty());
}

#[tokio::test]
async fn a_failure_skips_what_depends_on_it_transitively_and_nothing_else() {
    let runner = ScriptedRunner::new().failing(&["apply random_pet.a"]);
    let scripted = run_scripted(
        vec![
            pet("a", Action::Create, wired(&[], &[])),
            pet("b", Action::Create, wired(&[], &["a"])),
            pet("c", Action::Create, wired(&[], &["b"])),
            pet("d", Action::Create, wired(&[], &[])),
        ],
        &runner,
    )
    .await;
    let incomplete = scripted.incomplete();
    assert_eq!(Scripted::names(&incomplete.skipped), ["b", "c"]);
    assert_eq!(incomplete.completed, 1);
    let rendered = incomplete.to_string();
    assert!(
        rendered.contains("2 change(s) were not attempted because a change they depend on failed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("applied and recorded 1 of 4 changes"),
        "{rendered}"
    );
}

#[tokio::test]
async fn a_failed_prerequisite_leaves_no_stored_dependency_cycle_behind() {
    // N4: z's update fails. x (only refreshed, now depending on z) cannot
    // be rewritten, so y (now depending on x) must not be updated either:
    // its stored record would depend on x while x's still depends on y.
    let runner = ScriptedRunner::new().failing(&["apply random_pet.z"]);
    let scripted = run_scripted(
        vec![
            pet("x", Action::Refresh, wired(&["y"], &["z"])),
            pet("y", Action::Update, wired(&[], &["x"])),
            pet("z", Action::Update, wired(&[], &[])),
        ],
        &runner,
    )
    .await;
    assert_eq!(runner.ran(), ["apply z"]);
    let incomplete = scripted.incomplete();
    assert_eq!(incomplete.failures.len(), 1);
    assert_eq!(incomplete.failures[0].address.name, "z");
    assert_eq!(Scripted::names(&incomplete.skipped), ["x", "y"]);
}

#[test]
fn a_replacement_that_became_ready_late_is_recreated_before_unrelated_deletes() {
    // N7: after the delete of `x`, its create waits for `p`'s update. The
    // update goes before the unrelated replacement of `y`, so the window in
    // which `x` does not exist is as short as the graph allows.
    let changes = [
        pet("p", Action::Update, wired(&[], &[])),
        pet("x", Action::Replace, wired(&["p"], &["p"])),
        pet("y", Action::Replace, wired(&[], &[])),
    ];
    assert_eq!(
        short(&schedule_of(&changes)),
        ["delete x", "apply p", "create x", "delete y", "create y"]
    );
}

#[tokio::test]
async fn a_failed_replacement_delete_skips_its_create_without_calling_it_deleted() {
    let runner = ScriptedRunner::new().failing(&["delete random_pet.r"]);
    let scripted = run_scripted(vec![pet("r", Action::Replace, wired(&[], &[]))], &runner).await;
    let incomplete = scripted.incomplete();
    assert_eq!(incomplete.failures.len(), 1);
    assert!(incomplete.skipped.is_empty(), "{:?}", incomplete.skipped);
    assert!(incomplete.deleted_not_recreated.is_empty());
    assert!(scripted.uncreated().is_empty());
}

#[tokio::test]
async fn an_interrupt_between_the_halves_reports_the_replacement_it_left_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    let mut runner = ScriptedRunner::new();
    runner.stop_after = Some((
        "delete random_pet.r".to_string(),
        engine.cancellation().clone(),
    ));
    let lock = StateLock {
        lock_identifier: "scripted".into(),
    };
    let plan = plan_of(vec![pet("r", Action::Replace, wired(&[], &[]))]);
    let mut events = Vec::new();
    let result = engine
        .converge(
            &runner,
            Convergence {
                plan: &plan,
                lock: &lock,
                on_event: &mut |event| events.push(event),
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(InfrastructureError::Interrupted {
            completed: 0,
            total: 1
        })
    ));
    assert_eq!(runner.ran(), ["delete r"]);
    assert!(matches!(
        events.last(),
        Some(ApplyEvent::DeletedNotRecreated { address }) if address.name == "r"
    ));
}

#[tokio::test]
async fn a_replacement_deleted_by_the_provider_but_not_recorded_is_deleted_not_recreated() {
    // N9: the provider deleted the old object, and recording that failed.
    // The create never runs, so the object is gone and not recreated.
    let mut runner = ScriptedRunner::new();
    runner.delete_unrecorded = ["delete random_pet.x".to_string()].into_iter().collect();
    let scripted = run_scripted(vec![pet("x", Action::Replace, wired(&[], &[]))], &runner).await;
    assert_eq!(runner.ran(), ["delete x"]);
    assert!(
        matches!(scripted.result, Err(InfrastructureError::State(_))),
        "{:?}",
        scripted.result
    );
    assert_eq!(scripted.uncreated(), ["x"]);
}

#[tokio::test]
async fn a_replacement_created_but_not_recorded_is_not_called_deleted_not_recreated() {
    // The provider created the new object; the record could not be written
    // and was saved locally. The object exists, so it is not "not recreated".
    let mut runner = ScriptedRunner::new();
    runner.create_unrecorded = ["create random_pet.x".to_string()].into_iter().collect();
    let scripted = run_scripted(vec![pet("x", Action::Replace, wired(&[], &[]))], &runner).await;
    assert_eq!(runner.ran(), ["delete x", "create x"]);
    assert!(
        matches!(
            scripted.result,
            Err(InfrastructureError::UnrecordedChange { .. })
        ),
        "{:?}",
        scripted.result
    );
    assert!(scripted.uncreated().is_empty(), "{:?}", scripted.events);
    assert!(
        scripted.events.iter().any(|event| matches!(
            event,
            ApplyEvent::Warning(warning)
                if warning.contains("random_pet.x") && warning.contains("could not be recorded")
        )),
        "{:?}",
        scripted.events
    );
}

#[tokio::test]
async fn a_fatal_failure_ends_the_run_and_keeps_earlier_failures_visible() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    let mut runner = ScriptedRunner::new().failing(&["apply random_pet.a"]);
    runner.aborting = ["apply random_pet.b".to_string()].into_iter().collect();
    let lock = StateLock {
        lock_identifier: "scripted".into(),
    };
    let plan = plan_of(
        in_schedule_order(vec![
            pet("a", Action::Create, wired(&[], &[])),
            pet("b", Action::Create, wired(&[], &[])),
            pet("c", Action::Create, wired(&[], &[])),
        ])
        .unwrap()
        .changes,
    );
    let mut events = Vec::new();
    let result = engine
        .converge(
            &runner,
            Convergence {
                plan: &plan,
                lock: &lock,
                on_event: &mut |event| events.push(event),
            },
        )
        .await;
    assert!(
        matches!(result, Err(InfrastructureError::State(_))),
        "{result:?}"
    );
    assert_eq!(runner.ran(), ["apply a", "apply b"]);
    assert!(events.iter().any(|event| matches!(
        event,
        ApplyEvent::Warning(warning) if warning.contains("random_pet.a failed earlier")
    )));
}

#[tokio::test]
async fn missing_providers_name_the_environment_path_and_the_inheritance_rule() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    engine.tenant = TenantKey::with_environment("example.com/app", "web", "dev").unwrap();
    let error = engine
        .ensure_provider("undeclared", &mut Vec::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(
            "provider 'undeclared' is not declared in infrastructure.environments.dev.providers"
        ),
        "{error}"
    );
    assert!(
        error.contains("top-level `infrastructure.providers` are not inherited by environments"),
        "{error}"
    );
}

#[tokio::test]
async fn recreate_over_an_existing_row_recovers_after_a_lost_write_response() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(Arc::clone(&store), directory.path());
    let lock = store.lock(&tenant(), "lost response").await.unwrap();
    store
        .put(&tenant(), &lock, &record("random_pet", "pet", &[]))
        .await
        .unwrap();
    let prior = store.list(&tenant()).await.unwrap()[0].clone();
    let expected = RecordVersion::of(Some(&prior));
    // Refresh found the remote object missing. The create result overwrites
    // its retained row, so its payload must identify that same insertion.
    let recreated = ManagedResource {
        state: json!({"id": "recreated"}),
        generation: write_generation(expected),
        ..prior.clone()
    };
    let put = ConditionalPut {
        resource: &recreated,
        expected,
    };
    store
        .put_if_unchanged(&tenant(), &lock, &put)
        .await
        .unwrap();
    let saved = engine.save_unrecorded(&put, &InfrastructureError::state("write response lost"));
    assert!(matches!(
        saved,
        InfrastructureError::UnrecordedChange { .. }
    ));
    engine
        .unrecorded_store()
        .unwrap()
        .recover(
            store.as_ref(),
            &tenant(),
            &crate::unrecorded::RecoverOptions {
                lock: &lock,
                overrides: crate::unrecorded::RecoverOverrides::default(),
            },
        )
        .await
        .unwrap();
    let current = store.list(&tenant()).await.unwrap()[0].clone();
    assert_eq!(current.generation, prior.generation);
    assert_eq!(current.serial, prior.serial + 1);
    assert!(current.same_content(&recreated));
    assert!(
        !engine
            .unrecorded_store()
            .unwrap()
            .has_pending(&tenant())
            .unwrap()
    );
}

#[test]
fn interrupted_apply_without_a_provider_response_reports_the_unknown_resource() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = engine(store, directory.path());
    engine.cancellation().stop();
    let address = ResourceAddress::new("random_pet", "pet");
    let error = engine.interrupted_or(
        InfrastructureError::RemoteProcedure {
            method: "ApplyResourceChange".into(),
            status: Box::new(tonic::Status::unavailable("response lost")),
        },
        InterruptedOperation {
            address: &address,
            progress: Progress {
                completed: 0,
                total: 1,
            },
            on_event: &mut |_| {},
        },
    );
    assert!(
        matches!(error, InfrastructureError::InterruptedUnknownOutcome { ref address, completed: 0, total: 1 }
        if address == "random_pet.pet")
    );
    assert!(
        error
            .to_string()
            .contains("outcome for random_pet.pet is unknown")
    );
    assert!(
        !error
            .to_string()
            .contains("every applied change is recorded")
    );
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
    let mut unchanged = change(stored.address.clone(), Action::Refresh);
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

    let mut different_environment = base();
    different_environment.environment_identity =
        environment_identity(&BTreeMap::from([("CREDENTIAL".into(), "changed".into())]));
    assert_ne!(base().digest(), different_environment.digest());

    let mut development = base();
    development.tenant = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
    let mut staging = base();
    staging.tenant = TenantKey::with_environment("example.com/app", "web", "Staging").unwrap();
    assert_ne!(development.digest(), staging.digest());
    assert_ne!(base().digest(), development.digest());
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
            provider_environment_variables: BTreeMap::new(),
            unrecorded_directory: Some(unrecorded_directory.join("unrecorded")),
            cancellation: Cancellation::default(),
        },
    })
}

/// Store the record `refresh_only(name)` was planned from.
async fn store_planned_record(store: &dyn StateStore, name: &str) -> ResourceChange {
    let lock = store.lock(&tenant(), "setup").await.unwrap();
    store
        .put(&tenant(), &lock, &record("random_pet", name, &[]))
        .await
        .unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();
    let mut change = refresh_only(name);
    change.stored = store
        .list(&tenant())
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.address.name == name);
    change
}

#[tokio::test]
async fn apply_writes_refresh_only_records_under_the_lock() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let change = store_planned_record(store.as_ref(), "pet").await;
    let lock = store.lock(&tenant(), "test").await.unwrap();
    let plan = plan_of(vec![change]);
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
    let change = store_planned_record(store.as_ref(), "pet").await;
    let stale = StateLock {
        lock_identifier: "not-held".into(),
    };
    let error = engine
        .apply(
            &plan_of(vec![change]),
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
    let change = store_planned_record(store.as_ref(), "pet").await;
    let lock = store.lock(&tenant(), "test").await.unwrap();
    engine.cancellation().stop();
    let error = engine
        .apply(
            &plan_of(vec![change]),
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
    assert_eq!(
        store.list(&tenant()).await.unwrap()[0].state["id"],
        "stored"
    );
}

#[tokio::test]
async fn identical_contents_recreated_at_the_same_serial_make_a_plan_stale() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let change = store_planned_record(store.as_ref(), "pet").await;
    let planned = change.stored.clone().unwrap();
    let lock = store.lock(&tenant(), "test").await.unwrap();
    store
        .delete(&tenant(), &lock, &planned.address)
        .await
        .unwrap();
    store.put(&tenant(), &lock, &planned).await.unwrap();
    let recreated = store.list(&tenant()).await.unwrap()[0].clone();
    assert_eq!(recreated.serial, planned.serial);
    assert!(recreated.same_content(&planned));
    assert_ne!(recreated.generation, planned.generation);
    let plan = plan_of(vec![change]);
    let error = engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(error, InfrastructureError::PlanOutdated { .. }));
    assert_eq!(store.list(&tenant()).await.unwrap(), vec![recreated]);
}

#[tokio::test]
async fn stale_plans_are_refused_before_anything_is_written() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(Arc::clone(&store), directory.path());
    let change = store_planned_record(store.as_ref(), "pet").await;
    let lock = store.lock(&tenant(), "test").await.unwrap();
    // Another run rewrote the record after the plan was made: same content,
    // newer serial.
    store
        .put(&tenant(), &lock, &record("random_pet", "pet", &[]))
        .await
        .unwrap();
    let plan = plan_of(vec![change]);
    let error = engine
        .apply(&plan, ApplyContext { lock: &lock }, &mut |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(&error, InfrastructureError::PlanOutdated { address } if address == "random_pet.pet"),
        "{error}"
    );
    assert_eq!(
        store.list(&tenant()).await.unwrap()[0].state["id"],
        "stored"
    );

    // A record the plan never saw is refused too.
    let appeared = plan_of(Vec::new());
    let error = engine
        .apply(&appeared, ApplyContext { lock: &lock }, &mut |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(error, InfrastructureError::PlanOutdated { .. }),
        "{error}"
    );
}

/// A store whose record writes fail with a state error.
#[derive(Debug, Default)]
struct FailingWrites {
    inner: MemoryStateStore,
}

#[async_trait::async_trait]
impl StateStore for FailingWrites {
    async fn migrate(&self) -> Result<()> {
        self.inner.migrate().await
    }

    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        self.inner.list(tenant).await
    }

    async fn put(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _resource: &ManagedResource,
    ) -> Result<()> {
        Err(InfrastructureError::state("Turso returned HTTP 503"))
    }

    async fn put_if_unchanged(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _put: &ConditionalPut<'_>,
    ) -> Result<()> {
        Err(InfrastructureError::state("Turso returned HTTP 503"))
    }

    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        self.inner.delete(tenant, lock, address).await
    }

    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &crate::state::LockRequest<'_>,
    ) -> Result<StateLock> {
        self.inner.acquire_lock(tenant, request).await
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        self.inner.unlock(tenant, lock).await
    }

    async fn current_lock(
        &self,
        tenant: &TenantKey,
    ) -> Result<Option<crate::state::LockInformation>> {
        self.inner.current_lock(tenant).await
    }

    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
        self.inner.force_unlock(tenant, lock_identifier).await
    }

    async fn owner(&self, tenant: &TenantKey) -> Result<Option<crate::state::TenantOwner>> {
        self.inner.owner(tenant).await
    }

    async fn claim_owner(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        claim: &crate::state::OwnerClaim<'_>,
    ) -> Result<crate::state::TenantOwner> {
        self.inner.claim_owner(tenant, lock, claim).await
    }
}

#[tokio::test]
async fn failed_refresh_writes_name_their_address() {
    let directory = tempfile::tempdir().unwrap();
    let failing = FailingWrites::default();
    let change = store_planned_record(&failing.inner, "pet").await;
    let store: Arc<dyn StateStore> = Arc::new(failing);
    let mut engine = engine(Arc::clone(&store), directory.path());
    let lock = store.lock(&tenant(), "test").await.unwrap();
    let error = engine
        .apply(
            &plan_of(vec![change]),
            ApplyContext { lock: &lock },
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(error, InfrastructureError::State(_)), "{error}");
    assert!(error.to_string().contains("random_pet.pet"), "{error}");
    assert!(error.to_string().contains("HTTP 503"), "{error}");
}

#[test]
fn inconsistent_create_and_update_results_are_provider_errors() {
    let address = ResourceAddress::new("example_thing", "one");
    let block = name_block();
    let step = |kind| ApplyStep {
        kind,
        prior: Vec::new(),
        planned: Vec::new(),
        configuration: Vec::new(),
        planned_private: Vec::new(),
        planned_value: named("planned"),
    };
    let check = |step: &ApplyStep, returned: &Value| {
        inconsistent_result(&InconsistentResultCheck {
            address: &address,
            block: &block,
            step,
            returned,
        })
    };
    let create = step(StepKind::Create);
    assert!(check(&create, &named("planned")).is_none());
    let problem = check(&create, &named("drifted")).unwrap();
    assert!(problem.contains("inconsistent result after the create of example_thing.one"));
    assert!(problem.contains("name: planned value changed after apply"));
    assert!(!problem.contains("drifted"));
    assert!(check(&step(StepKind::Update), &named("drifted")).is_some());
    assert!(check(&step(StepKind::Delete), &Value::Null).is_none());
}

#[test]
fn undecodable_apply_results_report_the_provider_errors() {
    let value_type = Type::from_json(&json!(["object", {"id": "string"}])).unwrap();
    let error_diagnostic = Diagnostic {
        severity: Severity::Error as i32,
        summary: "create failed".into(),
        detail: "quota exceeded".into(),
        attribute: None,
    };
    let response = |diagnostics: Vec<Diagnostic>| protocol::ApplyResourceChangeResponse {
        // A number where the schema says string: cannot be decoded.
        new_state: Some(protocol::DynamicValue {
            message_pack: vec![0x81, 0xa2, b'i', b'd', 0x07],
            json: Vec::new(),
        }),
        private: Vec::new(),
        diagnostics,
        legacy_type_system: false,
    };
    let error = applied_value(&response(vec![error_diagnostic]), &value_type, "apply x")
        .unwrap_err()
        .to_string();
    assert!(error.contains("quota exceeded"), "{error}");
    let error = applied_value(&response(Vec::new()), &value_type, "apply x")
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not match type"), "{error}");
}

#[test]
fn upgraded_states_must_exist_and_be_known() {
    let address = ResourceAddress::new("example_thing", "one");
    assert!(check_upgraded(&address, &named("x")).is_ok());
    assert!(check_upgraded(&address, &Value::Null).is_err());
    let unknown = Value::Object(BTreeMap::from([("name".to_string(), Value::Unknown)]));
    let error = check_upgraded(&address, &unknown).unwrap_err().to_string();
    assert!(error.contains("unknown values while upgrading"), "{error}");
    assert!(error.contains("example_thing.one"), "{error}");
}

#[test]
fn refresh_changes_have_their_own_action_name() {
    assert_eq!(Action::Refresh.name(), "refresh");
    assert!(!Action::Refresh.changes_infrastructure());
    assert!(Action::Delete.changes_infrastructure());
    let plan = plan_of(vec![refresh_only("pet")]);
    assert_eq!(plan.changes[0].action.name(), "refresh");
}

#[tokio::test]
async fn planning_refuses_while_unrecorded_changes_are_pending() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let mut engine = engine(store, directory.path());
    engine
        .unrecorded_store()
        .unwrap()
        .save(
            &tenant(),
            &ConditionalPut {
                resource: &record("random_pet", "pet", &[]),
                expected: RecordVersion::Absent,
            },
        )
        .unwrap();
    let error = engine.plan(PlanMode::Apply).await.unwrap_err();
    assert!(
        matches!(
            error,
            InfrastructureError::UnrecordedChangesPending { count: 1, .. }
        ),
        "{error}"
    );
    // The library says what to do, not which command line does it.
    assert!(
        error.to_string().contains("recover them before planning"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("cuenv infrastructure"),
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
    let resource_record = record("random_pet", "pet", &[]);
    let replaced = RecordVersion::Generation {
        generation: uuid::Uuid::from_u128(3),
        serial: 3,
    };
    let pending_write = ConditionalPut {
        resource: &resource_record,
        expected: replaced,
    };
    let saved = engine.save_unrecorded(&pending_write, &cause);
    let message = saved.to_string();
    assert!(
        matches!(saved, InfrastructureError::UnrecordedChange { .. }),
        "{message}"
    );
    assert!(message.contains("random_pet.pet"), "{message}");
    assert!(message.contains("recover the saved changes"), "{message}");
    assert!(!message.contains("state recover"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    let listed = engine.unrecorded_store().unwrap().list(&tenant()).unwrap();
    assert_eq!(listed.len(), 1);
    // The saved record remembers which stored version it replaces.
    assert_eq!(listed[0].expected, replaced);

    // When even the local save fails, the error names the address and the
    // kind of local failure only.
    let blocked = tempfile::tempdir().unwrap();
    std::fs::write(blocked.path().join("unrecorded"), b"not a directory").unwrap();
    let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
    let engine = self::engine(store, blocked.path());
    let lost = engine.save_unrecorded(&pending_write, &cause);
    let message = lost.to_string();
    assert!(
        matches!(lost, InfrastructureError::UnrecordedChangeLost { .. }),
        "{message}"
    );
    assert!(message.contains("random_pet.pet"), "{message}");
    assert!(message.contains("not usable"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    assert!(
        !message.contains(&blocked.path().display().to_string()),
        "{message}"
    );
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
            provider_environment_variables: BTreeMap::from([(
                "CREDENTIAL".into(),
                "must-never-appear-in-debug".into(),
            )]),
            unrecorded_directory: None,
            cancellation: Cancellation::default(),
        },
    };
    let rendered = format!("{setup:?}");
    assert!(rendered.contains("EngineSetup"), "{rendered}");
    assert!(rendered.contains("example.com/app"), "{rendered}");
    assert!(
        !rendered.contains("must-never-appear-in-debug"),
        "{rendered}"
    );
}
