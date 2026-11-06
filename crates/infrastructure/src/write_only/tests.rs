use super::*;
use crate::references::ConfigurationReferences;
use crate::schema::{Documentation, NestedAttributes, NestedBlock, Presence};
use crate::type_system::Type;
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
    ConfigurationReferences::extract_with_schema(
        configuration
            .as_object()
            .ok_or_else(|| InfrastructureError::configuration("expected object"))?,
        &block(),
        &[],
    )
}

#[test]
fn write_only_uses_the_native_provider_path() -> Result<()> {
    let references =
        references(&json!({"value_wo":{"resolver":"exec","command":"unused","writeOnly":true}}))?;
    let [argument] = references.secrets() else {
        return Err(InfrastructureError::configuration("expected one secret"));
    };
    assert_eq!(argument.destination.to_string(), "value_wo");
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
fn only_schema_marked_arguments_accept_write_only_secrets() {
    for configuration in [
        json!({"value_wo":"literal"}),
        json!({"value_wo":{"resolver":"exec"}}),
        json!({"value":{"resolver":"exec","writeOnly":true}}),
        json!({"value":{"resolver":"exec","writeOnly":false}}),
        json!({"value":{"resolver":"exec","writeOnly":null}}),
        json!({"value":{"resolver":"exec","writeOnly":"true"}}),
        json!({"value":{"resolver":"exec","writeOnly":true},"value_wo":{"resolver":"exec","writeOnly":true}}),
    ] {
        assert!(references(&configuration).is_err());
    }
}

#[test]
fn optional_write_only_null_is_absence_and_required_null_is_rejected() -> Result<()> {
    let configuration = json!({"value_wo":null});
    let references = references(&configuration)?;
    assert!(references.secrets().is_empty());
    let prepared =
        references.prepare(configuration.as_object().unwrap(), &block().implied_type())?;
    let absent = Value::from_configuration_json(&json!({}), &block().implied_type())?;
    assert_eq!(prepared, absent);
    for presence in [
        Presence::Required,
        Presence::Computed,
        Presence::OptionalComputed,
    ] {
        let mut schema = block();
        schema.attributes.get_mut("value_wo").unwrap().presence = presence;
        assert!(
            ConfigurationReferences::extract_with_schema(
                configuration.as_object().unwrap(),
                &schema,
                &[]
            )
            .is_err()
        );
    }
    let mut schema = block();
    schema.attributes.get_mut("value_wo").unwrap().value_type = Type::Number;
    assert!(
        ConfigurationReferences::extract_with_schema(
            configuration.as_object().unwrap(),
            &schema,
            &[]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn ordinary_and_write_only_arguments_are_independent() -> Result<()> {
    let configuration = json!({
        "value": {"resolver":"exec","command":"ordinary"},
        "value_wo": {"resolver":"exec","command":"write-only","writeOnly":true},
    });
    let references = references(&configuration)?;
    assert_eq!(references.secrets().len(), 2);
    assert!(references.secrets()[0].write_only.is_none());
    assert_eq!(
        references.secrets()[1].write_only.as_ref(),
        Some(&references.secrets()[1].destination)
    );
    let prepared =
        references.prepare(configuration.as_object().unwrap(), &block().implied_type())?;
    assert!(prepared.attribute("value").is_some_and(Value::is_unknown));
    assert!(
        prepared
            .attribute("value_wo")
            .is_some_and(Value::is_unknown)
    );
    Ok(())
}

#[test]
fn attribute_suffixes_do_not_grant_write_only_behavior() {
    for name in ["value", "value_wo", "token"] {
        for write_only in [false, true] {
            let schema = Block {
                attributes: BTreeMap::from([(
                    name.into(),
                    Attribute {
                        write_only,
                        ..attribute()
                    },
                )]),
                ..Block::default()
            };
            let configuration = json!({name: {"resolver":"exec","writeOnly":true}});
            let result = ConfigurationReferences::extract_with_schema(
                configuration.as_object().unwrap(),
                &schema,
                &[],
            );
            assert_eq!(
                result.is_ok(),
                write_only,
                "{name}: write_only={write_only}"
            );
            if let Ok(references) = result {
                let argument = &references.secrets()[0];
                assert_eq!(argument.destination.to_string(), name);
                assert_eq!(argument.write_only.as_ref(), Some(&argument.destination));
            }
        }
    }
}

#[test]
fn nested_write_only_arguments_keep_provider_paths() -> Result<()> {
    for nesting in [
        Nesting::Single,
        Nesting::Group,
        Nesting::List,
        Nesting::Map,
        Nesting::Set,
    ] {
        let (value, path) = match nesting {
            Nesting::Single | Nesting::Group => (
                json!({"value_wo":{"resolver":"exec","writeOnly":true}}),
                "credentials.value_wo",
            ),
            Nesting::List | Nesting::Set => (
                json!([{"value_wo":{"resolver":"exec","writeOnly":true}}]),
                "credentials[0].value_wo",
            ),
            Nesting::Map => (
                json!({"value_wo":{"value_wo":{"resolver":"exec","writeOnly":true}}}),
                "credentials[<hidden key>].value_wo",
            ),
        };
        let configuration = json!({"credentials": value});
        let nested = NestedAttributes {
            attributes: block().attributes,
            nesting,
        };
        let attribute_schema = Block {
            attributes: BTreeMap::from([(
                "credentials".into(),
                Attribute {
                    value_type: match nesting {
                        Nesting::Single | Nesting::Group => block().implied_type(),
                        Nesting::List => Type::List(Box::new(block().implied_type())),
                        Nesting::Set => Type::Set(Box::new(block().implied_type())),
                        Nesting::Map => Type::Map(Box::new(block().implied_type())),
                    },
                    nested: Some(nested),
                    ..attribute()
                },
            )]),
            ..Block::default()
        };
        let block_schema = Block {
            blocks: BTreeMap::from([(
                "credentials".into(),
                NestedBlock {
                    block: block(),
                    nesting,
                    minimum_items: 0,
                    maximum_items: 0,
                },
            )]),
            ..Block::default()
        };
        for schema in [attribute_schema, block_schema] {
            let result = ConfigurationReferences::extract_with_schema(
                configuration.as_object().unwrap(),
                &schema,
                &[],
            );
            if nesting == Nesting::Set {
                assert!(result.is_err());
                continue;
            }
            let references = result?;
            let argument = &references.secrets()[0];
            assert_eq!(argument.destination.to_string(), path);
            assert_eq!(argument.write_only.as_ref(), Some(&argument.destination));
            let prepared =
                references.prepare(configuration.as_object().unwrap(), &schema.implied_type())?;
            assert!(
                argument
                    .destination
                    .value(&prepared)
                    .is_some_and(Value::is_unknown)
            );
            let mut optional_null = configuration.clone();
            argument
                .destination
                .replace_json(&mut optional_null, serde_json::Value::Null)?;
            let null_references = ConfigurationReferences::extract_with_schema(
                optional_null.as_object().unwrap(),
                &schema,
                &[],
            )?;
            assert!(null_references.secrets().is_empty());
            let prepared = null_references
                .prepare(optional_null.as_object().unwrap(), &schema.implied_type())?;
            assert!(
                argument
                    .destination
                    .value(&prepared)
                    .is_some_and(Value::is_null)
            );
        }
    }
    Ok(())
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
