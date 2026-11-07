//! What changed between two snapshots, declaration by declaration and field
//! by field.

use std::collections::BTreeMap;

use super::snapshot::{Snapshot, Value};

/// One step of a path into a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Segment {
    /// A struct field or map key.
    Field(String),
    /// A list element.
    Index(usize),
}

/// One field that differs between the base and the head.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FieldChange {
    /// Where the field is in the declaration, such as `configuration.name`.
    pub(super) path: Vec<Segment>,
    /// The base value; `None` when the head adds the field.
    pub(super) before: Option<Value>,
    /// The head value; `None` when the head removes the field.
    pub(super) after: Option<Value>,
}

/// How one declaration changed.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Change {
    /// Only the head declares it; its fields, as paths and values.
    Added(Vec<(Vec<Segment>, Value)>),
    /// Only the base declares it.
    Removed,
    /// Both declare it, with these differences.
    Changed(Vec<FieldChange>),
}

/// Every declaration that differs, by section and address.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Changes {
    pub(super) providers: BTreeMap<String, Change>,
    pub(super) resources: BTreeMap<String, Change>,
    pub(super) data_sources: BTreeMap<String, Change>,
}

impl Changes {
    /// What changed from `base` to `head`.
    pub(super) fn between(base: &Snapshot, head: &Snapshot) -> Self {
        Self {
            providers: section(&base.providers, &head.providers),
            resources: section(&base.resources, &head.resources),
            data_sources: section(&base.data_sources, &head.data_sources),
        }
    }

    /// Whether nothing changed.
    pub(super) fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.resources.is_empty() && self.data_sources.is_empty()
    }
}

fn section(
    base: &BTreeMap<String, Value>,
    head: &BTreeMap<String, Value>,
) -> BTreeMap<String, Change> {
    let mut changes = BTreeMap::new();
    for (address, before) in base {
        match head.get(address) {
            None => {
                changes.insert(address.clone(), Change::Removed);
            }
            Some(after) if after != before => {
                let mut fields = Vec::new();
                compare(&mut Vec::new(), (before, after), &mut fields);
                changes.insert(address.clone(), Change::Changed(fields));
            }
            Some(_) => {}
        }
    }
    for (address, after) in head {
        if !base.contains_key(address) {
            let mut fields = Vec::new();
            leaves(&mut Vec::new(), after, &mut fields);
            changes.insert(address.clone(), Change::Added(fields));
        }
    }
    changes
}

/// Record how `before` became `after` at `path`: field by field in objects,
/// element by element in lists of structured values, and as a whole
/// otherwise (a list of scalars reads best as one value).
fn compare(path: &mut Vec<Segment>, values: (&Value, &Value), changes: &mut Vec<FieldChange>) {
    match values {
        (before, after) if before == after => {}
        (Value::Object(before), Value::Object(after)) => {
            let keys: std::collections::BTreeSet<&String> =
                before.keys().chain(after.keys()).collect();
            for key in keys {
                path.push(Segment::Field(key.clone()));
                match (before.get(key), after.get(key)) {
                    (Some(before), Some(after)) => compare(path, (before, after), changes),
                    (before, after) => changes.push(FieldChange {
                        path: path.clone(),
                        before: before.cloned(),
                        after: after.cloned(),
                    }),
                }
                path.pop();
            }
        }
        (Value::List(before), Value::List(after))
            if before.iter().chain(after).any(Value::is_structured) =>
        {
            for index in 0..before.len().max(after.len()) {
                path.push(Segment::Index(index));
                match (before.get(index), after.get(index)) {
                    (Some(before), Some(after)) => compare(path, (before, after), changes),
                    (before, after) => changes.push(FieldChange {
                        path: path.clone(),
                        before: before.cloned(),
                        after: after.cloned(),
                    }),
                }
                path.pop();
            }
        }
        (before, after) => changes.push(FieldChange {
            path: path.clone(),
            before: Some(before.clone()),
            after: Some(after.clone()),
        }),
    }
}

/// Every field of an added declaration, as a path and a value, the same way
/// [`compare`] divides values.
fn leaves(path: &mut Vec<Segment>, value: &Value, fields: &mut Vec<(Vec<Segment>, Value)>) {
    match value {
        Value::Object(entries) if !entries.is_empty() => {
            for (key, entry) in entries {
                path.push(Segment::Field(key.clone()));
                leaves(path, entry, fields);
                path.pop();
            }
        }
        Value::List(items) if items.iter().any(Value::is_structured) => {
            for (index, item) in items.iter().enumerate() {
                path.push(Segment::Index(index));
                leaves(path, item, fields);
                path.pop();
            }
        }
        leaf => fields.push((path.clone(), leaf.clone())),
    }
}
