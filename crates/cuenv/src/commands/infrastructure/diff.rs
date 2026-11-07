//! `cuenv infrastructure diff`: how the working tree changes the
//! infrastructure configuration, from CUE alone.
//!
//! The working tree and a base revision are evaluated the way `plan`
//! evaluates the selected configuration, and compared declaration by
//! declaration. Nothing else is touched: no secret is resolved, no provider
//! started, no state read and nothing sent over the network, so it needs no
//! credential. The base is checked out into a temporary detached git
//! worktree, which is removed afterwards, also when the diff fails or is
//! interrupted.
//!
//! Generated provider types (`cue.mod/gen`) are not committed: the base
//! borrows the working tree's wherever it has none, and its provider versions
//! and schema digests then come from its own `cuenv.lock`.

mod changes;
mod markdown;
mod revision;
mod snapshot;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cuenv_infrastructure::references::ConfigurationEvaluator as _;
use cuenv_infrastructure::strip_control_characters;
use cuenv_manifest::lockfile::LockedInfrastructureProvider;

use self::changes::Changes;
use self::markdown::Comparison;
use self::revision::BaseCheckout;
use self::snapshot::{Selected, Snapshot};
use super::evaluation::{self, NameCheck, Needs, Target, TargetRequest};
use crate::cli::CliError;
use crate::commands::env_file::find_cue_module_root;

/// Options for `cuenv infrastructure diff`.
#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Path to the project directory.
    pub path: String,
    /// CUE package to evaluate.
    pub package: String,
    /// Named infrastructure environment selected with `--env`.
    pub environment: Option<String>,
    /// Git revision to compare with, such as `origin/main`.
    pub base: String,
}

/// Execute `cuenv infrastructure diff` and return the Markdown it prints.
///
/// # Errors
///
/// Returns an error when the project is not in a git working tree, the base
/// revision does not name a commit, the working tree's configuration does not
/// evaluate, or the base's configuration is declared but does not evaluate.
pub async fn execute_diff(options: &DiffOptions) -> Result<String, CliError> {
    let project = Path::new(&options.path).canonicalize().map_err(|error| {
        CliError::config(format!("cannot resolve path {}: {error}", options.path))
    })?;
    let base = options.base.clone();
    let located = project.clone();
    // The checkout belongs to this future, so dropping the future (an
    // interrupt) removes the worktree too.
    let (repository, checkout) = blocking(move || {
        let repository = revision::repository_root(&located)?;
        let checkout = BaseCheckout::create(&repository, &base)?;
        Ok((repository, checkout))
    })
    .await?;
    let sides = Sides::of(&SidesRequest {
        project: &project,
        repository: &repository,
        checkout: checkout.root(),
        options,
    })?;
    let changes = blocking(move || compare(&sides)).await?;
    let base_label = strip_control_characters(&options.base);
    let rendered = markdown::render(
        &changes,
        &Comparison {
            base: &base_label,
            commit: checkout.commit(),
            environment: options.environment.as_deref(),
        },
    );
    drop(checkout);
    Ok(rendered)
}

/// Run blocking work (git, CUE evaluation) off the async runtime.
async fn blocking<Output: Send + 'static>(
    work: impl FnOnce() -> Result<Output, CliError> + Send + 'static,
) -> Result<Output, CliError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CliError::other(format!("the diff stopped unexpectedly: {error}")))?
}

/// The two configurations a diff evaluates.
#[derive(Debug)]
struct Sides {
    /// The project directory in the working tree.
    head: PathBuf,
    /// The same directory in the base checkout; it may not exist there.
    base: PathBuf,
    /// The CUE modules holding the two, for the generated provider types.
    modules: Option<(PathBuf, PathBuf)>,
    package: String,
    environment: Option<String>,
}

struct SidesRequest<'request> {
    project: &'request Path,
    repository: &'request Path,
    checkout: &'request Path,
    options: &'request DiffOptions,
}

impl Sides {
    fn of(request: &SidesRequest<'_>) -> Result<Self, CliError> {
        let in_checkout = |path: &Path| {
            path.strip_prefix(request.repository)
                .map(|relative| request.checkout.join(relative))
        };
        let base = in_checkout(request.project).map_err(|_| {
            CliError::config(format!(
                "{} is outside the git working tree {}",
                request.project.display(),
                request.repository.display()
            ))
        })?;
        // Without a CUE module the working tree fails to evaluate on its own.
        let modules = find_cue_module_root(request.project)
            .map(|module| {
                // The checkout holds only the repository: a module above it
                // would leave the base without one, and everything added.
                let base_module = in_checkout(&module).map_err(|_| {
                    CliError::config(format!(
                        "cannot diff infrastructure: the CUE module {} is outside the git \
                         working tree {}, so the base revision cannot be evaluated",
                        module.display(),
                        request.repository.display()
                    ))
                })?;
                Ok::<_, CliError>((module, base_module))
            })
            .transpose()?;
        Ok(Self {
            head: request.project.to_path_buf(),
            base,
            modules,
            package: request.options.package.clone(),
            environment: request.options.environment.clone(),
        })
    }

    /// What evaluates the project's configuration in `directory`.
    fn evaluation_of(&self, directory: &Path) -> Evaluation {
        Evaluation {
            path: directory.to_string_lossy().into_owned(),
            package: self.package.clone(),
            environment: self.environment.clone(),
        }
    }
}

/// The owned parts of a [`TargetRequest`].
struct Evaluation {
    path: String,
    package: String,
    environment: Option<String>,
}

impl Evaluation {
    /// The request: only this instance is evaluated (no module-wide name
    /// check, which concerns state), and the configuration `plan` would
    /// select must be complete.
    fn request(&self) -> TargetRequest<'_> {
        TargetRequest {
            path: &self.path,
            package: &self.package,
            name_check: NameCheck::TargetOnly,
            needs: Needs::Configuration,
            environment: self.environment.as_deref(),
        }
    }
}

/// Evaluate both sides and compare them.
fn compare(sides: &Sides) -> Result<Changes, CliError> {
    let head = evaluation::evaluate(sides.evaluation_of(&sides.head).request())?;
    let head = snapshot(&head, None);
    let base = if sides.base.is_dir() {
        let borrowed = match &sides.modules {
            Some((head_module, base_module)) => borrow_generated_types(head_module, base_module)?,
            None => false,
        };
        let base_evaluation = sides.evaluation_of(&sides.base);
        match evaluation::evaluate_if_declared(base_evaluation.request())? {
            Some(target) => {
                let pins = borrowed
                    .then(|| super::types::Module::containing(&sides.base))
                    .transpose()?
                    .map(|module| module.lockfile())
                    .transpose()?
                    .map(|lockfile| lockfile.infrastructure_providers);
                snapshot(&target, pins.as_ref())
            }
            None => Snapshot::default(),
        }
    } else {
        Snapshot::default()
    };
    Ok(Changes::between(&base, &head))
}

fn snapshot(
    target: &Target,
    pins: Option<&BTreeMap<String, LockedInfrastructureProvider>>,
) -> Snapshot {
    Snapshot::of(&Selected {
        infrastructure: &target.infrastructure,
        references: target
            .configuration_evaluator
            .as_ref()
            .map_or(&[], |evaluator| evaluator.references()),
        pins,
    })
}

/// Copy the working tree's generated provider types (`cue.mod/gen`, which
/// `cuenv infra init` writes and git ignores) into the base checkout wherever
/// it has none, so the base resolves the same imports. Whether anything was
/// copied.
fn borrow_generated_types(head_module: &Path, base_module: &Path) -> Result<bool, CliError> {
    let source = head_module.join("cue.mod").join("gen");
    let target = base_module.join("cue.mod").join("gen");
    if !source.is_dir() || !base_module.join("cue.mod").is_dir() {
        return Ok(false);
    }
    let failure = |error: &dyn std::fmt::Display| {
        CliError::config(format!(
            "cannot copy the generated provider types into the base checkout: {error}"
        ))
    };
    let mut copied = false;
    for entry in walkdir::WalkDir::new(&source) {
        let entry = entry.map_err(|error| failure(&error))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&source) else {
            continue;
        };
        let destination = target.join(relative);
        if destination.exists() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| failure(&error))?;
        }
        std::fs::copy(entry.path(), &destination).map_err(|error| failure(&error))?;
        copied = true;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests;
