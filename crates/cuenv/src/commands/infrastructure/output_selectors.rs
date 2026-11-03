//! Preserve native CUE output declarations and clear their projected values.

use std::collections::{BTreeMap, HashMap};

use cuengine::ReferenceEvaluation;
use cuenv_core::module::DirectReference;
use cuenv_manifest::manifest::InfrastructureOutputType;
use serde_json::Value;

use crate::cli::CliError;

// Managed resources use their name; data sources use `data.<name>` so equal
// names in the two namespaces cannot overwrite each other's output types.
pub(super) type OutputDeclarations = BTreeMap<String, BTreeMap<String, InfrastructureOutputType>>;

pub(super) struct OutputSelectors<'selection> {
    pub(super) instance_path: &'selection str,
    pub(super) references: &'selection HashMap<String, Vec<DirectReference>>,
    pub(super) environment: Option<&'selection str>,
}

impl OutputSelectors<'_> {
    fn root(&self) -> Vec<String> {
        let mut root = vec!["infrastructure".to_string()];
        if let Some(environment) = self.environment {
            root.extend(["environments".to_string(), environment.to_string()]);
        }
        root
    }

    pub(super) fn scanning(&self) -> ReferenceEvaluation {
        ReferenceEvaluation {
            paths: ["resources", "data"]
                .into_iter()
                .map(|namespace| {
                    let mut path = self.root();
                    path.push(namespace.to_string());
                    quoted_path(&path)
                })
                .collect(),
            ..ReferenceEvaluation::default()
        }
    }

    pub(super) fn selected_references(&self) -> Result<Vec<&DirectReference>, CliError> {
        let root = self.root();
        let mut selected = Vec::new();
        for reference in self
            .references
            .get(self.instance_path)
            .into_iter()
            .flatten()
        {
            if !is_output_namespace(&reference.target) {
                continue;
            }
            let target = &reference.target;
            let destination = &reference.labels;
            if target.len() != root.len() + 4
                || target[..root.len()] != root
                || !is_declaration_namespace(&target[root.len()])
                || target[root.len() + 2] != "outputs"
                || destination.len() < root.len() + 4
                || destination[..root.len()] != root
                || !is_declaration_namespace(&destination[root.len()])
                || destination[root.len() + 2] != "configuration"
            {
                // Declarations may refer to a type definition, but may not
                // alias another output: every namespace field declares one
                // real provider attribute.
                return Err(CliError::config(format!(
                    "{}: output selectors require a resource or data source configuration argument and one top-level scalar output in the selected infrastructure configuration",
                    diagnostic_path(&reference.labels)
                )));
            }
            if !reference.supported || output_type(&reference.primitive_type).is_none() {
                return Err(CliError::config(format!(
                    "{}: output selectors support direct references and primitive type conjunctions; output declarations must be bare string, number or bool types without defaults or refinements",
                    diagnostic_path(&reference.labels)
                )));
            }
            selected.push(reference);
        }
        Ok(selected)
    }

    pub(super) fn lower(&self, project: &mut Value) -> Result<OutputDeclarations, CliError> {
        let root = self.root();
        let mut declarations = OutputDeclarations::new();
        for reference in self.selected_references()? {
            let producer = &reference.target[root.len() + 1];
            let namespace = &reference.target[root.len()];
            let attribute = &reference.target[root.len() + 3];
            let source = project
                .pointer(&pointer(&reference.target[..root.len() + 2]))
                .ok_or_else(|| CliError::config("output selector source is not declared"))?;
            source
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| CliError::config("output selector source type is not concrete"))?;
            let destination = project
                .pointer_mut(&pointer(&reference.labels))
                .ok_or_else(|| CliError::config("output selector destination is missing"))?;
            // Reference identity stays with the immutable evaluator; this
            // projected value is only a placeholder for the provider input.
            *destination = Value::Null;
            let primitive = output_type(&reference.primitive_type)
                .ok_or_else(|| CliError::config("output selector declaration is not primitive"))?;
            declarations
                .entry(if namespace == "data" {
                    format!("data.{producer}")
                } else {
                    producer.clone()
                })
                .or_default()
                .insert(attribute.clone(), primitive);
        }
        for namespace in ["resources", "data"] {
            if let Some(declarations) = project
                .pointer_mut(&pointer(&root))
                .and_then(|configuration| configuration.get_mut(namespace))
                .and_then(Value::as_object_mut)
            {
                for declaration in declarations.values_mut().filter_map(Value::as_object_mut) {
                    declaration.remove("outputs");
                }
            }
        }
        Ok(declarations)
    }
}

/// Render only known structural labels. Before loading the provider schema,
/// any user-provided label can be a sensitive map key, including an identifier
/// that happens to look like an ordinary attribute name.
pub(super) fn diagnostic_path(labels: &[String]) -> String {
    let Some(first) = labels.first().map(String::as_str) else {
        return "CUE value".to_string();
    };
    let mut parts = Vec::new();
    let mut index = 1;
    match first {
        "infrastructure" => {
            parts.push("infrastructure");
            if labels
                .get(index)
                .is_some_and(|label| label == "environments")
            {
                parts.push("environments");
                index += 1;
                if labels.get(index).is_some() {
                    parts.push("<environment>");
                    index += 1;
                }
            }
            if let Some(namespace) = labels.get(index).map(String::as_str) {
                let declaration = match namespace {
                    "providers" => Some("<provider>"),
                    "resources" => Some("<resource>"),
                    "data" => Some("<data source>"),
                    _ => None,
                };
                if let Some(declaration) = declaration {
                    parts.push(namespace);
                    index += 1;
                    if labels.get(index).is_some() {
                        parts.push(declaration);
                        index += 1;
                    }
                    if labels
                        .get(index)
                        .is_some_and(|label| label == "configuration")
                    {
                        parts.push("configuration");
                        index += 1;
                    }
                } else if matches!(
                    namespace,
                    "state" | "migrateStateTo" | "workflows" | "providerEnvironment"
                ) {
                    parts.push(namespace);
                    index += 1;
                }
            }
        }
        "name" | "env" | "cache" => parts.push(first),
        _ => return "CUE value".to_string(),
    }
    parts.extend(std::iter::repeat_n(
        "<value>",
        labels.len().saturating_sub(index),
    ));
    parts.join(".")
}

fn is_output_namespace(labels: &[String]) -> bool {
    labels
        .first()
        .is_some_and(|label| label == "infrastructure")
        && ((labels
            .get(1)
            .is_some_and(|label| is_declaration_namespace(label))
            && labels.get(3).is_some_and(|label| label == "outputs"))
            || (labels.get(1).is_some_and(|label| label == "environments")
                && labels
                    .get(3)
                    .is_some_and(|label| is_declaration_namespace(label))
                && labels.get(5).is_some_and(|label| label == "outputs")))
}

fn is_declaration_namespace(label: &str) -> bool {
    matches!(label, "resources" | "data")
}

fn output_type(primitive: &str) -> Option<InfrastructureOutputType> {
    match primitive {
        "string" => Some(InfrastructureOutputType::String),
        "number" => Some(InfrastructureOutputType::Number),
        "bool" => Some(InfrastructureOutputType::Boolean),
        _ => None,
    }
}

fn pointer(labels: &[String]) -> String {
    let mut pointer = String::new();
    for label in labels {
        pointer.push('/');
        for character in label.chars() {
            match character {
                '~' => pointer.push_str("~0"),
                '/' => pointer.push_str("~1"),
                character => pointer.push(character),
            }
        }
    }
    pointer
}

// Quoting every label produces a valid CUE input path. Canonical bridge paths
// always come from reference metadata, and are never reconstructed here.
fn quoted_path(labels: &[String]) -> String {
    labels
        .iter()
        .map(|label| Value::String(label.clone()).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_errors_never_display_user_supplied_labels() {
        for (target, supported) in [
            (
                vec![
                    "infrastructure",
                    "environments",
                    "private-environment",
                    "resources",
                    "producer",
                    "outputs",
                    "id",
                    "nested",
                ],
                true,
            ),
            (
                vec![
                    "infrastructure",
                    "environments",
                    "private-environment",
                    "resources",
                    "producer",
                    "outputs",
                    "id",
                ],
                false,
            ),
        ] {
            let labels: Vec<String> = [
                "infrastructure",
                "environments",
                "private-environment",
                "resources",
                "private-resource",
                "configuration",
                "private-attribute",
                "sensitive-map-key",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            let reference = DirectReference {
                path: "private-resource.private-attribute.sensitive-map-key".to_string(),
                labels,
                target: target.into_iter().map(str::to_owned).collect(),
                primitive_type: "string".to_string(),
                supported,
            };
            let references = HashMap::from([(".".to_string(), vec![reference])]);
            let selector = OutputSelectors {
                instance_path: ".",
                references: &references,
                environment: Some("private-environment"),
            };
            let message = selector
                .selected_references()
                .expect_err("invalid selector must fail")
                .to_string();
            assert!(
                message.contains("infrastructure.environments.<environment>.resources.<resource>.configuration.<value>.<value>"),
                "{message}"
            );
            for private in [
                "private-environment",
                "private-resource",
                "private-attribute",
                "sensitive-map-key",
            ] {
                assert!(!message.contains(private), "{message}");
            }
        }
    }
}
