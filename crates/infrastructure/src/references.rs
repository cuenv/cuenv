//! Concrete configuration descriptors for references to resource outputs.
//!
//! Only a whole, top-level scalar argument can be a reference. Provider schema
//! compatibility, dependency ordering and sensitivity belong to the engine;
//! this module parses descriptors and replaces their configuration positions.

use cuenv_manifest::manifest::InfrastructureOutputReference;
use serde_json::{Map, Value as Json};

use crate::error::{InfrastructureError, Result};
use crate::state::ResourceAddress;
use crate::type_system::{Type, Value};

/// One resource output used by a top-level configuration argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceOutputReference {
    /// Argument of the consuming resource that receives the output.
    pub destination_attribute: String,
    /// Full address of the resource that supplies the output.
    pub source: ResourceAddress,
    /// Top-level attribute to read from that resource's output.
    pub attribute: String,
}

/// Parsed references, without configuration values or resolved outputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationReferences {
    references: Vec<ResourceOutputReference>,
}

impl ConfigurationReferences {
    /// Extract references from top-level configuration arguments.
    ///
    /// An object containing `fromResource` is reserved as a descriptor and
    /// must have exactly `fromResource` and `attribute`, both strings. Other
    /// objects are ordinary provider configuration, including objects with an
    /// `attribute` field alone.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a malformed descriptor or address.
    /// Errors identify the argument, never its value.
    pub fn extract(configuration: &Map<String, Json>) -> Result<Self> {
        let mut references = Vec::new();
        for (destination_attribute, value) in configuration {
            let Some(descriptor) = value
                .as_object()
                .filter(|object| object.contains_key("fromResource"))
            else {
                continue;
            };
            if descriptor.len() != 2 || !descriptor.contains_key("attribute") {
                return Err(problem(
                    destination_attribute,
                    "a reference must contain exactly `fromResource` and `attribute`",
                ));
            }
            let descriptor: InfrastructureOutputReference = serde_json::from_value(value.clone())
                .map_err(|_| {
                problem(
                    destination_attribute,
                    "a reference must contain exactly `fromResource` and `attribute`, both strings",
                )
            })?;
            let source = parse_address(&descriptor.from_resource).ok_or_else(|| {
                problem(
                    destination_attribute,
                    "`fromResource` must be a valid resource address `type.name`",
                )
            })?;
            let attribute = descriptor.attribute;
            if !is_identifier(&attribute) {
                return Err(problem(
                    destination_attribute,
                    "`attribute` must be a CUE identifier",
                ));
            }
            references.push(ResourceOutputReference {
                destination_attribute: destination_attribute.clone(),
                source,
                attribute,
            });
        }
        Ok(Self { references })
    }

    /// Descriptors in configuration key order.
    #[must_use]
    pub fn references(&self) -> &[ResourceOutputReference] {
        &self.references
    }

    /// Decode ordinary arguments and put an unknown value at each reference.
    ///
    /// The supplied cty object type gives each unknown its scalar type during
    /// encoding. Descriptor objects never reach the provider value codec.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if descriptors changed after extraction,
    /// a destination is not a declared scalar attribute, or ordinary arguments
    /// do not conform to the supplied configuration type.
    pub fn prepare(&self, configuration: &Map<String, Json>, value_type: &Type) -> Result<Value> {
        if Self::extract(configuration)? != *self {
            return Err(InfrastructureError::configuration(
                "resource output references changed after extraction",
            ));
        }
        let mut ordinary = configuration.clone();
        let Type::Object(attributes) = value_type else {
            return Err(InfrastructureError::configuration(
                "resource configuration must have an object type",
            ));
        };
        for reference in &self.references {
            if !matches!(
                attributes.get(&reference.destination_attribute),
                Some(Type::Boolean | Type::Number | Type::String)
            ) {
                return Err(problem(
                    &reference.destination_attribute,
                    "references require a declared scalar destination attribute",
                ));
            }
            ordinary.insert(reference.destination_attribute.clone(), Json::Null);
        }
        let mut prepared = Value::from_configuration_json(&Json::Object(ordinary), value_type)?;
        let Value::Object(arguments) = &mut prepared else {
            return Err(InfrastructureError::configuration(
                "resource configuration must decode as an object",
            ));
        };
        for reference in &self.references {
            arguments.insert(reference.destination_attribute.clone(), Value::Unknown);
        }
        Ok(prepared)
    }

    /// Replace reference positions with concrete scalar outputs.
    ///
    /// The resolver chooses the source state and validates provider schema
    /// compatibility. Successful resolution preserves ordinary arguments and
    /// never changes `prepared`; errors never include the resolved value.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a missing destination, a null,
    /// unknown or nonscalar output, or propagates an error from the resolver.
    pub fn resolve(
        &self,
        prepared: &Value,
        mut resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        let mut configuration = prepared.clone();
        let Value::Object(arguments) = &mut configuration else {
            return Err(InfrastructureError::configuration(
                "resource configuration must be an object before resolving references",
            ));
        };
        for reference in &self.references {
            let destination = arguments
                .get_mut(&reference.destination_attribute)
                .ok_or_else(|| {
                    problem(
                        &reference.destination_attribute,
                        "reference destination is missing from configuration",
                    )
                })?;
            let value = resolver(reference)?;
            if !matches!(
                value.untyped(),
                Value::Boolean(_) | Value::Number(_) | Value::String(_)
            ) {
                return Err(problem(
                    &reference.destination_attribute,
                    "resource output must resolve to a concrete, non-null scalar value",
                ));
            }
            *destination = value;
        }
        Ok(configuration)
    }

    /// Fill references whose outputs are already known during planning.
    /// Unknown outputs stay unknown until their producer has been applied.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a missing destination, a known null
    /// or nonscalar output, or propagates an error from the resolver.
    pub fn resolve_planned(
        &self,
        prepared: &Value,
        mut resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        let mut configuration = prepared.clone();
        let Value::Object(arguments) = &mut configuration else {
            return Err(InfrastructureError::configuration(
                "resource configuration must be an object before resolving references",
            ));
        };
        for reference in &self.references {
            let destination = arguments
                .get_mut(&reference.destination_attribute)
                .ok_or_else(|| {
                    problem(
                        &reference.destination_attribute,
                        "reference destination is missing from configuration",
                    )
                })?;
            let value = resolver(reference)?;
            if matches!(value.untyped(), Value::Unknown) {
                continue;
            }
            if !matches!(
                value.untyped(),
                Value::Boolean(_) | Value::Number(_) | Value::String(_)
            ) {
                return Err(problem(
                    &reference.destination_attribute,
                    "known resource output must be a non-null scalar value",
                ));
            }
            *destination = value;
        }
        Ok(configuration)
    }
}

fn parse_address(address: &str) -> Option<ResourceAddress> {
    let (resource_type, name) = address.split_once('.')?;
    let (prefix, _) = resource_type.split_once('_')?;
    if prefix.is_empty()
        || !resource_type.starts_with(|character: char| character.is_ascii_lowercase())
        || resource_type.split('_').any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
        || !name.starts_with(|character: char| character.is_ascii_alphabetic())
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return None;
    }
    Some(ResourceAddress::new(resource_type, name))
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn problem(destination_attribute: &str, reason: &str) -> InfrastructureError {
    InfrastructureError::configuration(format!("configuration.{destination_attribute}: {reason}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn configuration() -> Map<String, Json> {
        Map::from_iter([
            (
                "zone_id".to_string(),
                json!({"fromResource": "cloudflare_zone.primary", "attribute": "id"}),
            ),
            ("name".to_string(), json!("www")),
            (
                "metadata".to_string(),
                json!({"attribute": "ordinary", "nested": [1, true]}),
            ),
        ])
    }

    fn configuration_type() -> Type {
        Type::Object(BTreeMap::from([
            ("zone_id".to_string(), Type::String),
            ("name".to_string(), Type::String),
            ("metadata".to_string(), Type::Dynamic),
        ]))
    }

    #[test]
    fn extracts_descriptors_without_treating_ordinary_objects_as_references() -> Result<()> {
        let references = ConfigurationReferences::extract(&configuration())?;
        assert_eq!(
            references.references(),
            &[ResourceOutputReference {
                destination_attribute: "zone_id".to_string(),
                source: ResourceAddress::new("cloudflare_zone", "primary"),
                attribute: "id".to_string(),
            }]
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_descriptors_without_echoing_values() -> Result<()> {
        for descriptor in [
            json!({"fromResource": "secret"}),
            json!({"fromResource": "secret", "attribute": "id", "extra": "secret"}),
            json!({"fromResource": 123, "attribute": "id"}),
            json!({"fromResource": "fake_obj.source", "attribute": "nested.id"}),
            json!({"fromResource": "fake_obj.source", "attribute": ""}),
            json!({"fromResource": "cloudflare_zone.primary", "attribute": 123}),
            json!({"fromResource": "cloudflare_zone.primary", "attribute": ""}),
            json!({"fromResource": "cloudflare_zone.primary", "attribute": "secret\n"}),
        ] {
            let error = expected_error(ConfigurationReferences::extract(&Map::from_iter([(
                "zone_id".to_string(),
                descriptor,
            )])))?
            .to_string();
            assert!(error.contains("configuration.zone_id"));
            assert!(!error.contains("secret"));
        }
        Ok(())
    }

    #[test]
    fn rejects_invalid_source_addresses() {
        for address in [
            "zone.primary",
            "_zone.primary",
            "Cloudflare_zone.primary",
            "cloudflare__zone.primary",
            "cloudflare_zone.",
            "cloudflare_zone.1primary",
            "cloudflare_zone.primary.extra",
            "cloudflare_zone.primary/other",
        ] {
            let configuration = Map::from_iter([(
                "zone_id".to_string(),
                json!({"fromResource": address, "attribute": "id"}),
            )]);
            assert!(ConfigurationReferences::extract(&configuration).is_err());
        }
    }

    #[test]
    fn prepares_typed_unknowns_and_preserves_ordinary_arguments() -> Result<()> {
        let configuration = configuration();
        let original = configuration.clone();
        let references = ConfigurationReferences::extract(&configuration)?;
        let prepared = references.prepare(&configuration, &configuration_type())?;
        assert_eq!(prepared.attribute("zone_id"), Some(&Value::Unknown));
        assert_eq!(
            prepared.attribute("name"),
            Some(&Value::String("www".to_string()))
        );
        let metadata = Value::from_configuration_json(&configuration["metadata"], &Type::Dynamic)?;
        assert_eq!(prepared.attribute("metadata"), Some(&metadata));
        assert_eq!(configuration, original);
        let bytes = crate::type_system::to_message_pack(&prepared, &configuration_type())?;
        assert!(crate::type_system::raw_equal(
            &crate::type_system::from_message_pack(&bytes, &configuration_type())?,
            &prepared,
            &configuration_type()
        ));
        Ok(())
    }

    #[test]
    fn resolves_concrete_values_without_mutating_prepared_configuration() -> Result<()> {
        let configuration = configuration();
        let references = ConfigurationReferences::extract(&configuration)?;
        let prepared = references.prepare(&configuration, &configuration_type())?;
        let resolved = references.resolve(&prepared, |reference| {
            assert_eq!(reference.source.to_string(), "cloudflare_zone.primary");
            Ok(Value::String("zone-value".to_string()))
        })?;
        assert_eq!(
            resolved.attribute("zone_id"),
            Some(&Value::String("zone-value".to_string()))
        );
        assert_eq!(
            resolved.attribute("metadata"),
            prepared.attribute("metadata")
        );
        assert_eq!(prepared.attribute("zone_id"), Some(&Value::Unknown));
        Ok(())
    }

    #[test]
    fn planned_resolution_fills_known_values_and_preserves_unknowns() -> Result<()> {
        let configuration = Map::from_iter([
            (
                "zone_id".to_string(),
                json!({"fromResource": "cloudflare_zone.primary", "attribute": "id"}),
            ),
            (
                "account_id".to_string(),
                json!({"fromResource": "cloudflare_account.main", "attribute": "id"}),
            ),
        ]);
        let references = ConfigurationReferences::extract(&configuration)?;
        let value_type = Type::Object(BTreeMap::from([
            ("zone_id".to_string(), Type::String),
            ("account_id".to_string(), Type::String),
        ]));
        let prepared = references.prepare(&configuration, &value_type)?;
        let planned = references.resolve_planned(&prepared, |reference| {
            if reference.source.name == "primary" {
                Ok(Value::String("zone-123".to_string()))
            } else {
                Ok(Value::Unknown)
            }
        })?;
        assert_eq!(
            planned.attribute("zone_id"),
            Some(&Value::String("zone-123".to_string()))
        );
        assert_eq!(planned.attribute("account_id"), Some(&Value::Unknown));
        Ok(())
    }

    #[test]
    fn resolves_boolean_and_number_outputs() -> Result<()> {
        for (value_type, value) in [
            (Type::Boolean, Value::Boolean(true)),
            (Type::Number, Value::Number(42.into())),
        ] {
            let configuration = Map::from_iter([(
                "argument".to_string(),
                json!({"fromResource": "example_resource.primary", "attribute": "output"}),
            )]);
            let references = ConfigurationReferences::extract(&configuration)?;
            let value_type = Type::Object(BTreeMap::from([("argument".to_string(), value_type)]));
            let prepared = references.prepare(&configuration, &value_type)?;
            let resolved = references.resolve(&prepared, |_| Ok(value.clone()))?;
            assert_eq!(resolved.attribute("argument"), Some(&value));
        }
        Ok(())
    }

    #[test]
    fn rejects_nonconcrete_or_nonscalar_outputs_without_echoing_values() -> Result<()> {
        let configuration = configuration();
        let references = ConfigurationReferences::extract(&configuration)?;
        let prepared = references.prepare(&configuration, &configuration_type())?;
        for value in [
            Value::Null,
            Value::Unknown,
            Value::List(vec![Value::String("secret".to_string())]),
            Value::Object(BTreeMap::from([("secret".to_string(), Value::Unknown)])),
        ] {
            let error =
                expected_error(references.resolve(&prepared, |_| Ok(value.clone())))?.to_string();
            assert!(error.contains("configuration.zone_id"));
            assert!(!error.contains("secret"));
        }
        Ok(())
    }

    #[test]
    fn rejects_changed_descriptors_and_nonscalar_destinations() -> Result<()> {
        let mut configuration = configuration();
        let references = ConfigurationReferences::extract(&configuration)?;
        let mut value_type = configuration_type();
        if let Type::Object(attributes) = &mut value_type {
            attributes.insert("zone_id".to_string(), Type::List(Box::new(Type::String)));
        }
        assert!(references.prepare(&configuration, &value_type).is_err());
        configuration.remove("zone_id");
        assert!(
            references
                .prepare(&configuration, &configuration_type())
                .is_err()
        );
        Ok(())
    }

    fn expected_error<T>(result: Result<T>) -> Result<InfrastructureError> {
        result
            .err()
            .ok_or_else(|| InfrastructureError::configuration("expected a configuration error"))
    }
}
