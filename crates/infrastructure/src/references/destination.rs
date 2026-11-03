//! Schema traversal for scalar reference destinations.

use std::collections::BTreeMap;

use super::{ConfigurationSelector, DestinationPath};
use crate::error::Result;
use crate::schema::{Attribute, Block, NestedAttributes, NestedBlock, Nesting, Presence};
use crate::type_system::{PathStep, Type, Value};

/// Type and inherited sensitivity of a validated scalar destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceDestination {
    value_type: Type,
    sensitive: bool,
}

impl ReferenceDestination {
    pub(crate) fn classify(
        block: &Block,
        selectors: &[ConfigurationSelector],
    ) -> Result<DestinationPath> {
        let mut cursor = Cursor {
            shape: Shape::Block(block),
            sensitive: false,
        };
        let mut steps = Vec::with_capacity(selectors.len());
        for selector in selectors {
            let step = cursor.selector_step(selector);
            let path = DestinationPath::try_from({
                let mut prefix = steps.clone();
                prefix.push(step.clone());
                prefix
            })?;
            cursor = cursor
                .select(&step)
                .map_err(|reason| path.problem(reason))?;
            steps.push(step);
        }
        let path = DestinationPath::try_from(steps)?;
        Self::for_path(block, &path)?;
        Ok(path)
    }

    /// The exact scalar type required at the selected position.
    #[must_use]
    pub const fn value_type(&self) -> &Type {
        &self.value_type
    }

    /// Whether the leaf or an ancestor attribute is marked sensitive.
    #[must_use]
    pub const fn is_sensitive(&self) -> bool {
        self.sensitive
    }

    /// Whether a resolved value has this exact scalar type. Unknown, null and
    /// composite values are refused; planning handles unknown outputs separately.
    /// Concrete dynamic wrappers must agree with the destination type too.
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        match value {
            Value::Typed(typed) => {
                typed.value_type == self.value_type && self.accepts(&typed.value)
            }
            Value::Boolean(_) => self.value_type == Type::Boolean,
            Value::Number(_) => self.value_type == Type::Number,
            Value::String(_) => self.value_type == Type::String,
            Value::Null | Value::Unknown | Value::List(_) | Value::Object(_) => false,
        }
    }

    pub(crate) fn for_path(block: &Block, path: &DestinationPath) -> Result<Self> {
        let cursor = path.steps().iter().try_fold(
            Cursor {
                shape: Shape::Block(block),
                sensitive: false,
            },
            |cursor, step| cursor.select(step).map_err(|reason| path.problem(reason)),
        )?;
        let value_type = match cursor.shape {
            Shape::Type(value_type @ (Type::Boolean | Type::Number | Type::String)) => {
                value_type.clone()
            }
            Shape::Type(Type::Dynamic) => {
                return Err(path.problem("references cannot target unresolved dynamic types"));
            }
            _ => return Err(path.problem("references require a scalar destination")),
        };
        Ok(Self {
            value_type,
            sensitive: cursor.sensitive,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Shape<'schema> {
    Block(&'schema Block),
    Attributes(&'schema BTreeMap<String, Attribute>),
    NestedAttributes(&'schema NestedAttributes),
    NestedBlock(&'schema NestedBlock),
    Type(&'schema Type),
}

#[derive(Debug, Clone, Copy)]
struct Cursor<'schema> {
    shape: Shape<'schema>,
    sensitive: bool,
}

type TraversalResult<'schema> = std::result::Result<Cursor<'schema>, &'static str>;

impl<'schema> Cursor<'schema> {
    fn selector_step(self, selector: &ConfigurationSelector) -> PathStep {
        let name = match selector {
            ConfigurationSelector::Field(name) => name,
            ConfigurationSelector::Index(index) => return PathStep::Index(*index),
        };
        match self.shape {
            Shape::Type(Type::Map(_)) => PathStep::Key(name.clone()),
            Shape::NestedAttributes(nested) => match nested.nesting {
                Nesting::Map => PathStep::Key(name.clone()),
                _ => PathStep::Attribute(name.clone()),
            },
            Shape::NestedBlock(nested) => match nested.nesting {
                Nesting::Map => PathStep::Key(name.clone()),
                _ => PathStep::Attribute(name.clone()),
            },
            _ => PathStep::Attribute(name.clone()),
        }
    }

    fn select(self, step: &PathStep) -> TraversalResult<'schema> {
        let selected = match self.shape {
            Shape::Block(block) => {
                let PathStep::Attribute(name) = step else {
                    return Err("a block requires an attribute step");
                };
                match (block.attributes.get(name), block.blocks.get(name)) {
                    (Some(_), Some(_)) => {
                        return Err("provider schema has an ambiguous field name");
                    }
                    (Some(attribute), None) => Self::from_attribute(attribute)?,
                    (None, Some(nested)) => Self {
                        shape: Shape::NestedBlock(nested),
                        sensitive: false,
                    },
                    (None, None) => {
                        return Err("reference destination is not declared in the schema");
                    }
                }
            }
            Shape::Attributes(attributes) => {
                let PathStep::Attribute(name) = step else {
                    return Err("a nested object requires an attribute step");
                };
                Self::from_attribute(
                    attributes
                        .get(name)
                        .ok_or("reference destination is not declared in the schema")?,
                )?
            }
            Shape::NestedAttributes(nested) => {
                Self::select_nested(nested.nesting, Shape::Attributes(&nested.attributes), step)?
            }
            Shape::NestedBlock(nested) => {
                Self::select_nested(nested.nesting, Shape::Block(&nested.block), step)?
            }
            Shape::Type(value_type) => {
                Self {
                    shape: Shape::Type(match (value_type, step) {
                        (Type::Object(attributes), PathStep::Attribute(name)) => attributes
                            .get(name)
                            .ok_or("reference destination is not declared in the schema")?,
                        (Type::Map(element), PathStep::Key(_))
                        | (Type::List(element), PathStep::Index(_)) => element,
                        (Type::Tuple(elements), PathStep::Index(index)) => usize::try_from(*index)
                            .ok()
                            .and_then(|index| elements.get(index))
                            .ok_or("reference destination tuple index is out of bounds")?,
                        (Type::Set(_), _) => return Err("references cannot traverse sets"),
                        (Type::Dynamic, _) => {
                            return Err("references cannot traverse unresolved dynamic types");
                        }
                        _ => return Err("reference path step does not match the destination type"),
                    }),
                    sensitive: false,
                }
            }
        };
        Ok(Self {
            sensitive: self.sensitive || selected.sensitive,
            ..selected
        })
    }

    fn from_attribute(attribute: &'schema Attribute) -> TraversalResult<'schema> {
        if attribute.presence == Presence::Computed {
            return Err("a reference cannot target or traverse a computed-only attribute");
        }
        if attribute.write_only {
            return Err("a reference cannot target or traverse a write-only attribute");
        }
        Ok(Self {
            shape: attribute
                .nested
                .as_ref()
                .map_or(Shape::Type(&attribute.value_type), Shape::NestedAttributes),
            sensitive: attribute.sensitive,
        })
    }

    fn select_nested(
        nesting: Nesting,
        element: Shape<'schema>,
        step: &PathStep,
    ) -> TraversalResult<'schema> {
        let cursor = Self {
            shape: element,
            sensitive: false,
        };
        match (nesting, step) {
            (Nesting::Single | Nesting::Group, _) => cursor.select(step),
            (Nesting::List, PathStep::Index(_)) | (Nesting::Map, PathStep::Key(_)) => Ok(cursor),
            (Nesting::Set, _) => Err("references cannot traverse sets"),
            _ => Err("reference path step does not match the destination nesting"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, NestingMode};
    use crate::schema::{Documentation, ProviderSchema};

    fn attribute(value_type: Type) -> Attribute {
        Attribute {
            value_type,
            nested: None,
            presence: Presence::Optional,
            sensitive: false,
            write_only: false,
            documentation: Documentation::default(),
        }
    }

    fn block_with(attribute: Attribute) -> Block {
        Block {
            attributes: BTreeMap::from([("argument".into(), attribute)]),
            ..Block::default()
        }
    }

    fn nested_attribute(nesting: Nesting) -> Attribute {
        let attributes = BTreeMap::from([("leaf".into(), attribute(Type::String))]);
        let object = NestedAttributes::object_type_of(&attributes);
        let value_type = match nesting {
            Nesting::Single | Nesting::Group => object,
            Nesting::List => Type::List(Box::new(object)),
            Nesting::Map => Type::Map(Box::new(object)),
            Nesting::Set => Type::Set(Box::new(object)),
        };
        Attribute {
            nested: Some(NestedAttributes {
                attributes,
                nesting,
            }),
            ..attribute(value_type)
        }
    }

    fn nested_path(nesting: Nesting) -> Result<DestinationPath> {
        let mut steps = vec![PathStep::Attribute("argument".into())];
        match nesting {
            Nesting::Single | Nesting::Group => {}
            Nesting::List | Nesting::Set => steps.push(PathStep::Index(0)),
            Nesting::Map => steps.push(PathStep::Key("a.b[0]".into())),
        }
        steps.push(PathStep::Attribute("leaf".into()));
        DestinationPath::try_from(steps)
    }

    #[test]
    fn follows_plain_object_map_list_and_heterogeneous_tuple_fields() -> Result<()> {
        let block = block_with(attribute(Type::Object(BTreeMap::from([(
            "a.b".into(),
            Type::List(Box::new(Type::Map(Box::new(Type::Tuple(vec![
                Type::Boolean,
                Type::String,
            ]))))),
        )]))));
        let path = DestinationPath::try_from(vec![
            PathStep::Attribute("argument".into()),
            PathStep::Attribute("a.b".into()),
            PathStep::Index(3),
            PathStep::Key("0".into()),
            PathStep::Index(1),
        ])?;
        assert_eq!(
            block.reference_destination(&path)?.value_type(),
            &Type::String
        );
        for tail in [PathStep::Index(2), PathStep::Key("1".into())] {
            let mut steps = path.steps().to_vec();
            steps.pop();
            steps.push(tail);
            assert!(
                block
                    .reference_destination(&DestinationPath::try_from(steps)?)
                    .is_err()
            );
        }
        let mut wrong_object_selector = path.steps().to_vec();
        wrong_object_selector[1] = PathStep::Key("a.b".into());
        assert!(
            block
                .reference_destination(&DestinationPath::try_from(wrong_object_selector)?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn follows_schema_nested_attributes_and_blocks_with_each_stable_nesting() -> Result<()> {
        for nesting in [Nesting::Single, Nesting::Group, Nesting::List, Nesting::Map] {
            let attributes = block_with(nested_attribute(nesting));
            let blocks = Block {
                blocks: BTreeMap::from([(
                    "argument".into(),
                    NestedBlock {
                        block: Block {
                            attributes: BTreeMap::from([("leaf".into(), attribute(Type::String))]),
                            ..Block::default()
                        },
                        nesting,
                        minimum_items: 0,
                        maximum_items: 0,
                    },
                )]),
                ..Block::default()
            };
            let path = nested_path(nesting)?;
            for block in [attributes, blocks] {
                assert_eq!(
                    block.reference_destination(&path)?.value_type(),
                    &Type::String
                );
                assert!(
                    block
                        .reference_destination(&DestinationPath::attribute("argument"))
                        .is_err()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn refuses_sets_unresolved_dynamic_and_non_scalar_destinations() -> Result<()> {
        for value_type in [Type::Set(Box::new(Type::String)), Type::Dynamic] {
            let block = block_with(attribute(value_type));
            assert!(
                block
                    .reference_destination(&DestinationPath::attribute("argument"))
                    .is_err()
            );
            let path = DestinationPath::try_from(vec![
                PathStep::Attribute("argument".into()),
                PathStep::Index(0),
            ])?;
            assert!(block.reference_destination(&path).is_err());
        }
        let block = block_with(nested_attribute(Nesting::Set));
        assert!(
            block
                .reference_destination(&nested_path(Nesting::Set)?)
                .is_err()
        );
        let block = Block {
            blocks: BTreeMap::from([(
                "argument".into(),
                NestedBlock {
                    block: Block::default(),
                    nesting: Nesting::Set,
                    minimum_items: 0,
                    maximum_items: 0,
                },
            )]),
            ..Block::default()
        };
        assert!(
            block
                .reference_destination(&nested_path(Nesting::Set)?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_computed_and_write_only_ancestors_and_leaves() -> Result<()> {
        let path = nested_path(Nesting::Single)?;
        for presence in [
            Presence::Required,
            Presence::Optional,
            Presence::OptionalComputed,
        ] {
            let mut parent = nested_attribute(Nesting::Single);
            parent.presence = presence;
            assert!(block_with(parent).reference_destination(&path).is_ok());
        }
        for restriction in [Presence::Computed, Presence::Optional] {
            let mut parent = nested_attribute(Nesting::Single);
            parent.presence = restriction;
            parent.write_only = restriction == Presence::Optional;
            assert!(block_with(parent).reference_destination(&path).is_err());

            let mut parent = nested_attribute(Nesting::Single);
            let nested = parent
                .nested
                .as_mut()
                .ok_or_else(|| path.problem("missing test schema"))?;
            let leaf = nested
                .attributes
                .get_mut("leaf")
                .ok_or_else(|| path.problem("missing test leaf"))?;
            leaf.presence = restriction;
            leaf.write_only = restriction == Presence::Optional;
            assert!(block_with(parent).reference_destination(&path).is_err());
        }
        Ok(())
    }

    #[test]
    fn retains_sensitivity_from_ancestors_without_inheriting_sibling_flags() -> Result<()> {
        let path = nested_path(Nesting::Single)?;
        let mut parent = nested_attribute(Nesting::Single);
        parent.sensitive = true;
        assert!(
            block_with(parent)
                .reference_destination(&path)?
                .is_sensitive()
        );

        let mut parent = nested_attribute(Nesting::Single);
        let nested = parent
            .nested
            .as_mut()
            .ok_or_else(|| path.problem("missing test schema"))?;
        nested.attributes.insert(
            "sibling".into(),
            Attribute {
                sensitive: true,
                ..attribute(Type::String)
            },
        );
        assert!(
            !block_with(parent.clone())
                .reference_destination(&path)?
                .is_sensitive()
        );
        parent
            .nested
            .as_mut()
            .and_then(|nested| nested.attributes.get_mut("leaf"))
            .ok_or_else(|| path.problem("missing test leaf"))?
            .sensitive = true;
        assert!(
            block_with(parent)
                .reference_destination(&path)?
                .is_sensitive()
        );
        Ok(())
    }

    #[test]
    fn ambiguous_and_missing_schema_fields_fail_closed() {
        let path = DestinationPath::attribute("argument");
        let mut block = block_with(attribute(Type::String));
        block.blocks.insert(
            "argument".into(),
            NestedBlock {
                block: Block::default(),
                nesting: Nesting::Single,
                minimum_items: 0,
                maximum_items: 0,
            },
        );
        assert!(block.reference_destination(&path).is_err());
        assert!(Block::default().reference_destination(&path).is_err());
    }

    #[test]
    fn resolved_values_must_match_the_exact_scalar_type() -> Result<()> {
        let path = DestinationPath::attribute("argument");
        for (value_type, value) in [
            (Type::Boolean, Value::Boolean(true)),
            (Type::Number, Value::Number(1.into())),
            (Type::String, Value::String("secret".into())),
        ] {
            let destination =
                block_with(attribute(value_type.clone())).reference_destination(&path)?;
            assert!(destination.accepts(&value));
            assert!(destination.accepts(&Value::typed(value_type, value)));
            for invalid in [
                Value::Null,
                Value::Unknown,
                Value::List(vec![]),
                Value::Object(BTreeMap::new()),
                Value::typed(Type::Dynamic, Value::String("secret".into())),
            ] {
                assert!(!destination.accepts(&invalid));
            }
        }
        let destination = block_with(attribute(Type::String)).reference_destination(&path)?;
        assert!(!destination.accepts(&Value::Number(1.into())));
        assert!(!destination.accepts(&Value::typed(Type::Number, Value::String("secret".into()))));
        Ok(())
    }

    #[test]
    fn protocol_six_nested_computed_flags_survive_implied_type_lowering() -> Result<()> {
        let response = protocol::version6::GetProviderSchemaResponse {
            provider: Some(protocol::version6::Schema {
                block: Some(protocol::version6::Block {
                    attributes: vec![protocol::version6::Attribute {
                        name: "argument".into(),
                        optional: true,
                        nested_type: Some(protocol::version6::Object {
                            nesting: NestingMode::Single as i32,
                            attributes: vec![protocol::version6::Attribute {
                                name: "leaf".into(),
                                r#type: br#""string""#.to_vec(),
                                computed: true,
                                ..Default::default()
                            }],
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let block = ProviderSchema::from_version6(response)?.provider.block;
        let path = nested_path(Nesting::Single)?;
        assert_eq!(
            block.implied_type().at_path(path.steps()),
            Some(Type::String)
        );
        assert!(block.reference_destination(&path).is_err());
        Ok(())
    }
}
