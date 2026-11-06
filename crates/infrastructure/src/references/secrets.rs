//! Find the secrets a configuration places at string arguments.
//!
//! Classification follows the provider schema, never the JSON shape alone: an
//! object at a string position is a secret when it names a `resolver`, because
//! a string position can never legally hold an object. The same object at a
//! dynamic position is ordinary data, and a provider attribute that happens to
//! be called `resolver` inside a nested block is never mistaken for one.

use serde_json::{Map, Value as Json};

use super::DestinationPath;
use crate::error::{InfrastructureError, Result};
use crate::schema::{Attribute, Block, NestedBlock, Nesting, Presence};
use crate::secrets::SecretReference;
use crate::type_system::{PathStep, Type};

/// A `#Secret` at one string position of a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretArgument {
    /// Top-level argument containing the secret.
    pub destination_attribute: String,
    /// Exact schema-classified position of the string it sets.
    pub destination: DestinationPath,
    /// The secret, as configured.
    pub secret: SecretReference,
    /// Provider destination when the argument is write-only.
    pub write_only: Option<DestinationPath>,
    pub(crate) schema_write_only: bool,
}

/// Why a position cannot take a secret, even though it is a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restriction {
    Computed,
    Set,
}

impl Restriction {
    const fn reason(self) -> &'static str {
        match self {
            Self::Computed => "a secret cannot set a computed-only attribute",
            Self::Set => "a secret cannot be placed inside a set",
        }
    }
}

/// Every secret in `configuration`, in path order.
///
/// # Errors
///
/// Returns a value-free configuration error for a secret at a position that
/// cannot take one: a number, a bool, a set element, or a computed-only or
/// write-only attribute, and for a malformed secret.
pub(super) fn secret_arguments(
    configuration: &Map<String, Json>,
    block: &Block,
) -> Result<Vec<SecretArgument>> {
    let mut found = Vec::new();
    Walk { found: &mut found }.block(block, configuration, &Position::root())?;
    found.sort_by(|left, right| left.destination.cmp(&right.destination));
    Ok(found)
}

/// Where the walk is, and what the path so far forbids.
#[derive(Clone)]
struct Position {
    steps: Vec<PathStep>,
    restriction: Option<Restriction>,
    write_only: bool,
}

impl Position {
    const fn root() -> Self {
        Self {
            steps: Vec::new(),
            restriction: None,
            write_only: false,
        }
    }

    fn step(&self, step: PathStep) -> Self {
        let mut steps = self.steps.clone();
        steps.push(step);
        Self {
            steps,
            restriction: self.restriction,
            write_only: self.write_only,
        }
    }

    fn restricted(mut self, restriction: Restriction) -> Self {
        self.restriction.get_or_insert(restriction);
        self
    }

    fn path(&self) -> Result<DestinationPath> {
        DestinationPath::try_from(self.steps.clone())
    }
}

struct Walk<'found> {
    found: &'found mut Vec<SecretArgument>,
}

impl Walk<'_> {
    fn block(&mut self, block: &Block, value: &Map<String, Json>, at: &Position) -> Result<()> {
        for (name, attribute) in &block.attributes {
            if attribute.write_only
                && attribute.presence == Presence::Required
                && !value.contains_key(name)
            {
                return Err(at
                    .step(PathStep::Attribute(name.clone()))
                    .path()?
                    .problem("a required write-only argument must name a secret"));
            }
        }
        for (name, value) in value {
            let here = at.step(PathStep::Attribute(name.clone()));
            if let Some(attribute) = block.attributes.get(name) {
                self.attribute(attribute, value, here)?;
            } else if let Some(nested) = block.blocks.get(name) {
                self.nested_block(nested, value, &here)?;
            }
            // Undeclared names are reported by the provider's validation.
        }
        Ok(())
    }

    fn attribute(&mut self, attribute: &Attribute, value: &Json, at: Position) -> Result<()> {
        let mut at = if attribute.presence == Presence::Computed {
            at.restricted(Restriction::Computed)
        } else {
            at
        };
        at.write_only |= attribute.write_only;
        if attribute.write_only && attribute.presence.is_computed() {
            return Err(at
                .path()?
                .problem("a write-only attribute cannot be computed"));
        }
        if attribute.write_only
            && (!matches!(attribute.value_type, Type::String) || attribute.nested.is_some())
        {
            return Err(at
                .path()?
                .problem("write-only secrets require a string attribute"));
        }
        if attribute.write_only
            && !value
                .as_object()
                .is_some_and(|object| object.contains_key("resolver"))
        {
            return Err(at
                .path()?
                .problem("a write-only argument requires #WriteOnly and a secret descriptor"));
        }
        match &attribute.nested {
            Some(nested) => self.collection(nested.nesting, value, &at, |walk, element, here| {
                walk.attributes(&nested.attributes, element, here)
            }),
            None => self.typed(&attribute.value_type, value, &at),
        }
    }

    fn attributes(
        &mut self,
        attributes: &std::collections::BTreeMap<String, Attribute>,
        value: &Json,
        at: &Position,
    ) -> Result<()> {
        let Json::Object(object) = value else {
            return Ok(());
        };
        for (name, attribute) in attributes {
            if attribute.write_only
                && attribute.presence == Presence::Required
                && !object.contains_key(name)
            {
                return Err(at
                    .step(PathStep::Attribute(name.clone()))
                    .path()?
                    .problem("a required write-only argument must name a secret"));
            }
        }
        for (name, value) in object {
            if let Some(attribute) = attributes.get(name) {
                self.attribute(attribute, value, at.step(PathStep::Attribute(name.clone())))?;
            }
        }
        Ok(())
    }

    fn nested_block(&mut self, nested: &NestedBlock, value: &Json, at: &Position) -> Result<()> {
        self.collection(
            nested.nesting,
            value,
            at,
            |walk, element, here| match element {
                Json::Object(object) => walk.block(&nested.block, object, here),
                _ => Ok(()),
            },
        )
    }

    /// Visit each object of a nested attribute or block collection.
    fn collection(
        &mut self,
        nesting: Nesting,
        value: &Json,
        at: &Position,
        mut visit: impl FnMut(&mut Self, &Json, &Position) -> Result<()>,
    ) -> Result<()> {
        match (nesting, value) {
            (Nesting::Single | Nesting::Group, _) => visit(self, value, at),
            (Nesting::List, Json::Array(elements)) => {
                for (index, element) in elements.iter().enumerate() {
                    visit(self, element, &at.step(index_step(index)?))?;
                }
                Ok(())
            }
            (Nesting::Set, Json::Array(elements)) => {
                let at = at.clone().restricted(Restriction::Set);
                for (index, element) in elements.iter().enumerate() {
                    visit(self, element, &at.step(index_step(index)?))?;
                }
                Ok(())
            }
            (Nesting::Map, Json::Object(elements)) => {
                for (key, element) in elements {
                    visit(self, element, &at.step(PathStep::Key(key.clone())))?;
                }
                Ok(())
            }
            // A mismatched shape is reported when the configuration is decoded.
            _ => Ok(()),
        }
    }

    fn typed(&mut self, value_type: &Type, value: &Json, at: &Position) -> Result<()> {
        if *value_type == Type::Dynamic {
            crate::write_only::reject_markers(value)?;
        }
        match (value_type, value) {
            (Type::String, Json::Object(object)) if object.contains_key("resolver") => {
                self.secret(object, at)
            }
            (Type::Number | Type::Boolean, Json::Object(object))
                if object.contains_key("resolver") =>
            {
                Err(at
                    .path()?
                    .problem("a secret can only set a string argument"))
            }
            (Type::List(element), Json::Array(elements)) => {
                for (index, item) in elements.iter().enumerate() {
                    self.typed(element, item, &at.step(index_step(index)?))?;
                }
                Ok(())
            }
            (Type::Set(element), Json::Array(elements)) => {
                let at = at.clone().restricted(Restriction::Set);
                for (index, item) in elements.iter().enumerate() {
                    self.typed(element, item, &at.step(index_step(index)?))?;
                }
                Ok(())
            }
            (Type::Tuple(elements), Json::Array(items)) => {
                for (index, (element, item)) in elements.iter().zip(items).enumerate() {
                    self.typed(element, item, &at.step(index_step(index)?))?;
                }
                Ok(())
            }
            (Type::Map(element), Json::Object(items)) => {
                for (key, item) in items {
                    self.typed(element, item, &at.step(PathStep::Key(key.clone())))?;
                }
                Ok(())
            }
            (Type::Object(attributes), Json::Object(items)) => {
                for (name, item) in items {
                    if let Some(attribute_type) = attributes.get(name) {
                        self.typed(
                            attribute_type,
                            item,
                            &at.step(PathStep::Attribute(name.clone())),
                        )?;
                    }
                }
                Ok(())
            }
            // Dynamic values are data, and every other mismatch is reported
            // when the configuration is decoded against its type.
            _ => Ok(()),
        }
    }

    fn secret(&mut self, object: &Map<String, Json>, at: &Position) -> Result<()> {
        let destination = at.path()?;
        if let Some(restriction) = at.restriction {
            return Err(destination.problem(restriction.reason()));
        }
        let marked = match object.get("writeOnly") {
            None if !at.write_only => false,
            Some(Json::Bool(true)) => true,
            _ => {
                return Err(destination.problem(
                    "write-only arguments require writeOnly: true and a secret descriptor",
                ));
            }
        };
        let secret = SecretReference::from_descriptor(object.clone())
            .map_err(|reason| destination.problem(reason))?;
        self.found.push(SecretArgument {
            destination_attribute: destination.root_attribute().to_string(),
            write_only: marked.then(|| destination.clone()),
            schema_write_only: at.write_only,
            destination,
            secret,
        });
        Ok(())
    }
}

fn index_step(index: usize) -> Result<PathStep> {
    i64::try_from(index)
        .map(PathStep::Index)
        .map_err(|_| InfrastructureError::configuration("a configuration list is too long"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::schema::{Documentation, NestedAttributes};

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

    fn secret() -> Json {
        json!({"resolver": "onepassword", "ref": "op://vault/item/field"})
    }

    fn block() -> Block {
        let nested_object = BTreeMap::from([
            ("token".into(), attribute(Type::String)),
            ("resolver".into(), attribute(Type::String)),
        ]);
        let mut write_only = attribute(Type::String);
        write_only.write_only = true;
        let mut computed = attribute(Type::String);
        computed.presence = Presence::Computed;
        Block {
            attributes: BTreeMap::from([
                ("value".into(), attribute(Type::String)),
                ("count".into(), attribute(Type::Number)),
                ("enabled".into(), attribute(Type::Boolean)),
                ("tags".into(), attribute(Type::Map(Box::new(Type::String)))),
                (
                    "names".into(),
                    attribute(Type::List(Box::new(Type::String))),
                ),
                (
                    "members".into(),
                    attribute(Type::Set(Box::new(Type::String))),
                ),
                ("metadata".into(), attribute(Type::Dynamic)),
                (
                    "pair".into(),
                    attribute(Type::Tuple(vec![Type::Number, Type::String])),
                ),
                (
                    "object".into(),
                    attribute(Type::Object(BTreeMap::from([(
                        "inner".into(),
                        Type::String,
                    )]))),
                ),
                (
                    "credentials".into(),
                    Attribute {
                        nested: Some(NestedAttributes {
                            attributes: nested_object,
                            nesting: Nesting::List,
                        }),
                        ..attribute(Type::Dynamic)
                    },
                ),
                ("value_wo".into(), write_only),
                ("id".into(), computed),
            ]),
            blocks: BTreeMap::from([(
                "upstream".into(),
                NestedBlock {
                    block: Block {
                        attributes: BTreeMap::from([("resolver".into(), attribute(Type::String))]),
                        ..Block::default()
                    },
                    nesting: Nesting::Set,
                    minimum_items: 0,
                    maximum_items: 0,
                },
            )]),
            documentation: Documentation::default(),
        }
    }

    fn found(configuration: &Json) -> Result<Vec<String>> {
        let Json::Object(configuration) = configuration else {
            unreachable!("test configurations are objects")
        };
        Ok(secret_arguments(configuration, &block())?
            .into_iter()
            .map(|argument| argument.destination.to_string())
            .collect())
    }

    #[test]
    fn finds_secrets_at_every_string_position() -> Result<()> {
        let configuration = json!({
            "value": secret(),
            "tags": {"team": secret(), "plain": "x"},
            "names": ["a", secret()],
            "pair": [1, secret()],
            "object": {"inner": secret()},
            "credentials": [{"token": secret()}],
        });
        assert_eq!(
            found(&configuration)?,
            [
                "credentials[0].token",
                "names[1]",
                "object.inner",
                "pair[1]",
                "tags[<hidden key>]",
                "value",
            ]
        );
        Ok(())
    }

    #[test]
    fn keeps_ordinary_values_and_dynamic_data() -> Result<()> {
        let configuration = json!({
            "value": "literal",
            "metadata": secret(),
            "credentials": [{"resolver": "1.1.1.1"}],
            "upstream": [{"resolver": "9.9.9.9"}],
            "object": {"inner": {"not": "a secret"}},
        });
        assert!(found(&configuration)?.is_empty());
        Ok(())
    }

    #[test]
    fn rejects_positions_that_cannot_take_a_secret() {
        for (configuration, expected) in [
            (
                json!({"count": secret()}),
                "count: a secret can only set a string argument",
            ),
            (
                json!({"enabled": secret()}),
                "enabled: a secret can only set a string argument",
            ),
            (
                json!({"members": [secret()]}),
                "members[0]: a secret cannot be placed inside a set",
            ),
            (
                json!({"upstream": [{"resolver": secret()}]}),
                "upstream[0].resolver: a secret cannot be placed inside a set",
            ),
            (
                json!({"value_wo": secret()}),
                "value_wo: write-only arguments require writeOnly: true",
            ),
            (
                json!({"id": secret()}),
                "id: a secret cannot set a computed-only attribute",
            ),
            (
                json!({"value": {"resolver": 7}}),
                "value: a secret's `resolver` must be a string",
            ),
        ] {
            let error = found(&configuration).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("op://"), "{error}");
        }
    }

    #[test]
    fn records_the_secret_and_its_top_level_argument() -> Result<()> {
        let Json::Object(configuration) = json!({"tags": {"team": secret()}}) else {
            unreachable!()
        };
        let arguments = secret_arguments(&configuration, &block())?;
        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments[0].destination_attribute, "tags");
        assert_eq!(arguments[0].secret.resolver(), "onepassword");
        assert_eq!(
            arguments[0]
                .destination
                .json_value(&Json::Object(configuration.clone())),
            Some(&secret())
        );
        Ok(())
    }
}
