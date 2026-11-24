//! Which part of a producer's output a reference reads.

use std::fmt;

use crate::error::{InfrastructureError, Result};
use crate::type_system::{PathStep, Type, Value};

/// A top-level provider output attribute, and optionally a position inside
/// it: `id`, or `result[0].id` inside a list of objects.
///
/// Selectors below the attribute are object attributes and list or tuple
/// indexes. Map elements are not selectable, so a path never carries a map
/// key and can always be shown in diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputPath {
    attribute: String,
    selector: Vec<PathStep>,
}

impl OutputPath {
    /// A whole top-level attribute.
    #[must_use]
    pub fn attribute(name: impl Into<String>) -> Self {
        Self {
            attribute: name.into(),
            selector: Vec::new(),
        }
    }

    /// A position inside a top-level attribute.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a map key or a negative index.
    pub fn nested(name: impl Into<String>, selector: Vec<PathStep>) -> Result<Self> {
        let attribute = name.into();
        if selector.iter().any(|step| {
            matches!(step, PathStep::Key(_)) || matches!(step, PathStep::Index(index) if *index < 0)
        }) {
            return Err(InfrastructureError::configuration(format!(
                "output {attribute}: a selector may only name object attributes and list indexes"
            )));
        }
        Ok(Self {
            attribute,
            selector,
        })
    }

    /// The top-level provider attribute holding the output.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.attribute
    }

    /// Steps below the top-level attribute; empty for the attribute itself.
    #[must_use]
    pub fn selector(&self) -> &[PathStep] {
        &self.selector
    }

    /// Whether this is a whole top-level attribute.
    #[must_use]
    pub fn is_attribute(&self) -> bool {
        self.selector.is_empty()
    }

    /// The type at this position of an attribute of `attribute_type`.
    #[must_use]
    pub fn value_type(&self, attribute_type: &Type) -> Option<Type> {
        attribute_type.at_path(&self.selector)
    }

    /// Read this output from a producer's whole value.
    ///
    /// `Ok(None)` means the top-level attribute is absent or null, as for a
    /// whole attribute. An unknown attribute or ancestor reads as unknown.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the attribute is known but the
    /// selected position does not exist in it, for example an index past the
    /// end of a list, so a lookup that matched nothing fails instead of
    /// waiting for a value that never comes. The error names the path, never
    /// a value.
    pub fn read<'value>(&self, producer: &'value Value) -> Result<Option<&'value Value>> {
        let Some(attribute) = producer.attribute(&self.attribute) else {
            return Ok(None);
        };
        if self.selector.is_empty() || matches!(attribute.untyped(), Value::Null) {
            return Ok(Some(attribute));
        }
        crate::type_system::value_at_path(attribute, &self.selector)
            .map(Some)
            .ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "output {self} does not exist: the provider returned fewer list elements \
                     than the selector needs, or a null on the way"
                ))
            })
    }
}

impl From<&str> for OutputPath {
    fn from(name: &str) -> Self {
        Self::attribute(name)
    }
}

impl From<String> for OutputPath {
    fn from(name: String) -> Self {
        Self::attribute(name)
    }
}

impl fmt::Display for OutputPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_label(formatter, &self.attribute)?;
        for step in &self.selector {
            match step {
                PathStep::Attribute(name) | PathStep::Key(name) => {
                    formatter.write_str(".")?;
                    write_label(formatter, name)?;
                }
                PathStep::Index(index) => write!(formatter, "[{index}]")?,
            }
        }
        Ok(())
    }
}

fn write_label(formatter: &mut fmt::Formatter<'_>, name: &str) -> fmt::Result {
    if super::is_identifier(name) {
        formatter.write_str(name)
    } else {
        formatter.write_str(&serde_json::to_string(name).map_err(|_| fmt::Error)?)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn permission_groups(ids: &[&str]) -> Value {
        Value::Object(BTreeMap::from([(
            "result".to_string(),
            Value::List(
                ids.iter()
                    .map(|id| {
                        Value::Object(BTreeMap::from([(
                            "id".to_string(),
                            Value::String((*id).to_string()),
                        )]))
                    })
                    .collect(),
            ),
        )]))
    }

    fn first_id() -> OutputPath {
        OutputPath::nested(
            "result",
            vec![PathStep::Index(0), PathStep::Attribute("id".into())],
        )
        .unwrap()
    }

    #[test]
    fn a_selector_reads_an_element_of_a_list_output() {
        let producer = permission_groups(&["one", "two"]);
        assert_eq!(
            first_id().read(&producer).unwrap(),
            Some(&Value::String("one".into()))
        );
        assert_eq!(first_id().to_string(), "result[0].id");
    }

    #[test]
    fn a_selector_past_the_end_of_a_known_list_fails() {
        let error = first_id()
            .read(&permission_groups(&[]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("output result[0].id does not exist"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_list_reads_as_unknown() {
        let producer = Value::Object(BTreeMap::from([("result".to_string(), Value::Unknown)]));
        assert_eq!(first_id().read(&producer).unwrap(), Some(&Value::Unknown));
    }

    #[test]
    fn a_missing_attribute_reads_as_absent() {
        let producer = Value::Object(BTreeMap::new());
        assert_eq!(first_id().read(&producer).unwrap(), None);
    }

    #[test]
    fn a_selector_cannot_name_a_map_key() {
        assert!(OutputPath::nested("tags", vec![PathStep::Key("team".into())]).is_err());
        assert!(OutputPath::nested("items", vec![PathStep::Index(-1)]).is_err());
    }
}
