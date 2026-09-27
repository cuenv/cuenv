//! Protocol-independent view of provider schemas.
//!
//! Provider schemas describe configuration as blocks of attributes and
//! nested blocks. The *implied type* of a block is the cty object type the
//! provider expects values to be encoded against.

use std::collections::{BTreeMap, HashMap};

use crate::error::Result;
use crate::protocol::{self, NestingMode};
use crate::type_system::{Type, Value};

/// A provider's schemas for itself and its managed resources.
#[derive(Debug, Clone)]
pub struct ProviderSchema {
    /// Schema of the `provider` configuration block.
    pub provider: Schema,
    /// Managed resource schemas, keyed by resource type name.
    pub resources: HashMap<String, Schema>,
}

/// A versioned schema.
#[derive(Debug, Clone)]
pub struct Schema {
    /// Schema version, used for state upgrades.
    pub version: i64,
    /// Root configuration block.
    pub block: Block,
}

/// A configuration block.
#[derive(Debug, Clone, Default)]
pub struct Block {
    /// Attributes by name.
    pub attributes: BTreeMap<String, Attribute>,
    /// Nested block types by name.
    pub blocks: BTreeMap<String, NestedBlock>,
}

/// A block attribute.
#[derive(Debug, Clone)]
pub struct Attribute {
    /// Value type (for nested attributes, the implied type of the nesting).
    pub value_type: Type,
    /// Nested attributes (protocol 6 only).
    pub nested: Option<NestedAttributes>,
    /// Who sets the attribute.
    pub presence: Presence,
    /// Value is sensitive and must not be displayed.
    pub sensitive: bool,
}

/// Optional and computed flags as reported by a provider schema.
#[derive(Debug, Clone, Copy)]
struct AttributeFlags {
    optional: bool,
    computed: bool,
}

/// Who may set an attribute's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Configuration must set it.
    Required,
    /// Configuration may set it.
    Optional,
    /// Only the provider sets it.
    Computed,
    /// Configuration may set it; otherwise the provider does.
    OptionalComputed,
}

impl Presence {
    const fn from_flags(flags: AttributeFlags) -> Self {
        match (flags.optional, flags.computed) {
            (true, true) => Self::OptionalComputed,
            (false, true) => Self::Computed,
            (true, false) => Self::Optional,
            (false, false) => Self::Required,
        }
    }

    /// Whether the provider may supply the value.
    #[must_use]
    pub const fn is_computed(self) -> bool {
        matches!(self, Self::Computed | Self::OptionalComputed)
    }
}

impl Attribute {
    /// Whether this attribute, or any nested attribute, is sensitive.
    #[must_use]
    pub fn contains_sensitive(&self) -> bool {
        self.sensitive
            || self
                .nested
                .as_ref()
                .is_some_and(|nested| nested.attributes.values().any(Self::contains_sensitive))
    }
}

/// Nested attributes of a protocol 6 attribute.
#[derive(Debug, Clone)]
pub struct NestedAttributes {
    /// Attributes of each nested object.
    pub attributes: BTreeMap<String, Attribute>,
    /// How the nested objects are collected.
    pub nesting: Nesting,
}

/// A nested block type.
#[derive(Debug, Clone)]
pub struct NestedBlock {
    /// Schema of each nested block.
    pub block: Block,
    /// How nested blocks are collected.
    pub nesting: Nesting,
}

/// Nesting mode for nested blocks and nested attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nesting {
    /// At most one block; absent means null.
    Single,
    /// Exactly one block; absent means an object of nulls.
    Group,
    /// Ordered list.
    List,
    /// Unordered set.
    Set,
    /// Map keyed by label.
    Map,
}

impl Nesting {
    fn from_protocol(mode: i32) -> Self {
        match NestingMode::try_from(mode).unwrap_or(NestingMode::Invalid) {
            NestingMode::List => Self::List,
            NestingMode::Set => Self::Set,
            NestingMode::Map => Self::Map,
            NestingMode::Group => Self::Group,
            NestingMode::Single | NestingMode::Invalid => Self::Single,
        }
    }

    fn wrap(self, object: Type) -> Type {
        match self {
            Self::Single | Self::Group => object,
            Self::List => Type::List(Box::new(object)),
            Self::Set => Type::Set(Box::new(object)),
            Self::Map => Type::Map(Box::new(object)),
        }
    }
}

impl Block {
    /// The cty object type values of this block are encoded against.
    #[must_use]
    pub fn implied_type(&self) -> Type {
        let mut attributes: BTreeMap<String, Type> = self
            .attributes
            .iter()
            .map(|(name, attribute)| (name.clone(), attribute.value_type.clone()))
            .collect();
        for (name, nested) in &self.blocks {
            attributes.insert(
                name.clone(),
                nested.nesting.wrap(nested.block.implied_type()),
            );
        }
        Type::Object(attributes)
    }

    /// Top-level attribute and block names whose values contain anything
    /// sensitive, at any depth. Rendering masks the whole top-level value.
    #[must_use]
    pub fn sensitive_attributes(&self) -> Vec<String> {
        let attributes = self
            .attributes
            .iter()
            .filter(|(_, attribute)| attribute.contains_sensitive())
            .map(|(name, _)| name.clone());
        let blocks = self
            .blocks
            .iter()
            .filter(|(_, nested)| nested.block.contains_sensitive())
            .map(|(name, _)| name.clone());
        attributes.chain(blocks).collect()
    }

    /// Whether any attribute in this block, at any depth, is sensitive.
    #[must_use]
    pub fn contains_sensitive(&self) -> bool {
        self.attributes.values().any(Attribute::contains_sensitive)
            || self
                .blocks
                .values()
                .any(|nested| nested.block.contains_sensitive())
    }

    /// Normalize a decoded configuration value the way Terraform does:
    /// absent list/set/map nested blocks become empty collections and absent
    /// group blocks become objects of nulls. Providers rely on this.
    #[must_use]
    pub fn normalize_configuration(&self, value: Value) -> Value {
        let Value::Object(mut attributes) = value else {
            return value;
        };
        for (name, nested) in &self.blocks {
            let current = attributes.remove(name).unwrap_or(Value::Null);
            let normalized = match (nested.nesting, current) {
                (Nesting::List | Nesting::Set, Value::Null) => Value::List(Vec::new()),
                (Nesting::Map, Value::Null) => Value::Object(BTreeMap::new()),
                (Nesting::Group, Value::Null) => nested
                    .block
                    .normalize_configuration(Value::Object(BTreeMap::new())),
                (Nesting::List | Nesting::Set, Value::List(items)) => Value::List(
                    items
                        .into_iter()
                        .map(|item| nested.block.normalize_configuration(item))
                        .collect(),
                ),
                (Nesting::Map, Value::Object(items)) => Value::Object(
                    items
                        .into_iter()
                        .map(|(key, item)| (key, nested.block.normalize_configuration(item)))
                        .collect(),
                ),
                (Nesting::Single | Nesting::Group, object @ Value::Object(_)) => {
                    nested.block.normalize_configuration(object)
                }
                (_, other) => other,
            };
            attributes.insert(name.clone(), normalized);
        }
        for name in self.attributes.keys() {
            attributes.entry(name.clone()).or_insert(Value::Null);
        }
        Value::Object(attributes)
    }

    /// Compute the proposed new state Terraform hands to
    /// `PlanResourceChange`: configuration values, with computed attributes
    /// the configuration leaves null carried over from prior state.
    ///
    /// This is a simplified port of Terraform's `objchange.ProposedNew`:
    /// it recurses through single/group blocks and single nested attributes
    /// and takes collections of nested blocks from configuration verbatim.
    #[must_use]
    pub fn proposed_new(&self, prior: &Value, configuration: &Value) -> Value {
        if configuration.is_null() {
            return Value::Null;
        }
        let empty = Value::Object(BTreeMap::new());
        let prior = if matches!(prior, Value::Object(_)) {
            prior
        } else {
            &empty
        };
        let (Value::Object(configuration_attributes), Value::Object(prior_attributes)) =
            (configuration, prior)
        else {
            return configuration.clone();
        };

        let mut proposed = BTreeMap::new();
        for (name, attribute) in &self.attributes {
            let configured = configuration_attributes.get(name).unwrap_or(&Value::Null);
            let prior_value = prior_attributes.get(name).unwrap_or(&Value::Null);
            let value = if attribute.presence.is_computed() && configured.is_null() {
                prior_value.clone()
            } else if let Some(nested) = &attribute.nested
                && nested.nesting == Nesting::Single
                && !prior_value.is_null()
            {
                let block = Self {
                    attributes: nested.attributes.clone(),
                    blocks: BTreeMap::new(),
                };
                block.proposed_new(prior_value, configured)
            } else {
                configured.clone()
            };
            proposed.insert(name.clone(), value);
        }
        for (name, nested) in &self.blocks {
            let configured = configuration_attributes.get(name).unwrap_or(&Value::Null);
            let prior_value = prior_attributes.get(name).unwrap_or(&Value::Null);
            let value = match nested.nesting {
                Nesting::Single | Nesting::Group if !configured.is_null() => {
                    nested.block.proposed_new(prior_value, configured)
                }
                _ => configured.clone(),
            };
            proposed.insert(name.clone(), value);
        }
        Value::Object(proposed)
    }
}

// ---------------------------------------------------------------------------
// Protocol conversions
// ---------------------------------------------------------------------------

fn nested_object_type(attributes: &BTreeMap<String, Attribute>, nesting: Nesting) -> Type {
    let object = Type::Object(
        attributes
            .iter()
            .map(|(name, attribute)| (name.clone(), attribute.value_type.clone()))
            .collect(),
    );
    nesting.wrap(object)
}

impl ProviderSchema {
    /// Convert a protocol 6 `GetProviderSchema` response.
    ///
    /// # Errors
    ///
    /// Returns a codec error when an attribute type cannot be parsed.
    pub fn from_version6(response: protocol::version6::GetProviderSchemaResponse) -> Result<Self> {
        Ok(Self {
            provider: response
                .provider
                .map_or_else(|| Ok(Schema::empty()), version6_schema)?,
            resources: response
                .resource_schemas
                .into_iter()
                .map(|(name, schema)| Ok((name, version6_schema(schema)?)))
                .collect::<Result<_>>()?,
        })
    }

    /// Convert a protocol 5 `GetSchema` response.
    ///
    /// # Errors
    ///
    /// Returns a codec error when an attribute type cannot be parsed.
    pub fn from_version5(response: protocol::version5::GetProviderSchemaResponse) -> Result<Self> {
        Ok(Self {
            provider: response
                .provider
                .map_or_else(|| Ok(Schema::empty()), version5_schema)?,
            resources: response
                .resource_schemas
                .into_iter()
                .map(|(name, schema)| Ok((name, version5_schema(schema)?)))
                .collect::<Result<_>>()?,
        })
    }
}

impl Schema {
    fn empty() -> Self {
        Self {
            version: 0,
            block: Block::default(),
        }
    }
}

fn version6_schema(schema: protocol::version6::Schema) -> Result<Schema> {
    Ok(Schema {
        version: schema.version,
        block: schema
            .block
            .map_or_else(|| Ok(Block::default()), version6_block)?,
    })
}

fn version6_block(block: protocol::version6::Block) -> Result<Block> {
    Ok(Block {
        attributes: version6_attributes(block.attributes)?,
        blocks: block
            .block_types
            .into_iter()
            .map(|nested_block| {
                Ok((
                    nested_block.type_name,
                    NestedBlock {
                        block: nested_block
                            .block
                            .map_or_else(|| Ok(Block::default()), version6_block)?,
                        nesting: Nesting::from_protocol(nested_block.nesting),
                    },
                ))
            })
            .collect::<Result<_>>()?,
    })
}

fn version6_attributes(
    attributes: Vec<protocol::version6::Attribute>,
) -> Result<BTreeMap<String, Attribute>> {
    attributes
        .into_iter()
        .map(|attribute| {
            let nested = attribute
                .nested_type
                .map(|object| {
                    Ok::<_, crate::error::InfrastructureError>(NestedAttributes {
                        attributes: version6_attributes(object.attributes)?,
                        nesting: Nesting::from_protocol(object.nesting),
                    })
                })
                .transpose()?;
            let value_type = match &nested {
                Some(nested_attributes) => {
                    nested_object_type(&nested_attributes.attributes, nested_attributes.nesting)
                }
                None => Type::from_json_bytes(&attribute.r#type)?,
            };
            Ok((
                attribute.name,
                Attribute {
                    value_type,
                    nested,
                    presence: Presence::from_flags(AttributeFlags {
                        optional: attribute.optional,
                        computed: attribute.computed,
                    }),
                    sensitive: attribute.sensitive,
                },
            ))
        })
        .collect()
}

fn version5_schema(schema: protocol::version5::Schema) -> Result<Schema> {
    Ok(Schema {
        version: schema.version,
        block: schema
            .block
            .map_or_else(|| Ok(Block::default()), version5_block)?,
    })
}

fn version5_block(block: protocol::version5::Block) -> Result<Block> {
    Ok(Block {
        attributes: block
            .attributes
            .into_iter()
            .map(|attribute| {
                Ok((
                    attribute.name,
                    Attribute {
                        value_type: Type::from_json_bytes(&attribute.r#type)?,
                        nested: None,
                        presence: Presence::from_flags(AttributeFlags {
                            optional: attribute.optional,
                            computed: attribute.computed,
                        }),
                        sensitive: attribute.sensitive,
                    },
                ))
            })
            .collect::<Result<_>>()?,
        blocks: block
            .block_types
            .into_iter()
            .map(|nested_block| {
                Ok((
                    nested_block.type_name,
                    NestedBlock {
                        block: nested_block
                            .block
                            .map_or_else(|| Ok(Block::default()), version5_block)?,
                        nesting: Nesting::from_protocol(nested_block.nesting),
                    },
                ))
            })
            .collect::<Result<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attribute(value_type: Type, presence: Presence) -> Attribute {
        Attribute {
            value_type,
            nested: None,
            presence,
            sensitive: false,
        }
    }

    fn sample_block() -> Block {
        let mut inner = Block::default();
        inner
            .attributes
            .insert("label".into(), attribute(Type::String, Presence::Optional));
        let mut block = Block::default();
        block
            .attributes
            .insert("id".into(), attribute(Type::String, Presence::Computed));
        block
            .attributes
            .insert("name".into(), attribute(Type::String, Presence::Optional));
        block.blocks.insert(
            "rule".into(),
            NestedBlock {
                block: inner,
                nesting: Nesting::List,
            },
        );
        block
    }

    #[test]
    fn implied_type_wraps_nested_blocks() {
        let value_type = sample_block().implied_type();
        assert_eq!(
            value_type.to_json(),
            json!(["object", {"id": "string", "name": "string", "rule": ["list", ["object", {"label": "string"}]]}])
        );
    }

    #[test]
    fn normalize_configuration_turns_absent_list_blocks_into_empty_lists() {
        let block = sample_block();
        let value =
            Value::from_configuration_json(&json!({"name": "example"}), &block.implied_type())
                .unwrap();
        let normalized = block.normalize_configuration(value);
        assert_eq!(normalized.attribute("rule"), Some(&Value::List(Vec::new())));
    }

    #[test]
    fn proposed_new_keeps_prior_computed_values() {
        let block = sample_block();
        let value_type = block.implied_type();
        let prior = Value::from_configuration_json(
            &json!({"id": "abc", "name": "old", "rule": []}),
            &value_type,
        )
        .unwrap();
        let configuration = block.normalize_configuration(
            Value::from_configuration_json(&json!({"name": "new"}), &value_type).unwrap(),
        );
        let proposed = block.proposed_new(&prior, &configuration);
        assert_eq!(proposed.attribute("id"), Some(&Value::String("abc".into())));
        assert_eq!(
            proposed.attribute("name"),
            Some(&Value::String("new".into()))
        );
    }

    #[test]
    fn proposed_new_for_create_leaves_computed_null() {
        let block = sample_block();
        let configuration = block.normalize_configuration(
            Value::from_configuration_json(&json!({"name": "new"}), &block.implied_type()).unwrap(),
        );
        let proposed = block.proposed_new(&Value::Null, &configuration);
        assert_eq!(proposed.attribute("id"), Some(&Value::Null));
    }

    #[test]
    fn nested_sensitive_values_mark_their_top_level_attribute() {
        let mut credentials = Block::default();
        credentials.attributes.insert(
            "client_key".into(),
            Attribute {
                sensitive: true,
                ..attribute(Type::String, Presence::Computed)
            },
        );
        let mut block = sample_block();
        block.blocks.insert(
            "master_auth".into(),
            NestedBlock {
                block: credentials,
                nesting: Nesting::List,
            },
        );
        let mut nested_secret = attribute(Type::String, Presence::Optional);
        nested_secret.sensitive = true;
        block.attributes.insert(
            "settings".into(),
            Attribute {
                nested: Some(NestedAttributes {
                    attributes: BTreeMap::from([("password".to_string(), nested_secret)]),
                    nesting: Nesting::Single,
                }),
                ..attribute(Type::Dynamic, Presence::Optional)
            },
        );
        let sensitive = block.sensitive_attributes();
        assert!(
            sensitive.contains(&"master_auth".to_string()),
            "{sensitive:?}"
        );
        assert!(sensitive.contains(&"settings".to_string()), "{sensitive:?}");
        assert!(!sensitive.contains(&"name".to_string()), "{sensitive:?}");
    }

    #[test]
    fn proposed_new_of_null_configuration_is_null() {
        assert_eq!(
            sample_block().proposed_new(&Value::Null, &Value::Null),
            Value::Null
        );
    }
}
