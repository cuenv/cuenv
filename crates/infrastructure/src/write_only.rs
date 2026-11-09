//! Schema-classified write-only arguments and the provider-state null invariant.

use std::collections::BTreeMap;

use crate::error::{InfrastructureError, Result};
use crate::schema::{Attribute, Block, Nesting};
use crate::type_system::{PathStep, Value};

/// Every non-null write-only leaf in a provider result is a protocol error.
pub fn check_state(block: &Block, value: &Value, context: &str) -> Result<()> {
    let mut invalid = Vec::new();
    Walk {
        path: Vec::new(),
        visit: &mut |path, value: &Value| {
            if !value.is_null() {
                register_strings(value);
                invalid.push(crate::object_change::render_diagnostic_steps(path));
            }
        },
    }
    .block(block, value);
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(InfrastructureError::plugin(format!(
            "{context}: provider returned a non-null write-only attribute at {}; no returned state was accepted; the remote object may have changed",
            invalid.join(", ")
        )))
    }
}

/// Schema-classified write-only leaves present in a provider value.
pub fn paths(block: &Block, value: &Value) -> Vec<Vec<PathStep>> {
    let mut paths = Vec::new();
    Walk {
        path: Vec::new(),
        visit: &mut |path, _| paths.push(path.to_vec()),
    }
    .block(block, value);
    paths
}

fn untyped(value: &Value) -> &Value {
    match value {
        Value::Typed(typed) => untyped(&typed.value),
        value => value,
    }
}

#[derive(Clone, Copy)]
enum Element<'schema> {
    Block(&'schema Block),
    Attributes(&'schema BTreeMap<String, Attribute>),
}

struct Walk<'visit> {
    path: Vec<PathStep>,
    visit: &'visit mut dyn FnMut(&[PathStep], &Value),
}

impl Walk<'_> {
    fn block(&mut self, block: &Block, value: &Value) {
        if value.is_unknown() && block.contains_write_only() {
            (self.visit)(&self.path, value);
            return;
        }
        let Value::Object(values) = untyped(value) else {
            return;
        };
        self.attributes(&block.attributes, values);
        for (name, nested) in &block.blocks {
            if let Some(value) = values.get(name) {
                self.path.push(PathStep::Attribute(name.clone()));
                self.collection(nested.nesting, value, Element::Block(&nested.block));
                self.path.pop();
            }
        }
    }

    fn attributes(
        &mut self,
        attributes: &BTreeMap<String, Attribute>,
        values: &BTreeMap<String, Value>,
    ) {
        for (name, attribute) in attributes {
            let Some(value) = values.get(name) else {
                continue;
            };
            self.path.push(PathStep::Attribute(name.clone()));
            if attribute.write_only {
                (self.visit)(&self.path, value);
            } else if let Some(nested) = &attribute.nested {
                self.collection(
                    nested.nesting,
                    value,
                    Element::Attributes(&nested.attributes),
                );
            }
            self.path.pop();
        }
    }

    fn element(&mut self, schema: Element<'_>, value: &Value) {
        if value.is_unknown() && contains_write_only(schema) {
            (self.visit)(&self.path, value);
            return;
        }
        match schema {
            Element::Block(block) => self.block(block, value),
            Element::Attributes(attributes) => {
                if let Value::Object(values) = untyped(value) {
                    self.attributes(attributes, values);
                }
            }
        }
    }

    fn collection(&mut self, nesting: Nesting, value: &Value, schema: Element<'_>) {
        if value.is_unknown() && contains_write_only(schema) {
            (self.visit)(&self.path, value);
            return;
        }
        match (nesting, untyped(value)) {
            (Nesting::Single | Nesting::Group, value) => self.element(schema, value),
            (Nesting::List | Nesting::Set, Value::List(values)) => {
                for (index, value) in values.iter().enumerate() {
                    if let Ok(index) = i64::try_from(index) {
                        self.path.push(PathStep::Index(index));
                        self.element(schema, value);
                        self.path.pop();
                    }
                }
            }
            (Nesting::Map, Value::Object(values)) => {
                for (key, value) in values {
                    self.path.push(PathStep::Key(key.clone()));
                    self.element(schema, value);
                    self.path.pop();
                }
            }
            _ => {}
        }
    }
}

fn register_strings(value: &Value) {
    match untyped(value) {
        Value::String(value) => cuenv_events::register_secret(value.clone()),
        Value::List(values) => {
            for value in values {
                register_strings(value);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                register_strings(value);
            }
        }
        _ => {}
    }
}

/// Refuse write-only secret markers outside schema-classified managed arguments.
pub fn reject_markers(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            if fields.contains_key("writeOnly") && fields.contains_key("resolver") {
                return Err(InfrastructureError::configuration(
                    "writeOnly is only supported on managed string arguments marked write-only by the provider schema",
                ));
            }
            for value in fields.values() {
                reject_markers(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                reject_markers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn contains_write_only(schema: Element<'_>) -> bool {
    match schema {
        Element::Block(block) => block.contains_write_only(),
        Element::Attributes(attributes) => attributes.values().any(Attribute::contains_write_only),
    }
}
