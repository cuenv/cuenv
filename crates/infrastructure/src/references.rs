//! Native CUE references to resource and data-source outputs.
//!
//! Scalar destinations may be nested inside provider objects, maps and ordered
//! collections. Provider schema compatibility, dependency ordering and
//! sensitivity belong to the engine; this module classifies references and
//! replaces their exact configuration positions.

use serde_json::{Map, Value as Json};

use crate::error::{InfrastructureError, Result};
use crate::state::ResourceAddress;
use crate::type_system::{Type, Value};

mod destination;
mod discovery;
mod evaluation;
mod path;

pub use destination::ReferenceDestination;
pub use evaluation::{
    ConfigurationEvaluator, ConfigurationSelector, ConfigurationSpecialization,
    ConfigurationVerification, NativeReference, OutputIdentity, SpecializedConfiguration,
};
pub use path::DestinationPath;

/// Identity of an output producer. Data sources never have managed state addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceAddress {
    /// A managed resource, backed by a state record.
    Resource(ResourceAddress),
    /// An invocation-local data source.
    DataSource(ResourceAddress),
}

impl SourceAddress {
    /// Provider type and local declaration name, without a namespace prefix.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        match self {
            Self::Resource(address) | Self::DataSource(address) => address,
        }
    }
}

impl std::fmt::Display for SourceAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(address) => address.fmt(formatter),
            Self::DataSource(address) => write!(formatter, "data.{address}"),
        }
    }
}

/// One top-level resource or data output used by a configuration argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceOutputReference {
    /// Top-level argument containing the receiving scalar.
    pub destination_attribute: String,
    /// Exact schema-classified position of the consuming scalar.
    pub destination: DestinationPath,
    /// Native CUE provenance, retained independently of provider values.
    pub native: NativeReference,
    /// Full address of the resource that supplies the output.
    pub source: SourceAddress,
    /// Top-level attribute to read from that resource's output.
    pub attribute: String,
}

/// Parsed references, without configuration values or resolved outputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationReferences {
    references: Vec<ResourceOutputReference>,
}

impl ConfigurationReferences {
    /// References in configuration path order.
    #[must_use]
    pub fn references(&self) -> &[ResourceOutputReference] {
        &self.references
    }

    /// Decode ordinary arguments and put an unknown value at each reference.
    ///
    /// The supplied cty object type gives each unknown its scalar type during
    /// encoding. Reference identity is carried separately from JSON values.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if a destination is not a declared
    /// scalar attribute, or ordinary arguments
    /// do not conform to the supplied configuration type.
    pub fn prepare(&self, configuration: &Map<String, Json>, value_type: &Type) -> Result<Value> {
        let ordinary = configuration.clone();
        let Type::Object(_) = value_type else {
            return Err(InfrastructureError::configuration(
                "resource configuration must have an object type",
            ));
        };
        for reference in &self.references {
            if !matches!(
                value_type.at_path(reference.destination.steps()),
                Some(Type::Boolean | Type::Number | Type::String)
            ) {
                return Err(problem(
                    &reference.destination_attribute,
                    "references require a declared scalar destination attribute",
                ));
            }
        }
        let mut ordinary = Json::Object(ordinary);
        for reference in &self.references {
            reference
                .destination
                .replace_json(&mut ordinary, Json::Null)?;
        }
        let mut prepared = Value::from_configuration_json(&ordinary, value_type)?;
        for reference in &self.references {
            reference
                .destination
                .replace_value(&mut prepared, Value::Unknown)?;
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
        resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        self.fill(prepared, UnknownOutputs::Reject, resolver)
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
        resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        self.fill(prepared, UnknownOutputs::Keep, resolver)
    }

    /// Whether any reference destination of `configuration` is unknown.
    #[must_use]
    pub fn has_unknown_destination(&self, configuration: &Value) -> bool {
        self.references.iter().any(|reference| {
            reference
                .destination
                .value(configuration)
                .is_some_and(Value::is_unknown)
        })
    }

    fn fill(
        &self,
        prepared: &Value,
        unknown_outputs: UnknownOutputs,
        mut resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        let mut configuration = prepared.clone();
        if !matches!(configuration.untyped(), Value::Object(_)) {
            return Err(InfrastructureError::configuration(
                "resource configuration must be an object before resolving references",
            ));
        }
        for reference in &self.references {
            let value = resolver(reference)?;
            if unknown_outputs == UnknownOutputs::Keep && value.is_unknown() {
                continue;
            }
            if !matches!(
                value.untyped(),
                Value::Boolean(_) | Value::Number(_) | Value::String(_)
            ) {
                return Err(problem(
                    &reference.destination_attribute,
                    "resource output must resolve to a concrete, non-null scalar value",
                ));
            }
            reference
                .destination
                .replace_value(&mut configuration, value)?;
        }
        Ok(configuration)
    }
}

/// Whether filling references accepts outputs that are not yet known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnknownOutputs {
    /// Every output must be concrete.
    Reject,
    /// Unknown outputs leave the destination unknown.
    Keep,
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

    fn reference(destination: &str, source: SourceAddress) -> ResourceOutputReference {
        let native = NativeReference {
            consumer: SourceAddress::Resource(ResourceAddress::new("example_record", "consumer")),
            destination: vec![ConfigurationSelector::Field(destination.to_string())],
            source: source.clone(),
            attribute: "id".into(),
            computed: false,
        };
        ResourceOutputReference {
            destination_attribute: destination.into(),
            destination: DestinationPath::attribute(destination),
            native,
            source,
            attribute: "id".into(),
        }
    }

    fn configuration() -> Map<String, Json> {
        Map::from_iter([
            ("zone_id".into(), Json::Null),
            ("name".into(), json!("www")),
            (
                "metadata".into(),
                json!({"fromResource": "ordinary", "attribute": "value"}),
            ),
        ])
    }

    fn configuration_type() -> Type {
        Type::Object(BTreeMap::from([
            ("zone_id".into(), Type::String),
            ("name".into(), Type::String),
            ("metadata".into(), Type::Dynamic),
        ]))
    }

    fn references() -> ConfigurationReferences {
        ConfigurationReferences {
            references: vec![reference(
                "zone_id",
                SourceAddress::Resource(ResourceAddress::new("cloudflare_zone", "primary")),
            )],
        }
    }

    #[test]
    fn prepares_unknowns_and_resolves_without_mutating_other_values() -> Result<()> {
        let configuration = configuration();
        let original = configuration.clone();
        let references = references();
        let prepared = references.prepare(&configuration, &configuration_type())?;
        assert_eq!(prepared.attribute("zone_id"), Some(&Value::Unknown));
        let metadata = Value::from_configuration_json(&configuration["metadata"], &Type::Dynamic)?;
        assert_eq!(prepared.attribute("metadata"), Some(&metadata));
        assert_eq!(configuration, original);
        let resolved = references.resolve(&prepared, |_| Ok(Value::String("zone-value".into())))?;
        assert_eq!(
            resolved.attribute("zone_id"),
            Some(&Value::String("zone-value".into()))
        );
        assert_eq!(resolved.attribute("metadata"), Some(&metadata));
        assert_eq!(prepared.attribute("zone_id"), Some(&Value::Unknown));
        let bytes = crate::type_system::to_message_pack(&prepared, &configuration_type())?;
        assert!(crate::type_system::raw_equal(
            &crate::type_system::from_message_pack(&bytes, &configuration_type())?,
            &prepared,
            &configuration_type(),
        ));
        Ok(())
    }

    #[test]
    fn planned_resolution_preserves_unknowns_and_source_namespaces() -> Result<()> {
        let configuration = Map::from_iter([
            ("managed".into(), Json::Null),
            ("lookup".into(), Json::Null),
        ]);
        let references = ConfigurationReferences {
            references: vec![
                reference(
                    "managed",
                    SourceAddress::Resource(ResourceAddress::new("example_object", "primary")),
                ),
                reference(
                    "lookup",
                    SourceAddress::DataSource(ResourceAddress::new("example_object", "primary")),
                ),
            ],
        };
        let value_type = Type::Object(BTreeMap::from([
            ("managed".into(), Type::String),
            ("lookup".into(), Type::String),
        ]));
        let prepared = references.prepare(&configuration, &value_type)?;
        let planned =
            references.resolve_planned(&prepared, |reference| match &reference.source {
                SourceAddress::Resource(_) => Ok(Value::String(reference.source.to_string())),
                SourceAddress::DataSource(_) => Ok(Value::Unknown),
            })?;
        assert_eq!(
            planned.attribute("managed"),
            Some(&Value::String("example_object.primary".into()))
        );
        assert_eq!(planned.attribute("lookup"), Some(&Value::Unknown));
        assert!(references.has_unknown_destination(&planned));
        let resolved = references.resolve(&prepared, |reference| {
            Ok(Value::String(reference.source.to_string()))
        })?;
        assert_eq!(
            resolved.attribute("lookup"),
            Some(&Value::String("data.example_object.primary".into()))
        );
        Ok(())
    }

    #[test]
    fn resolves_primitive_outputs_and_rejects_invalid_values() -> Result<()> {
        for (value_type, value) in [
            (Type::Boolean, Value::Boolean(true)),
            (Type::Number, Value::Number(42.into())),
        ] {
            let configuration = Map::from_iter([("argument".into(), Json::Null)]);
            let references = ConfigurationReferences {
                references: vec![reference(
                    "argument",
                    SourceAddress::Resource(ResourceAddress::new("example_resource", "primary")),
                )],
            };
            let value_type = Type::Object(BTreeMap::from([("argument".into(), value_type)]));
            let prepared = references.prepare(&configuration, &value_type)?;
            assert_eq!(
                references
                    .resolve(&prepared, |_| Ok(value.clone()))?
                    .attribute("argument"),
                Some(&value)
            );
        }
        let references = references();
        let prepared = references.prepare(&configuration(), &configuration_type())?;
        for value in [
            Value::Null,
            Value::Unknown,
            Value::List(vec![Value::String("secret".into())]),
        ] {
            let error = references
                .resolve(&prepared, |_| Ok(value.clone()))
                .unwrap_err()
                .to_string();
            assert!(error.contains("configuration.zone_id"));
            assert!(!error.contains("secret"));
        }
        Ok(())
    }

    #[test]
    fn rejects_missing_or_non_scalar_destinations() {
        let references = references();
        let mut value_type = configuration_type();
        if let Type::Object(attributes) = &mut value_type {
            attributes.insert("zone_id".into(), Type::List(Box::new(Type::String)));
        }
        assert!(references.prepare(&configuration(), &value_type).is_err());
        let mut missing = configuration();
        missing.remove("zone_id");
        assert!(references.prepare(&missing, &configuration_type()).is_err());
    }
}
