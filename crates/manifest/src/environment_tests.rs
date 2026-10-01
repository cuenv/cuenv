use super::*;
use serde_json::json;

fn error_of(value: serde_json::Value) -> String {
    serde_json::from_value::<EnvValue>(value)
        .unwrap_err()
        .to_string()
}

#[test]
fn misspelled_policy_field_is_rejected() {
    let error = error_of(json!({
        "value": "x",
        "policies": [{"allowInfrastucture": ["plan"]}],
    }));
    assert!(
        error.contains("unknown field `allowInfrastucture`"),
        "{error}"
    );
}

#[test]
fn policy_accepts_every_documented_field() {
    let policy: Policy = serde_json::from_value(json!({
        "allowTasks": ["build"],
        "allowExec": ["make"],
        "allowInfrastructure": ["plan", "state-list", "unlock"],
    }))
    .unwrap();
    assert_eq!(
        policy.allow_infrastructure,
        Some(vec![
            InfrastructurePolicyAction::Plan,
            InfrastructurePolicyAction::StateList,
            InfrastructurePolicyAction::Unlock,
        ])
    );
}

#[test]
fn invalid_infrastructure_action_names_are_rejected() {
    for name in ["deploy", "Plan", "state_list", "state", ""] {
        let error = serde_json::from_value::<Policy>(json!({"allowInfrastructure": [name]}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown variant"), "{name}: {error}");
        assert!(error.contains("state-adopt"), "{name}: {error}");
    }
}

#[test]
fn every_action_round_trips_through_its_schema_name() {
    let names = [
        "plan",
        "apply",
        "destroy",
        "state-list",
        "state-remove",
        "state-recover",
        "state-adopt",
        "unlock",
    ];
    for name in names {
        let action: InfrastructurePolicyAction = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(action.as_str(), name);
        assert_eq!(action.to_string(), name);
        assert_eq!(serde_json::to_value(action).unwrap(), json!(name));
    }
}

#[test]
fn value_with_policies_deserializes_next_to_secrets() {
    let plain: EnvValue = serde_json::from_value(json!({
        "value": "x",
        "policies": [{"allowInfrastructure": ["plan"]}],
    }))
    .unwrap();
    assert!(matches!(plain, EnvValue::WithPolicies(_)));
    assert!(plain.is_accessible_by_infrastructure(InfrastructurePolicyAction::Plan));
    assert!(!plain.is_accessible_by_infrastructure(InfrastructurePolicyAction::Apply));

    let secret: EnvValue =
        serde_json::from_value(json!({"resolver": "exec", "command": "echo", "args": ["hi"]}))
            .unwrap();
    assert!(matches!(secret, EnvValue::Secret(_)));

    let onepassword: EnvValue =
        serde_json::from_value(json!({"resolver": "onepassword", "ref": "op://v/i/f"})).unwrap();
    assert!(matches!(onepassword, EnvValue::Secret(_)));

    let protected: EnvValue = serde_json::from_value(json!({
        "value": {"resolver": "exec", "command": "echo"},
        "policies": [{"allowTasks": ["deploy"]}],
    }))
    .unwrap();
    assert!(matches!(
        protected,
        EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::Secret(_),
            ..
        })
    ));
}

#[test]
fn simple_values_keep_their_variants() {
    assert!(matches!(
        serde_json::from_value::<EnvValue>(json!("text")).unwrap(),
        EnvValue::String(_)
    ));
    assert!(matches!(
        serde_json::from_value::<EnvValue>(json!(7)).unwrap(),
        EnvValue::Int(7)
    ));
    assert!(matches!(
        serde_json::from_value::<EnvValue>(json!(true)).unwrap(),
        EnvValue::Bool(true)
    ));
    assert!(matches!(
        serde_json::from_value::<EnvValue>(json!(["a", {"resolver": "exec", "command": "x"}]))
            .unwrap(),
        EnvValue::Interpolated(_)
    ));
}

#[test]
fn incomplete_values_say_so() {
    let error = error_of(json!(null));
    assert!(error.contains("incomplete CUE value"), "{error}");
    let error = error_of(json!(1.5));
    assert!(error.contains("must be an integer"), "{error}");
}

#[test]
fn incomplete_overlay_value_is_named_in_the_environment_error() {
    let error = serde_json::from_value::<Env>(json!({
        "environment": {"staging": {"TOKEN": null}},
    }))
    .unwrap_err()
    .to_string();
    assert!(error.contains("incomplete CUE value"), "{error}");
}

#[test]
fn secret_without_resolver_names_the_secret() {
    let error = error_of(json!({"command": "echo"}));
    assert!(error.contains("secret"), "{error}");
    assert!(error.contains("resolver"), "{error}");
}

#[test]
fn interpolated_value_error_names_an_environment_variable() {
    // The module loader adds the `env.NAME` path and the value hint only to
    // messages that name an environment variable.
    let error = error_of(json!(["a", {"command": "echo"}]));
    assert!(error.contains("environment variable"), "{error}");
}

#[test]
fn passthrough_is_rejected_in_the_project_environment_with_the_reason() {
    let error = error_of(json!({"cuenvPassthrough": true, "name": "USER"}));
    assert!(error.contains("#EnvPassthrough"), "{error}");
    assert!(error.contains("task's `env`"), "{error}");
    assert!(!error.contains("resolver"), "{error}");
}

#[test]
fn null_value_inside_policies_says_the_value_is_incomplete() {
    let error = error_of(json!({"value": null, "policies": []}));
    assert!(error.contains("incomplete CUE value"), "{error}");
}

fn restricted(policies: &serde_json::Value) -> EnvValue {
    serde_json::from_value(json!({"value": "y", "policies": policies})).unwrap()
}

#[test]
fn shell_receives_only_unrestricted_values() {
    assert!(EnvValue::String("x".into()).is_accessible_by_shell());
    assert!(restricted(&json!([])).is_accessible_by_shell());
    assert!(
        serde_json::from_value::<EnvValue>(json!({"value": "y"}))
            .unwrap()
            .is_accessible_by_shell()
    );
    assert!(!restricted(&json!([{"allowTasks": ["build"]}])).is_accessible_by_shell());
    assert!(!restricted(&json!([{"allowExec": ["make"]}])).is_accessible_by_shell());
    assert!(!restricted(&json!([{"allowInfrastructure": ["plan"]}])).is_accessible_by_shell());
}
