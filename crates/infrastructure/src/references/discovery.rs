//! Classify native CUE references using the provider's schema.

use serde_json::{Map, Value as Json};

use super::{ConfigurationReferences, DestinationPath, NativeReference, ResourceOutputReference};
use crate::error::Result;
use crate::schema::Block;

impl ConfigurationReferences {
    /// Classify native provenance and validate its projected destinations.
    /// Provider configuration is ordinary data, including objects whose keys
    /// happen to resemble a former reference descriptor.
    ///
    /// # Errors
    /// Returns a configuration error for unsupported paths, unsettable schema
    /// fields, inconsistent placeholders, or duplicate destinations.
    pub fn extract_with_schema(
        configuration: &Map<String, Json>,
        block: &Block,
        native: &[NativeReference],
    ) -> Result<Self> {
        let projected = Json::Object(configuration.clone());
        let mut references = Vec::with_capacity(native.len());
        for reference in native {
            let destination = DestinationPath::from_selectors(block, &reference.destination)?;
            if destination.json_value(&projected) != Some(&Json::Null) {
                return Err(
                    destination.problem("native reference placeholder is missing or changed")
                );
            }
            // Only a computed value reads more than one source, each of them once.
            if references.iter().any(|existing: &ResourceOutputReference| {
                existing.destination == destination
                    && (!reference.computed
                        || !existing.native.computed
                        || (existing.source == reference.source
                            && existing.attribute == reference.attribute))
            }) {
                return Err(
                    destination.problem("multiple native references have the same destination")
                );
            }
            references.push(ResourceOutputReference {
                destination_attribute: destination.root_attribute().to_string(),
                destination,
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
                native: reference.clone(),
            });
        }
        references.sort_by(|left, right| left.destination.cmp(&right.destination));
        Ok(Self { references })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::{ConfigurationSelector, SourceAddress};
    use crate::schema::{Attribute, Documentation, Presence};
    use crate::state::ResourceAddress;
    use crate::type_system::{PathStep, Type, Value};
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

    fn native(destination: Vec<ConfigurationSelector>) -> NativeReference {
        NativeReference {
            consumer: SourceAddress::Resource(ResourceAddress::new("fake_obj", "consumer")),
            destination,
            source: SourceAddress::DataSource(ResourceAddress::new("fake_lookup", "source")),
            attribute: "id".into(),
            computed: false,
        }
    }

    #[test]
    fn placeholder_and_duplicate_errors_hide_unregistered_map_keys() {
        let key = format!("unregistered-map-key-{}", uuid::Uuid::new_v4());
        let block = block(Type::Map(Box::new(Type::String)));
        let reference = native(vec![
            ConfigurationSelector::Field("argument".into()),
            ConfigurationSelector::Field(key.clone()),
        ]);
        let missing = Map::from_iter([("argument".into(), json!({}))]);
        let changed = Map::from_iter([("argument".into(), json!({&key: "changed"}))]);
        let present = Map::from_iter([("argument".into(), json!({&key: null}))]);
        for result in [
            ConfigurationReferences::extract_with_schema(
                &missing,
                &block,
                std::slice::from_ref(&reference),
            ),
            ConfigurationReferences::extract_with_schema(
                &changed,
                &block,
                std::slice::from_ref(&reference),
            ),
            ConfigurationReferences::extract_with_schema(
                &present,
                &block,
                &[reference.clone(), reference],
            ),
        ] {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("configuration.argument[<hidden key>]"),
                "{error}"
            );
            assert!(!error.contains(&key), "{error}");
        }
    }

    #[test]
    fn ordinary_objects_do_not_infer_references() -> Result<()> {
        let block = block(Type::Object(BTreeMap::from([
            ("fromResource".into(), Type::String),
            ("fromDataSource".into(), Type::String),
            ("attribute".into(), Type::String),
        ])));
        let configuration = Map::from_iter([(
            "argument".into(),
            json!({"fromResource": "literal", "fromDataSource": "ordinary", "attribute": "value"}),
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
    fn former_descriptor_at_a_scalar_destination_is_ordinary_invalid_data() -> Result<()> {
        let block = block(Type::String);
        let configuration = Map::from_iter([(
            "argument".into(),
            json!({"fromResource": "fake_obj.source", "attribute": "id"}),
        )]);
        let references = ConfigurationReferences::extract_with_schema(&configuration, &block, &[])?;
        assert!(references.references().is_empty());
        assert!(
            references
                .prepare(&configuration, &block.implied_type())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn native_references_are_prepared_and_filled_at_exact_schema_positions() -> Result<()> {
        let block = block(Type::Map(Box::new(Type::List(Box::new(Type::String)))));
        let configuration =
            Map::from_iter([("argument".into(), json!({"a.b": ["neighbor", null]}))]);
        let reference = native(vec![
            ConfigurationSelector::Field("argument".into()),
            ConfigurationSelector::Field("a.b".into()),
            ConfigurationSelector::Index(1),
        ]);
        let references =
            ConfigurationReferences::extract_with_schema(&configuration, &block, &[reference])?;
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
    fn only_a_computed_value_reads_several_sources_and_each_of_them_once() {
        let block = block(Type::String);
        let projected = Map::from_iter([("argument".into(), Json::Null)]);
        let reference = |attribute: &str, computed| NativeReference {
            attribute: attribute.into(),
            computed,
            ..native(vec![ConfigurationSelector::Field("argument".into())])
        };
        let extract = |references: &[NativeReference]| {
            ConfigurationReferences::extract_with_schema(&projected, &block, references).is_ok()
        };
        assert!(extract(&[reference("a", true), reference("b", true)]));
        assert!(!extract(&[reference("a", true), reference("a", true)]));
        assert!(!extract(&[reference("a", false), reference("b", false)]));
        assert!(!extract(&[reference("a", true), reference("b", false)]));
    }

    #[test]
    fn native_provenance_requires_one_unchanged_placeholder() {
        let block = block(Type::String);
        let reference = native(vec![ConfigurationSelector::Field("argument".into())]);
        let projected = Map::from_iter([("argument".into(), Json::Null)]);
        assert!(
            ConfigurationReferences::extract_with_schema(
                &projected,
                &block,
                std::slice::from_ref(&reference)
            )
            .is_ok()
        );
        assert!(
            ConfigurationReferences::extract_with_schema(
                &projected,
                &block,
                &[reference.clone(), reference.clone()]
            )
            .is_err()
        );
        assert!(
            ConfigurationReferences::extract_with_schema(
                &Map::new(),
                &block,
                std::slice::from_ref(&reference)
            )
            .is_err()
        );
        let changed = Map::from_iter([("argument".into(), json!("other"))]);
        assert!(
            ConfigurationReferences::extract_with_schema(&changed, &block, &[reference]).is_err()
        );
    }
}
