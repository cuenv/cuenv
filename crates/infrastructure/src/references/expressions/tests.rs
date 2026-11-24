use std::collections::BTreeMap;

use serde_json::json;

use super::*;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

/// A reference to an output of the `bucket` resource.
fn output(path: &[&str], kind: &str) -> Json {
    json!({
        "cuenvOutput": {"namespace": "resources", "name": "bucket", "environment": "", "path": path},
        "type": kind,
    })
}

fn bucket() -> SourceAddress {
    SourceAddress::Resource(ResourceAddress::new("test_bucket", "bucket"))
}

fn consumer() -> SourceAddress {
    SourceAddress::Resource(ResourceAddress::new("test_object", "object"))
}

fn identity(attribute: OutputPath) -> OutputIdentity {
    OutputIdentity {
        source: bucket(),
        attribute,
    }
}

fn configuration() -> Json {
    json!({
        "resources": {
            "bucket": {
                "type": "test_bucket",
                "configuration": {"name": "assets"},
                "outputs": {
                    "id": output(&["id"], "string"),
                    "size": output(&["size"], "number"),
                    "tags": output(&["tags"], "list"),
                },
            },
            "object": {
                "type": "test_object",
                "configuration": {
                    "bucket": output(&["id"], "string"),
                    "key": {
                        "cuenvFunction": "format",
                        "parts": ["objects/", output(&["id"], "string"), "/", output(&["size"], "number"), "-", true],
                        "type": "string",
                    },
                    "digest": {
                        "cuenvFunction": "sha256",
                        "of": output(&["id"], "string"),
                        "encoding": "hex",
                        "type": "string",
                    },
                    "tags": [{
                        "cuenvFunction": "select",
                        "from": output(&["tags"], "list"),
                        "path": [0, "name"],
                        "type": "string",
                    }],
                },
            },
        },
    })
}

fn top_level() -> Selection<'static> {
    Selection { environment: None }
}

fn string(text: &str) -> Value {
    Value::String(text.to_string())
}

#[test]
fn lowering_leaves_null_and_names_every_source() -> TestResult {
    let mut value = configuration();
    let lowered = lower(&mut value, top_level())?;
    assert_eq!(
        value.pointer("/resources/object/configuration"),
        Some(&json!({"bucket": null, "key": null, "digest": null, "tags": [null]}))
    );
    assert!(value.pointer("/resources/bucket/outputs").is_none());
    assert_eq!(
        lowered.output_types()[&bucket()],
        BTreeMap::from([
            ("id".to_string(), InfrastructureOutputType::String),
            ("size".to_string(), InfrastructureOutputType::Number),
        ])
    );
    let evaluator = lowered.into_evaluator().ok_or("missing evaluator")?;
    let tags = OutputPath::nested(
        "tags",
        vec![PathStep::Index(0), PathStep::Attribute("name".into())],
    )?;
    let references = evaluator
        .references()
        .iter()
        .map(|reference| {
            (
                reference.destination.clone(),
                reference.attribute.to_string(),
                reference.computed,
            )
        })
        .collect::<Vec<_>>();
    let field = |name: &str| vec![ConfigurationSelector::Field(name.to_string())];
    assert!(references.contains(&(field("bucket"), "id".to_string(), false)));
    assert!(references.contains(&(field("key"), "id".to_string(), true)));
    assert!(references.contains(&(field("key"), "size".to_string(), true)));
    assert!(references.contains(&(field("digest"), "id".to_string(), true)));
    assert_eq!(
        references
            .iter()
            .filter(|(destination, ..)| *destination == field("key"))
            .count(),
        2,
        "a function reads each source once"
    );
    assert!(references.contains(&(
        vec![
            ConfigurationSelector::Field("tags".into()),
            ConfigurationSelector::Index(0)
        ],
        tags.to_string(),
        false
    )));
    assert_eq!(evaluator.declared_outputs().len(), 2);
    Ok(())
}

#[test]
fn specialization_fills_known_values_and_leaves_deferred_ones_null() -> TestResult {
    let mut value = configuration();
    let evaluator = lower(&mut value, top_level())?
        .into_evaluator()
        .ok_or("missing evaluator")?;
    let sources = BTreeMap::from([
        (identity(OutputPath::attribute("id")), string("abc")),
        (
            identity(OutputPath::attribute("size")),
            Value::Number(Number::from(serde_json::Number::from(12))),
        ),
    ]);
    let deferred = evaluator
        .references()
        .iter()
        .filter(|reference| reference.attribute.name() == "tags")
        .cloned()
        .collect::<Vec<_>>();
    let consumer = consumer();
    let specialized = evaluator.specialize(&ConfigurationSpecialization {
        consumer: &consumer,
        sources: &sources,
        deferred: &deferred,
    })?;
    assert_eq!(
        Json::Object(specialized.configuration),
        json!({
            "bucket": "abc",
            "key": "objects/abc/12-true",
            "digest": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "tags": [null],
        })
    );
    assert_eq!(specialized.unresolved, deferred);
    Ok(())
}

#[test]
fn a_function_waits_for_every_source() -> TestResult {
    let mut value = configuration();
    let evaluator = lower(&mut value, top_level())?
        .into_evaluator()
        .ok_or("missing evaluator")?;
    let sources = BTreeMap::from([(identity(OutputPath::attribute("id")), string("abc"))]);
    let consumer = consumer();
    let specialized = evaluator.specialize(&ConfigurationSpecialization {
        consumer: &consumer,
        sources: &sources,
        deferred: &[],
    })?;
    assert_eq!(specialized.configuration["key"], Json::Null);
    assert_eq!(specialized.configuration["bucket"], json!("abc"));
    Ok(())
}

fn render(expression: &Json, id: &str) -> std::result::Result<Json, Box<dyn std::error::Error>> {
    let mut value = json!({
        "resources": {
            "bucket": {
                "type": "test_bucket",
                "outputs": {"id": output(&["id"], "string")},
            },
            "object": {"type": "test_object", "configuration": {"value": expression.clone()}},
        },
    });
    let evaluator = lower(&mut value, top_level())?
        .into_evaluator()
        .ok_or("missing evaluator")?;
    let sources = BTreeMap::from([(identity(OutputPath::attribute("id")), string(id))]);
    let consumer = consumer();
    let specialized = evaluator.specialize(&ConfigurationSpecialization {
        consumer: &consumer,
        sources: &sources,
        deferred: &[],
    })?;
    Ok(specialized.configuration["value"].clone())
}

#[test]
fn functions_match_their_cue_counterparts() -> TestResult {
    let id = || output(&["id"], "string");
    assert_eq!(
        render(
            &json!({"cuenvFunction": "sha256", "of": id(), "encoding": "base64", "type": "string"}),
            "abc"
        )?,
        json!("ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=")
    );
    assert_eq!(
        render(
            &json!({"cuenvFunction": "base64", "of": id(), "encoding": "standard", "type": "string"}),
            "??>"
        )?,
        json!("Pz8+")
    );
    assert_eq!(
        render(
            &json!({"cuenvFunction": "base64", "of": id(), "encoding": "url", "type": "string"}),
            "??>"
        )?,
        json!("Pz8-")
    );
    assert_eq!(
        render(
            &json!({"cuenvFunction": "join", "items": ["a", id(), "c"], "separator": ",", "type": "string"}),
            "b"
        )?,
        json!("a,b,c")
    );
    assert_eq!(
        render(
            &json!({
                "cuenvFunction": "sha256",
                "of": {"cuenvFunction": "format", "parts": ["x-", id()], "type": "string"},
                "encoding": "hex",
                "type": "string",
            }),
            "abc"
        )?,
        json!(hex::encode(Sha256::digest(b"x-abc")))
    );
    Ok(())
}

#[test]
fn a_value_computed_from_a_secret_is_redacted() -> TestResult {
    let secret = "expressions-test-secret-value";
    cuenv_events::register_secret(secret);
    let digest = render(
        &json!({
            "cuenvFunction": "sha256",
            "of": output(&["id"], "string"),
            "encoding": "hex",
            "type": "string",
        }),
        secret,
    )?;
    let digest = digest.as_str().ok_or("digest is not a string")?;
    assert!(is_registered_secret(digest));
    Ok(())
}

fn refused(value: &mut Json, selection: Selection<'_>) -> String {
    lower(value, selection)
        .err()
        .map_or_else(String::new, |error| error.to_string())
}

#[test]
fn misplaced_or_mistyped_references_are_refused() {
    let mut other_environment = configuration();
    assert!(
        refused(
            &mut other_environment,
            Selection {
                environment: Some("production")
            }
        )
        .contains("must be the schema's own descriptor")
    );

    let mut in_provider = configuration();
    in_provider["providers"] =
        json!({"test": {"configuration": {"token": output(&["id"], "string")}}});
    assert!(refused(&mut in_provider, top_level()).contains("may only set"));

    let mut encoded = configuration();
    encoded["resources"]["object"]["configuration"]["label"] =
        json!(output(&["id"], "string").to_string());
    assert!(refused(&mut encoded, top_level()).contains("encoded output reference"));

    let mut whole_list = configuration();
    whole_list["resources"]["object"]["configuration"]["all"] = output(&["tags"], "list");
    assert!(refused(&mut whole_list, top_level()).contains("#Select"));

    let mut undeclared = configuration();
    undeclared["resources"]["object"]["configuration"]["arn"] = output(&["arn"], "string");
    assert!(refused(&mut undeclared, top_level()).contains("not declared"));

    let mut retyped = configuration();
    retyped["resources"]["object"]["configuration"]["count"] = output(&["id"], "number");
    assert!(refused(&mut retyped, top_level()).contains("type other than"));
}

#[test]
fn an_environment_reads_its_own_outputs() -> TestResult {
    let mut value = json!({
        "resources": {
            "bucket": {
                "type": "test_bucket",
                "outputs": {"id": {
                    "cuenvOutput": {"namespace": "resources", "name": "bucket", "environment": "production", "path": ["id"]},
                    "type": "string",
                }},
            },
            "object": {"type": "test_object", "configuration": {"bucket": {
                "cuenvOutput": {"namespace": "resources", "name": "bucket", "environment": "production", "path": ["id"]},
                "type": "string",
            }}},
        },
    });
    let lowered = lower(
        &mut value,
        Selection {
            environment: Some("production"),
        },
    )?;
    assert_eq!(
        lowered
            .into_evaluator()
            .ok_or("missing evaluator")?
            .references()
            .len(),
        1
    );
    Ok(())
}

#[test]
fn a_configuration_without_outputs_needs_no_evaluator() -> TestResult {
    let mut value =
        json!({"resources": {"pet": {"type": "random_pet", "configuration": {"length": 2}}}});
    assert!(lower(&mut value, top_level())?.into_evaluator().is_none());
    Ok(())
}
