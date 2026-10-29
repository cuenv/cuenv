//! Lower selected native CUE output selectors into the established descriptors.

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

    pub(super) fn evaluation(&self, project: &Value) -> Result<ReferenceEvaluation, CliError> {
        let root = self.root();
        let mut options = self.scanning();
        for namespace in ["resources", "data"] {
            if let Some(declarations) = project
                .pointer(&pointer(&root))
                .and_then(|configuration| configuration.get(namespace))
                .and_then(Value::as_object)
            {
                for (name, declaration) in declarations {
                    if declaration.get("outputs").is_some() {
                        let mut outputs = root.clone();
                        outputs.extend([
                            namespace.to_string(),
                            name.clone(),
                            "outputs".to_string(),
                        ]);
                        options.abstract_paths.push(quoted_path(&outputs));
                    }
                }
            }
        }
        for reference in self.selected_references()? {
            options
                .deferred
                .insert(reference.path.clone(), reference.target.clone());
        }
        Ok(options)
    }

    fn selected_references(&self) -> Result<Vec<&DirectReference>, CliError> {
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
                || destination.len() != root.len() + 4
                || destination[..root.len()] != root
                || !is_declaration_namespace(&destination[root.len()])
                || destination[root.len() + 2] != "configuration"
            {
                // Declarations may refer to a type definition, but may not
                // alias another output: every namespace field declares one
                // real provider attribute.
                return Err(CliError::config(format!(
                    "{}: output selectors require a whole top-level resource or data source argument and one top-level scalar output in the selected infrastructure configuration",
                    reference.path
                )));
            }
            if !reference.supported || output_type(&reference.primitive_type).is_none() {
                return Err(CliError::config(format!(
                    "{}: output selectors support direct references and primitive type conjunctions; output declarations must be bare string, number or bool types without defaults or refinements",
                    reference.path
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
            let source_type = source
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| CliError::config("output selector source type is not concrete"))?;
            let source_field = if namespace == "data" {
                "fromDataSource"
            } else {
                "fromResource"
            };
            let descriptor = serde_json::json!({
                (source_field): format!("{source_type}.{producer}"),
                "attribute": attribute,
            });
            let destination = project
                .pointer_mut(&pointer(&reference.labels))
                .ok_or_else(|| CliError::config("output selector destination is missing"))?;
            *destination = descriptor;
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
