//! Evaluation of the target project and the module-wide uniqueness check.
//!
//! State is keyed by the CUE module path and the project name, so two
//! projects with the same name anywhere in one module would share state and
//! delete each other's resources. Commands that change or plan changes check
//! every instance of every CUE package in the module first, and refuse to run
//! when any instance cannot be evaluated: an instance that does not evaluate
//! could hide a project with the same name.

use std::path::{Path, PathBuf};

use cuengine::{InstanceFailures, ModuleEvalOptions, PackageScope};
use cuenv_core::ModuleEvaluation;
use cuenv_core::cue::discovery::compute_relative_path;
use cuenv_core::manifest::Project;
use cuenv_infrastructure::TenantKey;
use cuenv_manifest::manifest::Infrastructure;

use crate::cli::CliError;
use crate::commands::module_evaluation::{PathEvaluation, evaluate_path};

/// The CUE path that must evaluate to a concrete value. Only this command's
/// evaluation of its own target requires it; workspace discovery for other
/// commands never does.
const CONCRETE_PATH: &str = "infrastructure";

/// Whether the command checks that the project name is unique in the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameCheck {
    /// Evaluate every instance of every package in the module and refuse a
    /// duplicate name or an instance that does not evaluate (`plan`, `apply`,
    /// `destroy`).
    WholeModule,
    /// Evaluate only the target (`state`, `unlock`), so a broken sibling
    /// never blocks inspecting or unlocking state.
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
}

/// Evaluated inputs for one run.
#[derive(Debug)]
pub(super) struct Target {
    /// Module path and project name.
    pub(super) tenant: TenantKey,
    /// The project's `infrastructure` block.
    pub(super) infrastructure: Infrastructure,
    /// Canonical project directory.
    pub(super) project_directory: PathBuf,
}

/// Evaluate the target project and, when asked, check its name is unique.
///
/// # Errors
///
/// Returns an error when the path does not resolve, evaluation fails, the
/// project has no `infrastructure` block, another instance in the module uses
/// the same project name, or an instance in the module cannot be evaluated.
pub(super) fn evaluate(request: TargetRequest<'_>) -> Result<Target, CliError> {
    let target_path = Path::new(request.path).canonicalize().map_err(|error| {
        CliError::config(format!("cannot resolve path {}: {error}", request.path))
    })?;
    let module = evaluate_path(PathEvaluation {
        target_path: &target_path,
        package: request.package,
        concrete_paths: vec![CONCRETE_PATH.to_string()],
    })
    .map_err(|error| explain_concrete_failure(&target_path, request.package, error))?;
    let relative_path = compute_relative_path(&target_path, &module.root);
    let project = target_project(&module, &target_path)?;
    let infrastructure = project
        .infrastructure
        .clone()
        .ok_or_else(|| missing_infrastructure(&project.name))?;

    if request.name_check == NameCheck::WholeModule {
        check_unique_name(&UniquenessCheck {
            module_root: &module.root,
            project_name: &project.name,
            target: InstanceIdentity {
                directory: relative_path,
                package: request.package.to_string(),
            },
        })?;
    }

    let module_path = cuenv_infrastructure::read_module_path(&module.root)
        .map_err(|error| super::failure(&error))?;
    let tenant =
        TenantKey::new(module_path, project.name).map_err(|error| super::failure(&error))?;
    Ok(Target {
        tenant,
        infrastructure,
        project_directory: target_path,
    })
}

fn target_project(module: &ModuleEvaluation, target_path: &Path) -> Result<Project, CliError> {
    let relative_path = compute_relative_path(target_path, &module.root);
    let instance = module.get(Path::new(&relative_path)).ok_or_else(|| {
        CliError::config(format!(
            "No CUE instance found at path: {}",
            target_path.display()
        ))
    })?;
    instance.deserialize().map_err(CliError::from)
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
    error: cuenv_core::Error,
) -> CliError {
    let lenient = evaluate_path(PathEvaluation {
        target_path,
        package,
        concrete_paths: Vec::new(),
    });
    match lenient.map(|module| target_project(&module, target_path)) {
        Ok(Ok(project)) if project.infrastructure.is_none() => {
            missing_infrastructure(&project.name)
        }
        _ => CliError::from(error),
    }
}

/// One CUE instance: a directory and a package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InstanceIdentity {
    /// Directory relative to the module root (`.` for the root).
    directory: String,
    /// Package name (`_` for files without a package clause).
    package: String,
}

impl InstanceIdentity {
    /// Parse a `"<directory>:<package>"` key of an all-packages evaluation.
    /// Package names are identifiers, so the key splits at its last colon.
    fn from_key(key: &str) -> Self {
        key.rsplit_once(':').map_or_else(
            || Self {
                directory: key.to_string(),
                package: String::new(),
            },
            |(directory, package)| Self {
                directory: directory.to_string(),
                package: package.to_string(),
            },
        )
    }
}

impl std::fmt::Display for InstanceIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} (package {})", self.directory, self.package)
    }
}

/// Inputs for [`check_unique_name`].
struct UniquenessCheck<'check> {
    module_root: &'check Path,
    project_name: &'check str,
    target: InstanceIdentity,
}

/// Whether an evaluated instance would share the target's state: it
/// declares the same project name and an `infrastructure` block. A child
/// directory inherits its ancestors' fields in CUE, so a child without its
/// own `name` shares its parent's; that only matters for state when the
/// `infrastructure` block is present too, and then it is a real conflict.
fn shares_state(value: &serde_json::Value, project_name: &str) -> bool {
    value.get("name").and_then(serde_json::Value::as_str) == Some(project_name)
        && value
            .get("infrastructure")
            .is_some_and(|infrastructure| !infrastructure.is_null())
}

fn check_unique_name(check: &UniquenessCheck<'_>) -> Result<(), CliError> {
    // Every instance of every package, recursively; any instance that fails
    // to load, build or export fails the whole call, so nothing that could
    // declare the same project is silently left out. No concrete paths here:
    // siblings only need to evaluate, not to be complete infrastructure.
    let options = ModuleEvalOptions {
        recursive: true,
        package_scope: PackageScope::All,
        instance_failures: InstanceFailures::Fail,
        ..Default::default()
    };
    let evaluation =
        cuengine::evaluate_module(check.module_root, "", Some(&options)).map_err(|error| {
            CliError::eval_with_help(
                format!(
                    "cannot confirm that project name '{}' is unique in this CUE module: {error}",
                    check.project_name
                ),
                "Infrastructure state is keyed by module path and project name, and an \
                 instance that does not evaluate could declare a project with the same name. \
                 Fix or remove the instances listed above; `state` and `unlock` still work \
                 meanwhile.",
            )
        })?;

    let mut duplicates: Vec<InstanceIdentity> = evaluation
        .instances
        .iter()
        .filter(|(_, value)| shares_state(value, check.project_name))
        .map(|(key, _)| InstanceIdentity::from_key(key))
        .filter(|identity| *identity != check.target)
        .collect();
    if duplicates.is_empty() {
        return Ok(());
    }
    duplicates.sort();
    let names: Vec<String> = duplicates.iter().map(ToString::to_string).collect();
    Err(CliError::config_with_help(
        format!(
            "project name '{}' with an `infrastructure` block is also declared by {} in this \
             CUE module; infrastructure state is keyed by module path and project name, so \
             they would share state",
            check.project_name,
            names.join(", ")
        ),
        "Give each project in the module a unique `name`. A directory below a project \
         inherits its `name` and `infrastructure` unless it sets its own.",
    ))
}
