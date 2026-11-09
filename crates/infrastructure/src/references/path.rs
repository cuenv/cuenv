//! Structured locations within provider configuration, independent of CUE syntax.

use std::fmt;

use serde_json::Value as Json;

use crate::error::{InfrastructureError, Result};
use crate::schema::Block;
use crate::type_system::{PathStep, Value};

/// A nonempty configuration path rooted at an object attribute.
///
/// Attribute names and map keys are distinct steps, including when their text
/// contains dots, quotes or digits. Display formatting is for diagnostics only:
/// it hides every map key before secret registration can occur. Static schema
/// fields and collection indexes remain visible. Callers must retain the steps
/// when comparing paths or computing digests.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestinationPath(Vec<PathStep>);

impl DestinationPath {
    /// Classify native CUE field selectors using the provider schema.
    ///
    /// # Errors
    /// Returns a configuration error for an unsupported or unsettable path.
    pub fn from_selectors(
        block: &Block,
        selectors: &[super::ConfigurationSelector],
    ) -> Result<Self> {
        super::ReferenceDestination::classify(block, selectors)
    }

    /// The top-level attribute containing this destination.
    #[must_use]
    pub fn root_attribute(&self) -> &str {
        match self.0.first() {
            Some(PathStep::Attribute(name)) => name,
            _ => "",
        }
    }

    /// A path selecting one top-level configuration attribute.
    #[must_use]
    pub fn attribute(name: impl Into<String>) -> Self {
        Self(vec![PathStep::Attribute(name.into())])
    }

    /// The structured path, with no negative list or tuple indexes.
    #[must_use]
    pub fn steps(&self) -> &[PathStep] {
        &self.0
    }

    /// Look up this position in evaluated configuration JSON.
    #[must_use]
    pub fn json_value<'value>(&self, root: &'value Json) -> Option<&'value Json> {
        self.0.iter().try_fold(root, |value, step| match step {
            PathStep::Attribute(name) | PathStep::Key(name) => value.as_object()?.get(name),
            PathStep::Index(index) => value.as_array()?.get(usize::try_from(*index).ok()?),
        })
    }

    /// Look up a decoded position. An unknown ancestor returns that unknown;
    /// a null or missing ancestor returns `None`.
    #[must_use]
    pub fn value<'value>(&self, root: &'value Value) -> Option<&'value Value> {
        crate::type_system::value_at_path(root, &self.0)
    }

    /// Replace an existing JSON position without creating ancestors or keys.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the position or an ancestor is missing,
    /// null, or a different kind of collection. Values never appear in errors.
    pub fn replace_json(&self, root: &mut Json, replacement: Json) -> Result<()> {
        let mut position = root;
        for step in &self.0 {
            position = match step {
                PathStep::Attribute(name) | PathStep::Key(name) => position
                    .as_object_mut()
                    .and_then(|object| object.get_mut(name)),
                PathStep::Index(index) => usize::try_from(*index)
                    .ok()
                    .and_then(|index| position.as_array_mut()?.get_mut(index)),
            }
            .ok_or_else(|| self.problem("reference destination is missing from configuration"))?;
        }
        *position = replacement;
        Ok(())
    }

    /// Replace an existing decoded position, preserving typed ancestors.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when an ancestor is missing, unknown,
    /// null, or a different kind of collection. No ancestors are fabricated.
    /// The caller must validate the replacement against the destination schema.
    pub fn replace_value(&self, root: &mut Value, replacement: Value) -> Result<()> {
        let mut position = root;
        for step in &self.0 {
            while let Value::Typed(typed) = position {
                position = &mut typed.value;
            }
            position = match (position, step) {
                (Value::Object(object), PathStep::Attribute(name) | PathStep::Key(name)) => {
                    object.get_mut(name)
                }
                (Value::List(elements), PathStep::Index(index)) => usize::try_from(*index)
                    .ok()
                    .and_then(|index| elements.get_mut(index)),
                _ => None,
            }
            .ok_or_else(|| self.problem("reference destination is missing or unresolved"))?;
        }
        *position = replacement;
        Ok(())
    }

    pub(super) fn problem(&self, reason: &str) -> InfrastructureError {
        InfrastructureError::configuration(format!("configuration.{self}: {reason}"))
    }
}

impl TryFrom<Vec<PathStep>> for DestinationPath {
    type Error = InfrastructureError;

    fn try_from(steps: Vec<PathStep>) -> Result<Self> {
        if !matches!(steps.first(), Some(PathStep::Attribute(_))) {
            return Err(InfrastructureError::configuration(
                "a reference destination must begin with an object attribute",
            ));
        }
        if steps
            .iter()
            .any(|step| matches!(step, PathStep::Index(index) if *index < 0))
        {
            return Err(InfrastructureError::configuration(
                "a reference destination cannot contain a negative index",
            ));
        }
        Ok(Self(steps))
    }
}

impl fmt::Display for DestinationPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, step) in self.0.iter().enumerate() {
            match step {
                PathStep::Attribute(name) => {
                    if index > 0 {
                        formatter.write_str(".")?;
                    }
                    if super::is_identifier(name) {
                        formatter.write_str(name)?;
                    } else {
                        serde_json::to_string(name)
                            .map_err(|_| fmt::Error)?
                            .fmt(formatter)?;
                    }
                }
                PathStep::Key(_) => formatter.write_str("[<hidden key>]")?,
                PathStep::Index(index) => write!(formatter, "[{index}]")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::type_system::Type;

    #[test]
    fn paths_distinguish_attributes_keys_indexes_and_literal_dots() -> Result<()> {
        let literal = DestinationPath::attribute("a.b");
        let nested = DestinationPath::try_from(vec![
            PathStep::Attribute("a".into()),
            PathStep::Attribute("b".into()),
        ])?;
        let keyed = DestinationPath::try_from(vec![
            PathStep::Attribute("a".into()),
            PathStep::Key("b".into()),
        ])?;
        assert_ne!(literal, nested);
        assert_ne!(nested, keyed);
        assert_eq!(literal.to_string(), "\"a.b\"");
        assert_eq!(nested.to_string(), "a.b");
        assert_eq!(keyed.to_string(), "a[<hidden key>]");
        let indexed =
            DestinationPath::try_from(vec![PathStep::Attribute("a".into()), PathStep::Index(0)])?;
        let numeric_key = DestinationPath::try_from(vec![
            PathStep::Attribute("a".into()),
            PathStep::Key("0".into()),
        ])?;
        assert_ne!(indexed, numeric_key);
        assert_eq!(indexed.to_string(), "a[0]");
        assert_eq!(numeric_key.to_string(), "a[<hidden key>]");

        let special = DestinationPath::try_from(vec![
            PathStep::Attribute("nested".into()),
            PathStep::Key("quote\"\n\\[]".into()),
            PathStep::Index(2),
            PathStep::Attribute(String::new()),
        ])?;
        assert_eq!(special.to_string(), "nested[<hidden key>][2].\"\"");
        assert_eq!(special.steps().len(), 4);
        Ok(())
    }

    #[test]
    fn diagnostics_hide_unregistered_map_keys_and_preserve_path_identity() -> Result<()> {
        let secret_key = format!("unregistered-map-key-{}", uuid::Uuid::new_v4());
        let path = DestinationPath::try_from(vec![
            PathStep::Attribute("config".into()),
            PathStep::Key(secret_key.clone()),
            PathStep::Index(2),
            PathStep::Attribute("token".into()),
            PathStep::Key("x".into()),
        ])?;
        let rendered = "config[<hidden key>][2].token[<hidden key>]";
        assert_eq!(path.to_string(), rendered);
        let error = path.problem("missing value").to_string();
        assert!(error.contains(rendered), "{error}");
        assert!(!error.contains(&secret_key), "{error}");
        assert_eq!(path.steps()[1], PathStep::Key(secret_key));
        let mut different = path.steps().to_vec();
        different[1] = PathStep::Key("different".into());
        let different = DestinationPath::try_from(different)?;
        assert_ne!(path, different);
        assert_eq!(path.to_string(), different.to_string());
        let mut absent = json!({"config": {}});
        let error = path
            .replace_json(&mut absent, Json::Null)
            .unwrap_err()
            .to_string();
        assert!(error.contains(rendered), "{error}");
        Ok(())
    }

    #[test]
    fn paths_require_an_attribute_root_and_nonnegative_indexes() {
        for steps in [
            vec![],
            vec![PathStep::Index(0)],
            vec![PathStep::Key("argument".into())],
            vec![PathStep::Attribute("argument".into()), PathStep::Index(-1)],
        ] {
            assert!(DestinationPath::try_from(steps).is_err());
        }
    }

    #[test]
    fn replacing_positions_preserves_neighboring_configuration() -> Result<()> {
        let path = DestinationPath::try_from(vec![
            PathStep::Attribute("items".into()),
            PathStep::Index(1),
            PathStep::Key("0".into()),
        ])?;
        let mut configuration = json!({"items": [{"0": "untouched"}, {"0": "replace"}]});
        assert_eq!(path.json_value(&configuration), Some(&json!("replace")));
        path.replace_json(&mut configuration, Json::Null)?;
        let mut decoded = Value::from_configuration_json(&configuration, &Type::Dynamic)?;
        path.replace_value(&mut decoded, Value::Unknown)?;
        assert_eq!(path.value(&decoded), Some(&Value::Unknown));
        assert_eq!(
            decoded.attribute("items"),
            Some(&Value::List(vec![
                Value::Object(BTreeMap::from([(
                    "0".into(),
                    Value::String("untouched".into())
                )])),
                Value::Object(BTreeMap::from([("0".into(), Value::Unknown)])),
            ]))
        );
        Ok(())
    }

    #[test]
    fn unknown_and_null_ancestors_are_not_fabricated() -> Result<()> {
        let path = DestinationPath::try_from(vec![
            PathStep::Attribute("items".into()),
            PathStep::Index(0),
        ])?;
        for ancestor in [Value::Unknown, Value::Null, Value::List(vec![])] {
            let mut root = Value::Object(BTreeMap::from([("items".into(), ancestor.clone())]));
            assert!(
                path.replace_value(&mut root, Value::String("secret".into()))
                    .is_err()
            );
            assert_eq!(root.attribute("items"), Some(&ancestor));
            assert_eq!(
                path.value(&root),
                ancestor.is_unknown().then_some(&Value::Unknown)
            );
        }
        let mut root = json!({"items": null});
        assert!(path.replace_json(&mut root, json!("secret")).is_err());
        assert_eq!(root, json!({"items": null}));
        Ok(())
    }

    #[test]
    fn replacement_preserves_typed_ancestors_and_reports_paths_without_values() -> Result<()> {
        let path = DestinationPath::try_from(vec![
            PathStep::Attribute("object".into()),
            PathStep::Attribute("argument".into()),
        ])?;
        let object_type = Type::Object(BTreeMap::from([("argument".into(), Type::String)]));
        let mut root = Value::Object(BTreeMap::from([(
            "object".into(),
            Value::typed(
                object_type.clone(),
                Value::Object(BTreeMap::from([("argument".into(), Value::Unknown)])),
            ),
        )]));
        path.replace_value(&mut root, Value::String("secret-value".into()))?;
        assert_eq!(
            path.value(&root),
            Some(&Value::String("secret-value".into()))
        );
        assert!(
            matches!(root.attribute("object"), Some(Value::Typed(typed)) if typed.value_type == object_type)
        );

        let mut invalid = json!({"object": "secret-value"});
        let error = path
            .replace_json(&mut invalid, json!("other-secret"))
            .err()
            .ok_or_else(|| path.problem("expected missing position"))?
            .to_string();
        assert!(error.contains("configuration.object.argument"));
        assert!(!error.contains("secret"));
        Ok(())
    }
}
