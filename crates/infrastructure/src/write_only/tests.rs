use super::*;
use crate::references::ConfigurationReferences;
use crate::schema::{Documentation, NestedAttributes};
use serde_json::json;

fn attribute() -> Attribute {
    Attribute {
        value_type: Type::String,
        nested: None,
        presence: Presence::Optional,
        sensitive: false,
        write_only: false,
        documentation: Documentation::default(),
    }
}

fn block() -> Block {
    Block {
        attributes: BTreeMap::from([
            ("value".into(), attribute()),
            (
                "value_wo".into(),
                Attribute {
                    write_only: true,
                    ..attribute()
                },
            ),
        ]),
        ..Block::default()
    }
}

fn references(configuration: &serde_json::Value) -> Result<ConfigurationReferences> {
    let mut references = ConfigurationReferences::extract_with_schema(
        configuration
            .as_object()
            .ok_or_else(|| InfrastructureError::configuration("expected object"))?,
        &block(),
        &[],
    )?;
    references.bind_write_only("example.com/test/fake", "fake_write_only", &block())?;
    Ok(references)
}

#[test]
fn composed_value_selects_verified_provider_path() -> Result<()> {
    let references =
        references(&json!({"value":{"resolver":"exec","command":"unused","writeOnly":true}}))?;
    let [argument] = references.secrets() else {
        return Err(InfrastructureError::configuration("expected one secret"));
    };
    assert_eq!(argument.destination.to_string(), "value");
    assert_eq!(
        argument
            .write_only
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("value_wo")
    );
    assert!(argument.secret.descriptor().contains_key("writeOnly"));
    Ok(())
}

#[test]
fn missing_invalid_or_conflicting_bindings_fail_closed() {
    for configuration in [
        json!({"value_wo":"literal"}),
        json!({"value_wo":null}),
        json!({"value_wo":{"resolver":"exec","writeOnly":true}}),
        json!({"value":{"resolver":"exec","writeOnly":false}}),
        json!({"value":{"resolver":"exec","writeOnly":null}}),
        json!({"value":{"resolver":"exec","writeOnly":"true"}}),
        json!({"value":{"resolver":"exec","writeOnly":true},"value_wo":{"resolver":"exec","writeOnly":true}}),
    ] {
        assert!(references(&configuration).is_err());
    }
    let configuration = json!({"value":{"resolver":"exec","writeOnly":true}});
    let mut references = ConfigurationReferences::extract_with_schema(
        configuration.as_object().unwrap(),
        &block(),
        &[],
    )
    .unwrap();
    assert!(
        references
            .bind_write_only("example.com/other/fake", "fake_write_only", &block())
            .is_err()
    );
    let mut required = block();
    required.attributes.get_mut("value").unwrap().presence = Presence::Required;
    assert!(verified_binding(&required, "value", "value_wo").is_err());
}

#[test]
fn proposed_state_is_null_and_provider_echoes_are_errors() -> Result<()> {
    let block = block();
    let configuration = Value::from_configuration_json(
        &json!({"value_wo":"test-write-only-secret-731"}),
        &block.implied_type(),
    )?;
    let proposed = block.proposed_new(&Value::Null, &configuration);
    assert!(proposed.attribute("value_wo").is_some_and(Value::is_null));
    assert!(
        crate::object_change::plan_problems(
            &block,
            crate::object_change::PlanValues {
                prior: &Value::Null,
                configuration: &configuration,
                planned: &proposed,
            }
        )
        .is_empty()
    );
    for value in [
        Value::String("test-write-only-secret-731".into()),
        Value::Unknown,
    ] {
        let state = Value::Object(BTreeMap::from([("value_wo".into(), value)]));
        let error = check_state(&block, &state, "test").unwrap_err().to_string();
        assert!(error.contains("non-null write-only"));
        assert!(!error.contains("test-write-only-secret-731"));
    }
    Ok(())
}

#[test]
fn nested_state_checks_hide_map_keys_and_reject_unknown_leaves() {
    let nested = NestedAttributes {
        attributes: block().attributes,
        nesting: Nesting::Map,
    };
    let block = Block {
        attributes: BTreeMap::from([(
            "credentials".into(),
            Attribute {
                value_type: Type::Map(Box::new(block().implied_type())),
                nested: Some(nested),
                ..attribute()
            },
        )]),
        ..Block::default()
    };
    let value = Value::Object(BTreeMap::from([(
        "credentials".into(),
        Value::Object(BTreeMap::from([(
            "hidden-private-map-key".into(),
            Value::Object(BTreeMap::from([("value_wo".into(), Value::Unknown)])),
        )])),
    )]));
    let error = check_state(&block, &value, "test").unwrap_err().to_string();
    assert!(error.contains("credentials[<hidden key>].value_wo"));
    assert!(!error.contains("hidden-private-map-key"));
}

#[test]
fn unsupported_contexts_reject_markers_at_any_depth() {
    for value in [
        json!({"resolver":"exec","writeOnly":true}),
        json!({"nested":[{"resolver":"exec","writeOnly":true}]}),
    ] {
        assert!(reject_markers(&value).is_err());
    }
}

#[test]
fn unknown_write_only_ancestors_fail_instead_of_being_nulled() {
    for nesting in [Nesting::Single, Nesting::List, Nesting::Map] {
        let schema = Block {
            attributes: BTreeMap::from([(
                "credentials".into(),
                Attribute {
                    nested: Some(NestedAttributes {
                        attributes: block().attributes,
                        nesting,
                    }),
                    value_type: Type::Dynamic,
                    ..attribute()
                },
            )]),
            ..Block::default()
        };
        for value in [
            Value::Unknown,
            Value::Object(BTreeMap::from([("credentials".into(), Value::Unknown)])),
        ] {
            assert!(check_state(&schema, &value, "test").is_err());
        }
        assert!(
            check_state(
                &schema,
                &Value::Object(BTreeMap::from([("credentials".into(), Value::Null)])),
                "test"
            )
            .is_ok()
        );
    }
}

#[test]
fn ordinary_provider_objects_keep_their_write_only_named_fields() -> Result<()> {
    let schema = Block {
        attributes: BTreeMap::from([(
            "options".into(),
            Attribute {
                value_type: Type::Object(BTreeMap::from([
                    ("writeOnly".into(), Type::Boolean),
                    ("resolver".into(), Type::String),
                ])),
                ..attribute()
            },
        )]),
        ..Block::default()
    };
    let configuration = json!({"options":{"writeOnly":true,"resolver":"ordinary-provider-field"}});
    let references = ConfigurationReferences::extract_with_schema(
        configuration.as_object().unwrap(),
        &schema,
        &[],
    )?;
    assert!(references.secrets().is_empty());
    Ok(())
}

#[test]
fn required_nested_write_only_children_and_computed_flags_are_checked() {
    let mut attributes = block().attributes;
    attributes.get_mut("value_wo").unwrap().presence = Presence::Required;
    let schema = Block {
        attributes: BTreeMap::from([(
            "credentials".into(),
            Attribute {
                value_type: Type::Dynamic,
                nested: Some(NestedAttributes {
                    attributes,
                    nesting: Nesting::Single,
                }),
                ..attribute()
            },
        )]),
        ..Block::default()
    };
    let configuration = json!({"credentials":{}});
    assert!(
        ConfigurationReferences::extract_with_schema(
            configuration.as_object().unwrap(),
            &schema,
            &[]
        )
        .is_err()
    );
    let mut schema = block();
    schema.attributes.get_mut("value_wo").unwrap().presence = Presence::OptionalComputed;
    let configuration = json!({"value_wo":{"resolver":"exec","writeOnly":true}});
    assert!(
        ConfigurationReferences::extract_with_schema(
            configuration.as_object().unwrap(),
            &schema,
            &[]
        )
        .is_err()
    );
}
