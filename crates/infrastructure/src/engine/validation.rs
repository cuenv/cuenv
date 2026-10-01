//! Semantic checks of an `infrastructure` configuration, with the path of
//! every problem found.

use std::collections::{BTreeMap, BTreeSet};

use cuenv_manifest::manifest::{Infrastructure, ManagedResourceDeclaration};

use crate::error::{InfrastructureError, Result};
use crate::registry::{ProviderSource, validate_version};

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
/// cycles (each cycle is reported on its own, with the `dependsOn` entries
/// that form it, whether or not other dependencies are unknown).
#[tracing::instrument(skip_all, fields(environment = environment.unwrap_or("")))]
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
                problems.push(format!(
                    "{path}.dependsOn[{index}]: resource '{name}' depends on unknown resource \
                     '{dependency}'; declare it in {resources}"
                ));
            }
        }
    }
    check_cycles(&infrastructure.resources, &resources, problems);
}

/// Report every dependency cycle among the declared resources: a resource
/// that depends on itself, and each group of resources that depend on each
/// other, with only its own members and the `dependsOn` entries that form it.
/// Dependencies on resources that are not declared are reported elsewhere
/// and ignored here.
fn check_cycles(
    resources: &BTreeMap<String, ManagedResourceDeclaration>,
    path: &str,
    problems: &mut Vec<String>,
) {
    let names: Vec<&str> = resources.keys().map(String::as_str).collect();
    let position: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, index))
        .collect();
    let mut graph: Vec<Vec<usize>> = vec![Vec::new(); names.len()];
    for (name, declaration) in resources {
        let from = position[name.as_str()];
        for (index, dependency) in declaration.depends_on.iter().enumerate() {
            let Some(&to) = position.get(dependency.as_str()) else {
                continue;
            };
            if to == from {
                problems.push(format!(
                    "{path}.{name}.dependsOn[{index}]: resource '{name}' depends on itself"
                ));
            } else if !graph[from].contains(&to) {
                graph[from].push(to);
            }
        }
    }
    for mut component in strongly_connected_components(&graph) {
        if component.len() < 2 {
            continue;
        }
        component.sort_unstable();
        let members: Vec<&str> = component.iter().map(|node| names[*node]).collect();
        let entries: Vec<String> = members
            .iter()
            .flat_map(|name| {
                resources[*name]
                    .depends_on
                    .iter()
                    .enumerate()
                    .filter(|(_, dependency)| {
                        dependency.as_str() != *name && members.contains(&dependency.as_str())
                    })
                    .map(move |(index, dependency)| {
                        format!("{path}.{name}.dependsOn[{index}] -> {dependency}")
                    })
            })
            .collect();
        problems.push(format!(
            "{path}: dependency cycle between resources {}: {}; remove one of these dependsOn \
             entries",
            members.join(", "),
            entries.join(", ")
        ));
    }
}

/// Tarjan's algorithm, without recursion: the strongly connected components
/// of a graph given as adjacency lists.
fn strongly_connected_components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut index: Vec<Option<usize>> = vec![None; graph.len()];
    let mut low = vec![0; graph.len()];
    let mut on_stack = vec![false; graph.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0;
    let mut components = Vec::new();
    for root in 0..graph.len() {
        if index[root].is_some() {
            continue;
        }
        index[root] = Some(counter);
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root] = true;
        // Each entry is a node and how many of its dependencies are done.
        let mut work = vec![(root, 0_usize)];
        while let Some(&(node, done)) = work.last() {
            if let Some(&next) = graph[node].get(done) {
                if let Some(last) = work.last_mut() {
                    last.1 += 1;
                }
                match index[next] {
                    None => {
                        index[next] = Some(counter);
                        low[next] = counter;
                        counter += 1;
                        stack.push(next);
                        on_stack[next] = true;
                        work.push((next, 0));
                    }
                    Some(order) if on_stack[next] => low[node] = low[node].min(order),
                    Some(_) => {}
                }
                continue;
            }
            work.pop();
            if index[node] == Some(low[node]) {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }
    components
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
