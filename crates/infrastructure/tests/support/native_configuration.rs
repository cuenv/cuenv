//! Frozen test evaluator: explicit fixture selectors become native provenance.
use cuenv_infrastructure::references::{
    ConfigurationEvaluator, ConfigurationSelector, ConfigurationSpecialization, NativeReference,
    OutputIdentity, SourceAddress, SpecializedConfiguration,
};
use cuenv_infrastructure::{InfrastructureError, ResourceAddress};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub fn native_resource(address: &str, attribute: &str) -> Value {
    json!({"__test_native_selector": ["resources", address, attribute]})
}

pub fn native_data_source(address: &str, attribute: &str) -> Value {
    json!({"__test_native_selector": ["data", address, attribute]})
}

pub struct NativeConfiguration {
    references: Vec<NativeReference>,
    configurations: BTreeMap<SourceAddress, Map<String, Value>>,
}

impl NativeConfiguration {
    pub fn has_references(&self) -> bool {
        !self.references.is_empty()
    }

    pub fn lower(infrastructure: &mut Value) -> cuenv_infrastructure::Result<Self> {
        let mut evaluator = Self {
            references: Vec::new(),
            configurations: BTreeMap::new(),
        };
        for namespace in ["resources", "data"] {
            let Some(declarations) = infrastructure
                .get_mut(namespace)
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            for (name, declaration) in declarations {
                let resource_type = declaration["type"].as_str().ok_or_else(|| {
                    InfrastructureError::configuration("test declaration has no type")
                })?;
                let address = ResourceAddress::new(resource_type, name);
                let consumer = if namespace == "data" {
                    SourceAddress::DataSource(address)
                } else {
                    SourceAddress::Resource(address)
                };
                let Some(configuration) = declaration.get_mut("configuration") else {
                    continue;
                };
                evaluator.visit(configuration, &consumer, &mut Vec::new())?;
                evaluator.configurations.insert(
                    consumer,
                    configuration
                        .as_object()
                        .ok_or_else(|| {
                            InfrastructureError::configuration(
                                "test configuration is not an object",
                            )
                        })?
                        .clone(),
                );
            }
        }
        Ok(evaluator)
    }

    fn visit(
        &mut self,
        value: &mut Value,
        consumer: &SourceAddress,
        path: &mut Vec<ConfigurationSelector>,
    ) -> cuenv_infrastructure::Result<()> {
        if let Some(marker) = value.get("__test_native_selector") {
            let source = ResourceAddress::parse(
                marker[1]
                    .as_str()
                    .ok_or_else(|| InfrastructureError::configuration("invalid test source"))?,
            )
            .ok_or_else(|| InfrastructureError::configuration("invalid test source address"))?;
            self.references.push(NativeReference {
                consumer: consumer.clone(),
                destination: path.clone(),
                source: if marker[0] == "data" {
                    SourceAddress::DataSource(source)
                } else {
                    SourceAddress::Resource(source)
                },
                attribute: marker[2]
                    .as_str()
                    .ok_or_else(|| InfrastructureError::configuration("invalid test output"))?
                    .into(),
            });
            *value = Value::Null;
        } else {
            match value {
                Value::Object(fields) => {
                    for (name, child) in fields {
                        path.push(ConfigurationSelector::Field(name.clone()));
                        self.visit(child, consumer, path)?;
                        path.pop();
                    }
                }
                Value::Array(elements) => {
                    for (index, child) in elements.iter_mut().enumerate() {
                        path.push(ConfigurationSelector::Index(i64::try_from(index).map_err(
                            |_| InfrastructureError::configuration("test index overflow"),
                        )?));
                        self.visit(child, consumer, path)?;
                        path.pop();
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl ConfigurationEvaluator for NativeConfiguration {
    fn references(&self) -> &[NativeReference] {
        &self.references
    }
    fn specialize(
        &self,
        input: &ConfigurationSpecialization<'_>,
    ) -> cuenv_infrastructure::Result<SpecializedConfiguration> {
        let mut configuration = Value::Object(
            self.configurations
                .get(input.consumer)
                .ok_or_else(|| InfrastructureError::configuration("unknown test consumer"))?
                .clone(),
        );
        for reference in self
            .references
            .iter()
            .filter(|reference| &reference.consumer == input.consumer)
        {
            let mut destination = &mut configuration;
            for selector in &reference.destination {
                destination = match selector {
                    ConfigurationSelector::Field(name) => destination.get_mut(name),
                    ConfigurationSelector::Index(index) => usize::try_from(*index)
                        .ok()
                        .and_then(|index| destination.get_mut(index)),
                }
                .ok_or_else(|| InfrastructureError::configuration("missing test destination"))?;
            }
            let identity = OutputIdentity {
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
            };
            *destination = input.sources.get(&identity).map_or(
                Value::Null,
                cuenv_infrastructure::type_system::Value::to_display_json,
            );
        }
        Ok(SpecializedConfiguration {
            configuration: configuration
                .as_object()
                .ok_or_else(|| InfrastructureError::configuration("invalid test projection"))?
                .clone(),
            unresolved: input.deferred.to_vec(),
        })
    }
}

#[test]
fn native_fixture_preserves_ordinary_reference_shaped_objects() -> cuenv_infrastructure::Result<()>
{
    let ordinary = json!({"fromResource": "fake_plain.producer", "fromDataSource": "fake_lookup.source", "attribute": "id"});
    let mut infrastructure = json!({"resources": {"consumer": {"type": "fake_plain", "configuration": {
        "ordinary": ordinary,
        "resource": native_resource("fake_plain.producer", "id"),
        "lookup": native_data_source("fake_lookup.source", "value"),
    }}}});
    let evaluator = NativeConfiguration::lower(&mut infrastructure)?;
    assert_eq!(evaluator.references().len(), 2);
    assert_eq!(
        infrastructure["resources"]["consumer"]["configuration"]["ordinary"],
        ordinary
    );
    assert!(infrastructure["resources"]["consumer"]["configuration"]["resource"].is_null());
    assert!(infrastructure["resources"]["consumer"]["configuration"]["lookup"].is_null());
    Ok(())
}
