//! Descriptor discovery follows provider schema shapes rather than object keys.

use serde_json::{Map, Value as Json};

use super::{
    ConfigurationReferences, DestinationPath, NativeReference, ReferenceOrigin,
    ResourceOutputReference, is_descriptor,
};
use crate::error::Result;
use crate::schema::Block;
use crate::type_system::{PathStep, Type};

impl ConfigurationReferences {
    /// Discover descriptors only at scalar schema positions, then attach native
    /// references from the original CUE instance. Ordinary provider objects may
    /// legitimately contain `fromResource`, `fromDataSource`, and `attribute`.
    ///
    /// # Errors
    /// Returns a configuration error for malformed descriptors, unsupported
    /// paths, unsettable schema fields, or two references at the same position.
    pub fn extract_with_schema(
        configuration: &Map<String, Json>,
        block: &Block,
        native: &[NativeReference],
    ) -> Result<Self> {
        let mut parsed = Self::descriptors_for_type(configuration, &block.implied_type())?;
        for reference in &parsed.references {
            block.reference_destination(&reference.destination)?;
        }
        for reference in native {
            let destination = DestinationPath::from_selectors(block, &reference.destination)?;
            if let Some(existing) = parsed
                .references
                .iter_mut()
                .find(|existing| existing.destination == destination)
            {
                if existing.source != reference.source
                    || existing.attribute != reference.attribute
                    || !matches!(existing.origin, ReferenceOrigin::Descriptor)
                {
                    return Err(destination
                        .problem("native reference provenance does not match its descriptor"));
                }
                existing.origin = ReferenceOrigin::Native(reference.clone());
                continue;
            }
            parsed.references.push(ResourceOutputReference {
                destination_attribute: destination.root_attribute().to_string(),
                destination,
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
                origin: ReferenceOrigin::Native(reference.clone()),
            });
        }
        parsed
            .references
            .sort_by(|left, right| left.destination.cmp(&right.destination));
        Ok(parsed)
    }

    pub(super) fn descriptors_for_type(
        configuration: &Map<String, Json>,
        value_type: &Type,
    ) -> Result<Self> {
        let mut references = Vec::new();
        visit(
            Visit {
                value: &Json::Object(configuration.clone()),
                value_type,
                steps: Vec::new(),
            },
            &mut references,
        )?;
        references.sort_by(|left, right| left.destination.cmp(&right.destination));
        Ok(Self { references })
    }
}

struct Visit<'value> {
    value: &'value Json,
    value_type: &'value Type,
    steps: Vec<PathStep>,
}

fn visit(input: Visit<'_>, references: &mut Vec<ResourceOutputReference>) -> Result<()> {
    let Visit {
        value,
        value_type,
        steps,
    } = input;
    if matches!(value_type, Type::Boolean | Type::Number | Type::String) && is_descriptor(value) {
        let path = DestinationPath::try_from(steps)?;
        let top = Map::from_iter([(path.root_attribute().to_string(), value.clone())]);
        let parsed = ConfigurationReferences::extract(&top)
            .map_err(|_| path.problem("malformed output reference descriptor"))?;
        for mut reference in parsed.references {
            reference.destination = path.clone();
            references.push(reference);
        }
        return Ok(());
    }
    let mut descend = |step: PathStep, value: &Json, value_type: &Type| {
        let mut child = steps.clone();
        child.push(step);
        visit(
            Visit {
                value,
                value_type,
                steps: child,
            },
            references,
        )
    };
    match (value_type, value) {
        (Type::Object(attributes), Json::Object(values)) => {
            for (name, value_type) in attributes {
                if let Some(value) = values.get(name) {
                    descend(PathStep::Attribute(name.clone()), value, value_type)?;
                }
            }
        }
        (Type::Map(element), Json::Object(values)) => {
            for (name, value) in values {
                descend(PathStep::Key(name.clone()), value, element)?;
            }
        }
        (Type::List(element), Json::Array(values)) => {
            for (index, value) in values.iter().enumerate() {
                if let Ok(index) = i64::try_from(index) {
                    descend(PathStep::Index(index), value, element)?;
                }
            }
        }
        (Type::Tuple(elements), Json::Array(values)) => {
            for (index, (value_type, value)) in elements.iter().zip(values).enumerate() {
                if let Ok(index) = i64::try_from(index) {
                    descend(PathStep::Index(index), value, value_type)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::{ConfigurationSelector, SourceAddress};
    use crate::schema::{Attribute, Documentation, Presence};
    use crate::state::ResourceAddress;
    use crate::type_system::Value;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn block(value_type: Type) -> Block {
        Block {
            attributes: BTreeMap::from([(
                "argument".into(),
                Attribute {
                    value_type,
                    nested: None,
                    presence: Presence::Optional,
                    sensitive: false,
                    write_only: false,
                    documentation: Documentation::default(),
                },
            )]),
            ..Block::default()
        }
    }

    #[test]
    fn ordinary_schema_objects_can_have_descriptor_field_names() -> Result<()> {
        let block = block(Type::Object(BTreeMap::from([
            ("fromResource".into(), Type::String),
            ("attribute".into(), Type::String),
        ])));
        let configuration = Map::from_iter([(
            "argument".into(),
            json!({"fromResource": "literal", "attribute": "ordinary"}),
        )]);
        let references = ConfigurationReferences::extract_with_schema(&configuration, &block, &[])?;
        assert!(references.references().is_empty());
        assert_eq!(
            references
                .prepare(&configuration, &block.implied_type())?
                .to_display_json(),
            Json::Object(configuration)
        );
        Ok(())
    }

    #[test]
    fn nested_descriptors_are_prepared_and_filled_at_exact_schema_positions() -> Result<()> {
        let block = block(Type::Map(Box::new(Type::List(Box::new(Type::String)))));
        let configuration = Map::from_iter([(
            "argument".into(),
            json!({"a.b": ["neighbor", {"fromResource": "fake_obj.source", "attribute": "id"}]}),
        )]);
        let references = ConfigurationReferences::extract_with_schema(&configuration, &block, &[])?;
        assert_eq!(references.references().len(), 1);
        assert_eq!(
            references.references()[0].destination.steps(),
            &[
                PathStep::Attribute("argument".into()),
                PathStep::Key("a.b".into()),
                PathStep::Index(1),
            ]
        );
        let prepared = references.prepare(&configuration, &block.implied_type())?;
        assert!(references.has_unknown_destination(&prepared));
        let filled = references.resolve(&prepared, |_| Ok(Value::String("resolved".into())))?;
        assert_eq!(
            filled.to_display_json(),
            json!({"argument": {"a.b": ["neighbor", "resolved"]}})
        );
        Ok(())
    }

    #[test]
    fn native_synthetic_descriptors_keep_their_distinct_provenance() -> Result<()> {
        let block = block(Type::String);
        let native = NativeReference {
            consumer: SourceAddress::Resource(ResourceAddress::new("fake_obj", "consumer")),
            destination: vec![ConfigurationSelector::Field("argument".into())],
            source: SourceAddress::Resource(ResourceAddress::new("fake_obj", "source")),
            attribute: "id".into(),
        };
        for value in [
            Json::Null,
            json!("narrowed literal"),
            json!({"fromResource": "fake_obj.source", "attribute": "id"}),
        ] {
            let configuration = Map::from_iter([("argument".into(), value)]);
            let parsed = ConfigurationReferences::extract_with_schema(
                &configuration,
                &block,
                std::slice::from_ref(&native),
            )?;
            assert_eq!(parsed.references().len(), 1);
            assert!(
                matches!(&parsed.references()[0].origin, ReferenceOrigin::Native(reference) if reference == &native)
            );
            assert!(
                parsed.has_unknown_destination(
                    &parsed.prepare(&configuration, &block.implied_type())?
                )
            );
        }
        let mismatched = Map::from_iter([(
            "argument".into(),
            json!({"fromResource": "fake_obj.other", "attribute": "id"}),
        )]);
        assert!(
            ConfigurationReferences::extract_with_schema(&mismatched, &block, &[native]).is_err()
        );
        Ok(())
    }
}
