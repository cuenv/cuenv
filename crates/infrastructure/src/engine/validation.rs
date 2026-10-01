//! Semantic checks of an `infrastructure` configuration, with the path of
//! every problem found.

use std::collections::BTreeSet;

use cuenv_manifest::manifest::Infrastructure;

use crate::error::{InfrastructureError, Result};
use crate::registry::{ProviderSource, validate_version};

use super::{dependency_graph, topological_order};

/// Where in the `infrastructure` block a configuration lives: the top level,
/// or one named environment, which does not inherit anything from the top
/// level.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConfigurationScope<'name> {
    environment: Option<&'name str>,
}

impl<'name> ConfigurationScope<'name> {
    pub(super) const fn new(environment: Option<&'name str>) -> Self {
        Self { environment }
    }

    /// The path of the provider declarations.
    pub(super) fn providers(&self) -> String {
        self.environment.map_or_else(
            || "infrastructure.providers".to_string(),
            |name| format!("infrastructure.environments.{name}.providers"),
        )
    }

    /// The path of the resource declarations.
    pub(super) fn resources(&self) -> String {
        self.environment.map_or_else(
            || "infrastructure.resources".to_string(),
            |name| format!("infrastructure.environments.{name}.resources"),
        )
    }

    /// A reminder, for the messages that need one, that an environment's
    /// providers are its own.
    pub(super) fn inheritance_note(&self) -> String {
        if self.environment.is_some() {
            format!(
                "; top-level `infrastructure.providers` are not inherited by environments, so \
                 declare the provider in {}",
                self.providers()
            )
        } else {
            String::new()
        }
    }
}

/// Validate every provider and resource declaration in the selected
/// configuration.
///
/// The selected concrete configuration is checked here as well as by the CUE
/// schema, so a configuration that never met the schema (or met an older
/// one) still fails before anything is launched. Every problem found is
/// reported, each with the full path of the field at fault, for example
/// `infrastructure.environments.dev.resources.pet.dependsOn[0]`. Call this
/// before resolving provider secrets or opening a state backend. The engine
/// repeats the validation as defense in depth before planning.
///
/// `environment` is the environment the configuration was selected from, if
/// any, and only changes the paths in the messages.
///
/// # Errors
///
/// Returns an error for invalid provider sources, versions or paths, resource
/// references to undeclared providers, unknown dependencies, or dependency
/// cycles.
pub fn validate_configuration(
    infrastructure: &Infrastructure,
    environment: Option<&str>,
) -> Result<()> {
    let scope = ConfigurationScope::new(environment);
    let mut problems = Vec::new();
    check_providers(infrastructure, &scope, &mut problems);
    check_resources(infrastructure, &scope, &mut problems);
    match problems.len() {
        0 => Ok(()),
        1 => Err(InfrastructureError::configuration(problems.remove(0))),
        count => Err(InfrastructureError::configuration(format!(
            "{count} problems:\n  - {}",
            problems.join("\n  - ")
        ))),
    }
}

fn check_providers(
    infrastructure: &Infrastructure,
    scope: &ConfigurationScope<'_>,
    problems: &mut Vec<String>,
) {
    let providers = scope.providers();
    for (name, declaration) in &infrastructure.providers {
        let path = format!("{providers}.{name}");
        if let Err(error) = ProviderSource::parse(&declaration.source) {
            problems.push(format!("{path}.source: {}", detail(error)));
        }
        let local_path = declaration.path.as_deref();
        if local_path == Some("") {
            problems.push(format!(
                "{path}.path: must not be empty; set a path to the provider binary or remove it \
                 and set `version`"
            ));
            continue;
        }
        match (declaration.version.as_deref(), local_path) {
            (Some(_), Some(_)) => problems.push(format!(
                "{path}: provider '{name}' sets both `path` and `version`; set exactly one"
            )),
            (None, None) => problems.push(format!(
                "{path}: provider '{name}' needs an exact `version` (or a local `path`); set \
                 exactly one"
            )),
            (Some(version), None) => {
                if let Err(error) = validate_version(version) {
                    problems.push(format!("{path}.version: {}", detail(error)));
                }
            }
            (None, Some(_)) => {}
        }
    }
}

fn check_resources(
    infrastructure: &Infrastructure,
    scope: &ConfigurationScope<'_>,
    problems: &mut Vec<String>,
) {
    let resources = scope.resources();
    let providers = scope.providers();
    let declared: BTreeSet<&String> = infrastructure.resources.keys().collect();
    let mut unknown_dependency = false;
    for (name, declaration) in &infrastructure.resources {
        let path = format!("{resources}.{name}");
        let provider_name = declaration.provider_name();
        if !infrastructure.providers.contains_key(provider_name) {
            let note = scope.inheritance_note();
            if declaration.provider.is_some() {
                problems.push(format!(
                    "{path}.provider: no provider named '{provider_name}' in {providers}; \
                     declare it there or change `provider`{note}"
                ));
            } else {
                problems.push(format!(
                    "{path}.type: no provider named '{provider_name}' (the prefix of type '{}') \
                     in {providers}; declare it there or set `provider` on the resource{note}",
                    declaration.resource_type
                ));
            }
        }
        for (index, dependency) in declaration.depends_on.iter().enumerate() {
            if !declared.contains(dependency) {
                unknown_dependency = true;
                problems.push(format!(
                    "{path}.dependsOn[{index}]: resource '{name}' depends on unknown resource \
                     '{dependency}'; declare it in {resources}"
                ));
            }
        }
    }
    if !unknown_dependency
        && let Err(error) = topological_order(&dependency_graph(&infrastructure.resources))
    {
        problems.push(format!("{resources}: {}", detail(error)));
    }
}

/// Say which field of the configuration a configuration error is about.
pub(super) fn at_path(error: InfrastructureError, path: &str) -> InfrastructureError {
    match error {
        InfrastructureError::Configuration(message) => {
            InfrastructureError::configuration(format!("{path}: {message}"))
        }
        other => other,
    }
}

/// The message of a configuration error, without its generic prefix.
fn detail(error: InfrastructureError) -> String {
    match error {
        InfrastructureError::Configuration(message) => message,
        other => other.to_string(),
    }
}
