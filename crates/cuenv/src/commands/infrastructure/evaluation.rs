//! Evaluation of the target project and the module-wide uniqueness check.
//!
//! State is keyed by the CUE module path and the project name, so two
//! projects with the same name anywhere in one module would share state and
//! delete each other's resources. Commands that change or plan changes check
//! every instance of every CUE package in the module first, and refuse to run
//! when any instance cannot be evaluated (it could hide a project with the
//! same name) or when the module-wide evaluation does not include the target
//! itself (the CUE loader skips some directories, and a project there would
//! escape the check). The state store's owner record is the second fence:
//! it names the one instance allowed to act on the state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cuengine::{
    InstanceFailures, ModuleEvalOptions, PackageScope, SkippedDirectories, SkippedDirectory,
    SkippedReason,
};
use cuenv_core::ModuleEvaluation;
use cuenv_core::cue::discovery::compute_relative_path;
use cuenv_core::manifest::Project;
use cuenv_infrastructure::{ProjectInstance, TenantKey};
use cuenv_manifest::environment::Env;
use cuenv_manifest::manifest::Infrastructure;

use crate::cli::CliError;
use crate::commands::module_evaluation::{PathEvaluation, evaluate_path};

/// Whether the command checks that the project name is unique in the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameCheck {
    /// Evaluate every instance of every package in the module and refuse a
    /// duplicate name, an instance that does not evaluate, or a target the
    /// module-wide evaluation leaves out (`plan`, `apply`, `destroy`,
    /// `state adopt`).
    WholeModule,
    /// Evaluate only the target (`state list`, `state remove`,
    /// `state recover`, `unlock`), so a broken sibling never blocks
    /// inspecting, repairing or unlocking state.
    TargetOnly,
}

/// What to evaluate.
#[derive(Debug, Clone, Copy)]
pub(super) struct TargetRequest<'request> {
    /// Directory of the project.
    pub(super) path: &'request str,
    /// CUE package of the project.
    pub(super) package: &'request str,
    /// Whether to check the project name across the module.
    pub(super) name_check: NameCheck,
    /// Selected named infrastructure environment, if any.
    pub(super) environment: Option<&'request str>,
}

/// Evaluated inputs for one run.
#[derive(Debug)]
pub(super) struct Target {
    /// Module path and project name.
    pub(super) tenant: TenantKey,
    /// The CUE instance (directory and package) the project comes from.
    pub(super) instance: ProjectInstance,
    /// The project's `infrastructure` block.
    pub(super) infrastructure: Infrastructure,
    /// Named environment selected by the global CLI flag.
    pub(super) environment: Option<String>,
    /// Project environment variables, including only the selected overlay.
    pub(super) env: Option<Env>,
    /// Canonical project directory.
    pub(super) project_directory: PathBuf,
}

/// Evaluate the target project and, when asked, check its name is unique.
///
/// # Errors
///
/// Returns an error when the path does not resolve, evaluation fails, the
/// project has no `infrastructure` block, another instance in the module uses
/// the same project name, an instance in the module cannot be evaluated, or
/// the module-wide evaluation does not include the target.
pub(super) fn evaluate(request: TargetRequest<'_>) -> Result<Target, CliError> {
    let target_path = Path::new(request.path).canonicalize().map_err(|error| {
        CliError::config(format!("cannot resolve path {}: {error}", request.path))
    })?;
    let selected_path = request
        .environment
        .map(|name| {
            serde_json::to_string(name)
                .map(|name| format!("infrastructure.environments.{name}"))
                .map_err(|error| {
                    CliError::config(format!("cannot encode infrastructure environment: {error}"))
                })
        })
        .transpose()?;
    let mut concrete_paths = vec!["infrastructure.state".to_string()];
    if let Some(path) = &selected_path {
        concrete_paths.push(path.clone());
    }
    // Export the infrastructure object leniently so its complete set of
    // top-level keys reaches the DTO's unknown-field check. Only selected
    // paths are required concrete; unselected named values are removed before
    // deserialization.
    let export_paths = vec![
        "name".to_string(),
        "env".to_string(),
        "infrastructure".to_string(),
    ];
    let mut module = evaluate_path(PathEvaluation {
        target_path: &target_path,
        package: request.package,
        concrete_paths: concrete_paths.clone(),
        export_paths: export_paths.clone(),
    })
    .map_err(|error| {
        explain_concrete_failure(
            &target_path,
            request.package,
            request.environment,
            export_paths,
            error,
        )
    })?;
    if request.environment.is_none() {
        let relative_path = compute_relative_path(&target_path, &module.root);
        if let Some(value) = module
            .get(Path::new(&relative_path))
            .map(|instance| &instance.value)
        {
            for field in ["providers", "resources"] {
                if value.pointer(&format!("/infrastructure/{field}")).is_some() {
                    concrete_paths.push(format!("infrastructure.{field}"));
                }
            }
        }
        if concrete_paths.len() > 1 {
            let legacy_export_paths = vec![
                "name".to_string(),
                "env".to_string(),
                "infrastructure".to_string(),
            ];
            module = evaluate_path(PathEvaluation {
                target_path: &target_path,
                package: request.package,
                concrete_paths,
                export_paths: legacy_export_paths.clone(),
            })
            .map_err(|error| {
                explain_concrete_failure(
                    &target_path,
                    request.package,
                    request.environment,
                    legacy_export_paths,
                    error,
                )
            })?;
        }
    }
    let relative_path = compute_relative_path(&target_path, &module.root);
    let (project, infrastructure) = target_project(&module, &target_path, request.environment)?;
    let infrastructure = infrastructure.ok_or_else(|| missing_infrastructure(&project.name))?;
    let infrastructure = if let Some(name) = request.environment {
        infrastructure.for_environment(name).ok_or_else(|| {
            CliError::config(format!(
                "project '{}' has no infrastructure environment named '{name}'",
                project.name
            ))
        })?
    } else {
        infrastructure
    };
    let instance = ProjectInstance::new(&relative_path, request.package)
        .map_err(|error| super::failure(&error))?;

    if request.name_check == NameCheck::WholeModule {
        check_unique_name(&UniquenessCheck {
            module_root: &module.root,
            project_name: &project.name,
            target: &instance,
        })?;
    }

    let module_path = cuenv_infrastructure::read_module_path(&module.root)
        .map_err(|error| super::failure(&error))?;
    let tenant = if let Some(name) = request.environment {
        TenantKey::with_environment(module_path, project.name, name)
    } else {
        TenantKey::new(module_path, project.name)
    }
    .map_err(|error| super::failure(&error))?;
    Ok(Target {
        tenant,
        instance,
        infrastructure,
        environment: request.environment.map(str::to_owned),
        env: project.env,
        project_directory: target_path,
    })
}

fn target_project(
    module: &ModuleEvaluation,
    target_path: &Path,
    environment: Option<&str>,
) -> Result<(Project, Option<Infrastructure>), CliError> {
    let relative_path = compute_relative_path(target_path, &module.root);
    let instance = module.get(Path::new(&relative_path)).ok_or_else(|| {
        CliError::config(format!(
            "No CUE instance found at path: {}",
            target_path.display()
        ))
    })?;
    let mut selected = instance.clone();
    // The CLI resolves only the selected overlay. Other overlays may remain
    // incomplete CUE values and must not enter Env deserialization.
    if let Some(overlays) = selected.value.pointer_mut("/env/environment")
        && let Some(overlays) = overlays.as_object_mut()
    {
        overlays.retain(|name, _| environment == Some(name.as_str()));
    }
    if let Some(environments) = selected.value.pointer_mut("/infrastructure/environments")
        && let Some(environments) = environments.as_object_mut()
    {
        environments.retain(|name, _| environment == Some(name.as_str()));
    }
    if environment.is_some()
        && let Some(infrastructure) = selected.value.get_mut("infrastructure")
        && let Some(infrastructure) = infrastructure.as_object_mut()
    {
        infrastructure.remove("providers");
        infrastructure.remove("resources");
    }
    let project = selected.deserialize().map_err(CliError::from)?;
    // Ordinary Project decoding keeps infrastructure as raw JSON. Only this
    // command consumes it, after selecting the concrete configuration above.
    let infrastructure = selected
        .value
        .get("infrastructure")
        .cloned()
        .map(|value| {
            selected.value = value;
            selected
                .deserialize::<Infrastructure>()
                .map_err(CliError::from)
        })
        .transpose()?;
    Ok((project, infrastructure))
}

fn missing_infrastructure(project_name: &str) -> CliError {
    CliError::config(format!(
        "project '{project_name}' has no `infrastructure` block"
    ))
}

/// A failed evaluation with the concrete path may only mean the project has
/// no `infrastructure` block (the bridge fails a missing concrete path).
/// Tell that case apart by evaluating once more without the requirement;
/// this runs only on the failure path.
fn explain_concrete_failure(
    target_path: &Path,
    package: &str,
    environment: Option<&str>,
    export_paths: Vec<String>,
    error: cuenv_core::Error,
) -> CliError {
    let lenient = evaluate_path(PathEvaluation {
        target_path,
        package,
        concrete_paths: Vec::new(),
        export_paths,
    });
    if let Ok(module) = lenient {
        let relative_path = compute_relative_path(target_path, &module.root);
        if let Some(instance) = module.get(Path::new(&relative_path)) {
            let project_name = instance
                .value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            if instance.value.get("infrastructure").is_none() {
                return missing_infrastructure(project_name);
            }
            if let Some(name) = environment
                && instance
                    .value
                    .pointer("/infrastructure/environments")
                    .and_then(serde_json::Value::as_object)
                    .is_none_or(|environments| !environments.contains_key(name))
            {
                return CliError::config(format!(
                    "project '{project_name}' has no infrastructure environment named '{name}'"
                ));
            }
        }
    }
    CliError::from(error)
}

/// The only fields of each instance the uniqueness check reads.
const NAME_PATH: &str = "name";
const INFRASTRUCTURE_PATH: &str = "infrastructure";

/// What the uniqueness check needs to know about one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceSummary {
    /// The instance's `name`, when it is a string.
    name: Option<String>,
    /// Whether it has an `infrastructure` block (a regular field, not only
    /// an optional declaration).
    has_infrastructure: bool,
}

impl InstanceSummary {
    /// Whether the instance would share the target's state: it declares
    /// the same project name and an `infrastructure` block. A child
    /// directory inherits its ancestors' fields in CUE, so a child without
    /// its own `name` shares its parent's; that only matters for state when
    /// the `infrastructure` block is present too, and then it is a real
    /// conflict.
    fn shares_state_with(&self, project_name: &str) -> bool {
        self.has_infrastructure && self.name.as_deref() == Some(project_name)
    }
}

/// Every instance of every package in the module, keyed by its normalized
/// `<directory>:<package>` identity, and the directories the evaluation
/// left out.
#[derive(Debug, Default)]
struct ModuleInstances {
    instances: BTreeMap<String, InstanceSummary>,
    skipped: Vec<SkippedDirectory>,
}

/// Normalize a `<directory>:<package>` key of an all-packages evaluation
/// to the form [`ProjectInstance`] uses. Package names are identifiers, so
/// the key splits at its last colon.
fn normalized_key(key: &str) -> String {
    key.rsplit_once(':')
        .and_then(|(directory, package)| ProjectInstance::new(directory, package).ok())
        .map_or_else(|| key.to_string(), |instance| instance.as_str().to_string())
}

/// Evaluate every instance of every package, recursively, exporting only
/// `name` and whether `infrastructure` exists. Any instance that fails to
/// load, build or export fails the whole call, so nothing that could
/// declare the same project is silently left out; the directories the
/// loader does not visit are reported. No concrete paths: other instances
/// only need to evaluate, not to be complete infrastructure.
fn module_instances(module_root: &Path) -> Result<ModuleInstances, cuengine::CueEngineError> {
    let options = ModuleEvalOptions {
        recursive: true,
        package_scope: PackageScope::All,
        instance_failures: InstanceFailures::Fail,
        export_paths: vec![NAME_PATH.to_string()],
        presence_paths: vec![INFRASTRUCTURE_PATH.to_string()],
        skipped_directories: SkippedDirectories::Report,
        ..Default::default()
    };
    let evaluation = cuengine::evaluate_module(module_root, "", Some(&options))?;
    let instances = evaluation
        .instances
        .iter()
        .map(|(key, value)| {
            let has_infrastructure = evaluation
                .present
                .get(key)
                .is_some_and(|present| present.iter().any(|path| path == INFRASTRUCTURE_PATH));
            let summary = InstanceSummary {
                name: value
                    .get(NAME_PATH)
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                has_infrastructure,
            };
            (normalized_key(key), summary)
        })
        .collect();
    Ok(ModuleInstances {
        instances,
        skipped: evaluation.skipped_directories,
    })
}

/// Inputs for [`check_unique_name`].
struct UniquenessCheck<'check> {
    module_root: &'check Path,
    project_name: &'check str,
    target: &'check ProjectInstance,
}

/// The directory part of an instance identity.
fn instance_directory(instance: &ProjectInstance) -> &str {
    instance
        .as_str()
        .rsplit_once(':')
        .map_or(".", |(directory, _)| directory)
}

/// Why the loader left out `skipped`, and what to do about it.
fn skipped_help(skipped: &SkippedDirectory) -> String {
    let path = &skipped.path;
    let why = match skipped.reason {
        SkippedReason::Dot => format!("its directory '{path}' starts with '.'"),
        SkippedReason::Underscore => format!("its directory '{path}' starts with '_'"),
        SkippedReason::Testdata => format!("it is inside '{path}', a 'testdata' directory"),
        SkippedReason::NestedModule => {
            format!("it is inside '{path}', which holds its own cue.mod (another CUE module)")
        }
        SkippedReason::Unreadable => format!("its directory '{path}' cannot be read"),
    };
    format!(
        "The CUE loader leaves this project out when it loads every package of the module: \
         {why}. A project the loader leaves out cannot be checked for a duplicate name, so it \
         cannot manage infrastructure. Move it to a directory whose path has no component \
         starting with '.' or '_', no 'testdata' and no nested CUE module{}.",
        if skipped.reason == SkippedReason::Unreadable {
            ", or make the directory readable"
        } else {
            ""
        }
    )
}

/// The skipped directory that holds the target, if any.
fn skipped_target<'module>(
    module: &'module ModuleInstances,
    target: &ProjectInstance,
) -> Option<&'module SkippedDirectory> {
    let directory = instance_directory(target);
    module.skipped.iter().find(|skipped| {
        directory == skipped.path
            || directory
                .strip_prefix(skipped.path.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The first directory on the target's path that the loader skips by its
/// name, for when the evaluation reported none (it failed outright).
fn skipped_by_name(target: &ProjectInstance) -> Option<SkippedDirectory> {
    let directory = instance_directory(target);
    let mut path = String::new();
    for component in directory.split('/').filter(|component| *component != ".") {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(component);
        let reason = if component.starts_with('.') {
            Some(SkippedReason::Dot)
        } else if component.starts_with('_') {
            Some(SkippedReason::Underscore)
        } else if component == "testdata" {
            Some(SkippedReason::Testdata)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Some(SkippedDirectory { path, reason });
        }
    }
    None
}

fn check_unique_name(check: &UniquenessCheck<'_>) -> Result<(), CliError> {
    let module = module_instances(check.module_root).map_err(|error| {
        CliError::eval_with_help(
            format!(
                "cannot confirm that project name '{}' is unique in this CUE module: {error}",
                check.project_name
            ),
            skipped_by_name(check.target).map_or_else(
                || {
                    "Infrastructure state is keyed by module path and project name, and an \
                     instance that does not evaluate could declare a project with the same \
                     name. Fix or remove the instances listed above; `state list` and \
                     `unlock` still work meanwhile."
                        .to_string()
                },
                |skipped| skipped_help(&skipped),
            ),
        )
    })?;

    // Fail closed when the evaluation did not include the target itself:
    // the loader skipped its directory, so it would skip a duplicate there
    // as well.
    if !module.instances.contains_key(check.target.as_str()) {
        return Err(CliError::config_with_help(
            format!(
                "cannot confirm that project name '{}' is unique in this CUE module: the \
                 module-wide evaluation does not include this project's instance {}",
                check.project_name, check.target
            ),
            skipped_target(&module, check.target)
                .cloned()
                .or_else(|| skipped_by_name(check.target))
                .map_or_else(
                    || {
                        "The module-wide evaluation loads every package with CUE's `./...` \
                         pattern and did not produce this instance. Check that --path names \
                         the project directory and --package the package that declares the \
                         project."
                            .to_string()
                    },
                    |skipped| skipped_help(&skipped),
                ),
        ));
    }

    let duplicates: Vec<&str> = module
        .instances
        .iter()
        .filter(|(key, summary)| {
            key.as_str() != check.target.as_str() && summary.shares_state_with(check.project_name)
        })
        .map(|(key, _)| key.as_str())
        .collect();
    if duplicates.is_empty() {
        return Ok(());
    }
    Err(CliError::config_with_help(
        format!(
            "project name '{}' with an `infrastructure` block is also declared by {} in this \
             CUE module; infrastructure state is keyed by module path and project name, so \
             they would share state",
            check.project_name,
            duplicates.join(", ")
        ),
        "Give each project in the module a unique `name`. A directory below a project inherits \
         the project's `name` and `infrastructure`, and cannot declare another `name` in the \
         same package (the two values conflict): put the files of that directory in a \
         different CUE package, or move them out from under the project.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_normalize_like_instances() {
        assert_eq!(normalized_key("./app:cuenv"), "app:cuenv");
        assert_eq!(normalized_key(".:cuenv"), ".:cuenv");
        assert_eq!(normalized_key("broken"), "broken");
    }

    #[test]
    fn the_skipped_directory_holding_the_target_explains_it() {
        let module = ModuleInstances {
            instances: BTreeMap::new(),
            skipped: vec![
                SkippedDirectory {
                    path: "deploy/_staging".to_string(),
                    reason: SkippedReason::Underscore,
                },
                SkippedDirectory {
                    path: "deploy/_stag".to_string(),
                    reason: SkippedReason::Underscore,
                },
            ],
        };
        let inside = ProjectInstance::new("deploy/_staging/app", "cuenv").unwrap();
        let skipped = skipped_target(&module, &inside).unwrap();
        assert_eq!(skipped.path, "deploy/_staging");
        let help = skipped_help(skipped);
        assert!(help.contains("'deploy/_staging' starts with '_'"), "{help}");

        let exact = ProjectInstance::new("deploy/_stag", "cuenv").unwrap();
        assert_eq!(
            skipped_target(&module, &exact).unwrap().path,
            "deploy/_stag"
        );

        let elsewhere = ProjectInstance::new("deploy/app", "cuenv").unwrap();
        assert!(skipped_target(&module, &elsewhere).is_none());
    }
}
