//! Protocol-independent view of provider schemas.
//!
//! Provider schemas describe configuration as blocks of attributes and
//! nested blocks. The *implied type* of a block is the cty object type the
//! provider expects values to be encoded against.

use std::collections::{BTreeMap, HashMap};

use crate::cty::{Type, Value};
use crate::error::Result;
use crate::proto::{self, NestingMode};

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
    pub ty: Type,
    /// Nested attributes (protocol 6 only).
    pub nested: Option<NestedAttributes>,
    /// Who sets the attribute.
    pub presence: Presence,
    /// Value is sensitive and must not be displayed.
    pub sensitive: bool,
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
    const fn from_flags(optional: bool, computed: bool) -> Self {
        match (optional, computed) {
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
    fn from_proto(mode: i32) -> Self {
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
        let mut attrs: BTreeMap<String, Type> = self
            .attributes
            .iter()
            .map(|(name, attr)| (name.clone(), attr.ty.clone()))
            .collect();
        for (name, nested) in &self.blocks {
            attrs.insert(
                name.clone(),
                nested.nesting.wrap(nested.block.implied_type()),
            );
        }
        Type::Object(attrs)
    }

    /// Top-level attribute names flagged sensitive.
    #[must_use]
    pub fn sensitive_attributes(&self) -> Vec<String> {
        self.attributes
            .iter()
            .filter(|(_, a)| a.sensitive)
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Normalize a decoded configuration value the way Terraform does:
    /// absent list/set/map nested blocks become empty collections and absent
    /// group blocks become objects of nulls. Providers rely on this.
    #[must_use]
    pub fn normalize_config(&self, value: Value) -> Value {
        let Value::Object(mut attrs) = value else {
            return value;
        };
        for (name, nested) in &self.blocks {
            let current = attrs.remove(name).unwrap_or(Value::Null);
            let normalized = match (nested.nesting, current) {
                (Nesting::List | Nesting::Set, Value::Null) => Value::List(Vec::new()),
                (Nesting::Map, Value::Null) => Value::Object(BTreeMap::new()),
                (Nesting::Group, Value::Null) => nested
                    .block
                    .normalize_config(Value::Object(BTreeMap::new())),
                (Nesting::List | Nesting::Set, Value::List(items)) => Value::List(
                    items
                        .into_iter()
                        .map(|item| nested.block.normalize_config(item))
                        .collect(),
                ),
                (Nesting::Map, Value::Object(items)) => Value::Object(
                    items
                        .into_iter()
                        .map(|(k, item)| (k, nested.block.normalize_config(item)))
                        .collect(),
                ),
                (Nesting::Single | Nesting::Group, obj @ Value::Object(_)) => {
                    nested.block.normalize_config(obj)
                }
                (_, other) => other,
            };
            attrs.insert(name.clone(), normalized);
        }
        for name in self.attributes.keys() {
            attrs.entry(name.clone()).or_insert(Value::Null);
        }
        Value::Object(attrs)
    }

    /// Compute the proposed new state Terraform hands to
    /// `PlanResourceChange`: configuration values, with computed attributes
    /// the configuration leaves null carried over from prior state.
    ///
    /// This is a simplified port of Terraform's `objchange.ProposedNew`:
    /// it recurses through single/group blocks and single nested attributes
    /// and takes collections of nested blocks from configuration verbatim.
    #[must_use]
    pub fn proposed_new(&self, prior: &Value, config: &Value) -> Value {
        if config.is_null() {
            return Value::Null;
        }
        let empty = Value::Object(BTreeMap::new());
        let prior = if matches!(prior, Value::Object(_)) {
            prior
        } else {
            &empty
        };
        let (Value::Object(config_attrs), Value::Object(prior_attrs)) = (config, prior) else {
            return config.clone();
        };

        let mut out = BTreeMap::new();
        for (name, attr) in &self.attributes {
            let cfg = config_attrs.get(name).unwrap_or(&Value::Null);
            let pri = prior_attrs.get(name).unwrap_or(&Value::Null);
            let value = if attr.presence.is_computed() && cfg.is_null() {
                pri.clone()
            } else if let Some(nested) = &attr.nested
                && nested.nesting == Nesting::Single
                && !pri.is_null()
            {
                let block = Self {
                    attributes: nested.attributes.clone(),
                    blocks: BTreeMap::new(),
                };
                block.proposed_new(pri, cfg)
            } else {
                cfg.clone()
            };
            out.insert(name.clone(), value);
        }
        for (name, nested) in &self.blocks {
            let cfg = config_attrs.get(name).unwrap_or(&Value::Null);
            let pri = prior_attrs.get(name).unwrap_or(&Value::Null);
            let value = match nested.nesting {
                Nesting::Single | Nesting::Group if !cfg.is_null() => {
                    nested.block.proposed_new(pri, cfg)
                }
                _ => cfg.clone(),
            };
            out.insert(name.clone(), value);
        }
        Value::Object(out)
    }
}

// ---------------------------------------------------------------------------
// Protocol conversions
// ---------------------------------------------------------------------------

fn nested_object_type(attributes: &BTreeMap<String, Attribute>, nesting: Nesting) -> Type {
    let object = Type::Object(
        attributes
            .iter()
            .map(|(n, a)| (n.clone(), a.ty.clone()))
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
    pub fn from_v6(resp: proto::v6::GetProviderSchemaResponse) -> Result<Self> {
        Ok(Self {
            provider: resp
                .provider
                .map_or_else(|| Ok(Schema::empty()), v6_schema)?,
            resources: resp
                .resource_schemas
                .into_iter()
                .map(|(name, schema)| Ok((name, v6_schema(schema)?)))
                .collect::<Result<_>>()?,
        })
    }

    /// Convert a protocol 5 `GetSchema` response.
    ///
    /// # Errors
    ///
    /// Returns a codec error when an attribute type cannot be parsed.
    pub fn from_v5(resp: proto::v5::GetProviderSchemaResponse) -> Result<Self> {
        Ok(Self {
            provider: resp
                .provider
                .map_or_else(|| Ok(Schema::empty()), v5_schema)?,
            resources: resp
                .resource_schemas
                .into_iter()
                .map(|(name, schema)| Ok((name, v5_schema(schema)?)))
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

fn v6_schema(schema: proto::v6::Schema) -> Result<Schema> {
    Ok(Schema {
        version: schema.version,
        block: schema
            .block
            .map_or_else(|| Ok(Block::default()), v6_block)?,
    })
}

fn v6_block(block: proto::v6::Block) -> Result<Block> {
    Ok(Block {
        attributes: v6_attributes(block.attributes)?,
        blocks: block
            .block_types
            .into_iter()
            .map(|nb| {
                Ok((
                    nb.type_name,
                    NestedBlock {
                        block: nb.block.map_or_else(|| Ok(Block::default()), v6_block)?,
                        nesting: Nesting::from_proto(nb.nesting),
                    },
                ))
            })
            .collect::<Result<_>>()?,
    })
}

fn v6_attributes(attrs: Vec<proto::v6::Attribute>) -> Result<BTreeMap<String, Attribute>> {
    attrs
        .into_iter()
        .map(|a| {
            let nested = a
                .nested_type
                .map(|obj| {
                    Ok::<_, crate::error::InfraError>(NestedAttributes {
                        attributes: v6_attributes(obj.attributes)?,
                        nesting: Nesting::from_proto(obj.nesting),
                    })
                })
                .transpose()?;
            let ty = match &nested {
                Some(n) => nested_object_type(&n.attributes, n.nesting),
                None => Type::from_json_bytes(&a.r#type)?,
            };
            Ok((
                a.name,
                Attribute {
                    ty,
                    nested,
                    presence: Presence::from_flags(a.optional, a.computed),
                    sensitive: a.sensitive,
                },
            ))
        })
        .collect()
}

fn v5_schema(schema: proto::v5::Schema) -> Result<Schema> {
    Ok(Schema {
        version: schema.version,
        block: schema
            .block
            .map_or_else(|| Ok(Block::default()), v5_block)?,
    })
}

fn v5_block(block: proto::v5::Block) -> Result<Block> {
    Ok(Block {
        attributes: block
            .attributes
            .into_iter()
            .map(|a| {
                Ok((
                    a.name,
                    Attribute {
                        ty: Type::from_json_bytes(&a.r#type)?,
                        nested: None,
                        presence: Presence::from_flags(a.optional, a.computed),
                        sensitive: a.sensitive,
                    },
                ))
            })
            .collect::<Result<_>>()?,
        blocks: block
            .block_types
            .into_iter()
            .map(|nb| {
                Ok((
                    nb.type_name,
                    NestedBlock {
                        block: nb.block.map_or_else(|| Ok(Block::default()), v5_block)?,
                        nesting: Nesting::from_proto(nb.nesting),
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

    fn attr(ty: Type, computed: bool) -> Attribute {
        Attribute {
            ty,
            nested: None,
            presence: Presence::from_flags(!computed, computed),
            sensitive: false,
        }
    }

    fn sample_block() -> Block {
        let mut inner = Block::default();
        inner
            .attributes
            .insert("k".into(), attr(Type::String, false));
        let mut block = Block::default();
        block
            .attributes
            .insert("id".into(), attr(Type::String, true));
        block
            .attributes
            .insert("name".into(), attr(Type::String, false));
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
        let ty = sample_block().implied_type();
        assert_eq!(
            ty.to_json(),
            json!(["object", {"id": "string", "name": "string", "rule": ["list", ["object", {"k": "string"}]]}])
        );
    }

    #[test]
    fn normalize_config_turns_absent_list_blocks_into_empty_lists() {
        let block = sample_block();
        let value = Value::from_config_json(&json!({"name": "x"}), &block.implied_type()).unwrap();
        let normalized = block.normalize_config(value);
        assert_eq!(normalized.attr("rule"), Some(&Value::List(Vec::new())));
    }

    #[test]
    fn proposed_new_keeps_prior_computed_values() {
        let block = sample_block();
        let ty = block.implied_type();
        let prior =
            Value::from_config_json(&json!({"id": "abc", "name": "old", "rule": []}), &ty).unwrap();
        let config =
            block.normalize_config(Value::from_config_json(&json!({"name": "new"}), &ty).unwrap());
        let proposed = block.proposed_new(&prior, &config);
        assert_eq!(proposed.attr("id"), Some(&Value::String("abc".into())));
        assert_eq!(proposed.attr("name"), Some(&Value::String("new".into())));
    }

    #[test]
    fn proposed_new_for_create_leaves_computed_null() {
        let block = sample_block();
        let config = block.normalize_config(
            Value::from_config_json(&json!({"name": "new"}), &block.implied_type()).unwrap(),
        );
        let proposed = block.proposed_new(&Value::Null, &config);
        assert_eq!(proposed.attr("id"), Some(&Value::Null));
    }

    #[test]
    fn proposed_new_of_null_config_is_null() {
        assert_eq!(
            sample_block().proposed_new(&Value::Null, &Value::Null),
            Value::Null
        );
    }
}
