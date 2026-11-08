//! `cuenv infrastructure diff`: how the working tree changes the
//! infrastructure configuration, from CUE alone.
//!
//! The selected configuration of the working tree and of a base revision,
//! checked out into a temporary detached git worktree, are evaluated the way
//! `plan` evaluates them, written out as JSON and compared as text. No secret
//! is resolved (a `#Secret` stays the reference it is), no provider started,
//! no state read and nothing sent over the network.
//!
//! Generated provider types (`cue.mod/gen`) are not committed: the base
//! borrows the working tree's wherever it has none, and its provider versions
//! and schema digests then come from its own `cuenv.lock`.

mod revision;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cuenv_infrastructure::references::{
    ConfigurationEvaluator as _, ConfigurationSelector, NativeReference, SourceAddress,
};
use cuenv_infrastructure::registry::ProviderSource;
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::InfrastructureProvider;
use serde_json::{Value as Json, json};
use similar::TextDiff;

use self::revision::BaseCheckout;
use super::evaluation::{self, NameCheck, Needs, Target, TargetRequest};
use super::types::Module;
use crate::cli::CliError;
use crate::commands::env_file::find_cue_module_root;

/// Pinned provider releases, by source.
type Pins = BTreeMap<String, LockedInfrastructureProvider>;

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
    let head = Path::new(&options.path).canonicalize().map_err(|error| {
        CliError::config(format!("cannot resolve path {}: {error}", options.path))
    })?;
    let (located, revision) = (head.clone(), options.base.clone());
    // The checkout belongs to this future, so dropping the future (an
    // interrupt) removes the worktree too.
    let checkout = blocking(move || BaseCheckout::create(&located, &revision)).await?;
    // The checkout holds only the repository: a CUE module above it would
    // leave the base without one, and everything added.
    let modules = find_cue_module_root(&head)
        .map(|module| match checkout.locate(&module) {
            Some(base_module) => Ok((module, base_module)),
            None => Err(CliError::config(format!(
                "cannot diff infrastructure: the CUE module {} is outside the git working tree",
                module.display()
            ))),
        })
        .transpose()?;
    let sides = Sides {
        base: checkout.locate(&head),
        head,
        modules,
        options: options.clone(),
    };
    let (base, head) = blocking(move || sides.configurations()).await?;
    Ok(render(&base, &head, checkout.label()))
}

/// Run blocking work (git, CUE evaluation) off the async runtime.
async fn blocking<Output: Send + 'static>(
    work: impl FnOnce() -> Result<Output, CliError> + Send + 'static,
) -> Result<Output, CliError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CliError::other(format!("the diff stopped unexpectedly: {error}")))?
}

/// The project in the working tree and in the base checkout.
struct Sides {
    head: PathBuf,
    base: Option<PathBuf>,
    /// The CUE modules holding the two, for the generated provider types.
    modules: Option<(PathBuf, PathBuf)>,
    options: DiffOptions,
}

impl Sides {
    /// The base's and the working tree's configuration, as text.
    fn configurations(&self) -> Result<(String, String), CliError> {
        let head = evaluation::evaluate(self.request(&self.head.to_string_lossy()))?;
        let mut base = None;
        if let Some(directory) = self.base.as_ref().filter(|directory| directory.is_dir()) {
            let pins = match &self.modules {
                Some((head_module, base_module)) => {
                    borrow_generated_types(head_module, base_module)?
                }
                None => None,
            };
            base = evaluation::evaluate_if_declared(self.request(&directory.to_string_lossy()))?;
            if let (Some(base), Some(pins)) = (base.as_mut(), pins) {
                pin(&mut base.infrastructure.providers, &pins);
            }
        }
        Ok((document(base.as_ref())?, document(Some(&head))?))
    }

    /// Only the instance in `directory` is evaluated (the module-wide name
    /// check concerns state), and the configuration `plan` would select must
    /// be complete.
    fn request<'request>(&'request self, directory: &'request str) -> TargetRequest<'request> {
        TargetRequest {
            path: directory,
            package: &self.options.package,
            name_check: NameCheck::TargetOnly,
            needs: Needs::Configuration,
            environment: self.options.environment.as_deref(),
        }
    }
}

/// The providers, managed resources and data sources of a selected
/// configuration (none when it is not declared) as JSON with sorted keys, one
/// value per line. A `#Secret` stays the reference it is; an output reference
/// (`null` after evaluation) is written where it goes as the output it reads,
/// such as `"${cloudflare_r2_bucket.bucket.outputs.name}"`.
fn document(target: Option<&Target>) -> Result<String, CliError> {
    let failure = |error| CliError::other(format!("cannot write the configuration: {error}"));
    let mut document = json!({"providers": {}, "resources": {}, "data": {}});
    if let Some(target) = target {
        let infrastructure = &target.infrastructure;
        document["providers"] = serde_json::to_value(&infrastructure.providers).map_err(failure)?;
        document["resources"] = serde_json::to_value(&infrastructure.resources).map_err(failure)?;
        document["data"] = serde_json::to_value(&infrastructure.data).map_err(failure)?;
        let evaluator = target.configuration_evaluator.as_ref();
        let references = evaluator.map_or(&[][..], |evaluator| evaluator.references());
        for reference in references.iter().collect::<BTreeSet<_>>() {
            place(&mut document, reference);
        }
    }
    document.sort_all_objects();
    Ok(serde_json::to_string_pretty(&document).map_err(failure)? + "\n")
}

/// Write an output reference at its destination in its consumer's
/// configuration. A value CUE computes from outputs says so, and names every
/// output it reads.
fn place(document: &mut Json, reference: &NativeReference) {
    let section = match reference.consumer {
        SourceAddress::Resource(_) => "resources",
        SourceAddress::DataSource(_) => "data",
    };
    let mut destination = document
        .get_mut(section)
        .and_then(|declarations| declarations.get_mut(&reference.consumer.address().name))
        .and_then(|declaration| declaration.get_mut("configuration"));
    for selector in &reference.destination {
        destination = destination.and_then(|value| match selector {
            ConfigurationSelector::Field(name) => value.get_mut(name),
            ConfigurationSelector::Index(index) => value.get_mut(usize::try_from(*index).ok()?),
        });
    }
    let Some(destination) = destination else {
        return;
    };
    let output = format!("${{{}.outputs.{}}}", reference.source, reference.attribute);
    *destination = Json::String(match destination.as_str() {
        Some(earlier) => format!("{earlier}, {output}"),
        None if reference.computed => format!("computed from {output}"),
        None => output,
    });
}

/// Replace the provider versions and schema digests, which the working
/// tree's generated types set, with the base's own pins.
fn pin(providers: &mut BTreeMap<String, InfrastructureProvider>, pins: &Pins) {
    for provider in providers.values_mut() {
        let source = ProviderSource::parse(&provider.source).map(|source| source.to_string());
        if let Some(pin) = source.ok().and_then(|source| pins.get(&source)) {
            provider.version = provider.version.as_ref().map(|_| pin.version.clone());
            let digest = provider.schema_digest.as_ref();
            provider.schema_digest = digest.map(|_| pin.schema_digest.clone());
        }
    }
}

/// The Markdown: a unified diff of the two configurations in a `diff` code
/// block, or one line when they are the same.
fn render(base: &str, head: &str, base_label: &str) -> String {
    if base == head {
        return format!("No infrastructure changes compared with {base_label}.\n");
    }
    let diff = TextDiff::from_lines(base, head)
        .unified_diff()
        .context_radius(3)
        .header(base_label, "working tree")
        .to_string();
    // A fence longer than any run of backticks in a value.
    let longest = diff.split(|character| character != '`').map(str::len).max();
    let fence = "`".repeat(longest.unwrap_or(0).max(2) + 1);
    format!("{fence}diff\n{diff}{fence}\n")
}

/// Copy the working tree's generated provider types (`cue.mod/gen`, which
/// `cuenv infra init` writes and git ignores) into the base checkout wherever
/// it has none, so the base resolves the same imports. When it copied any,
/// the pins of the base's `cuenv.lock`, which decide its provider versions.
fn borrow_generated_types(
    head_module: &Path,
    base_module: &Path,
) -> Result<Option<Pins>, CliError> {
    let source = head_module.join("cue.mod").join("gen");
    if !source.is_dir() || !base_module.join("cue.mod").is_dir() {
        return Ok(None);
    }
    let failure = |error: &dyn std::fmt::Display| {
        CliError::config(format!("cannot copy the generated provider types: {error}"))
    };
    let mut copied = false;
    for entry in walkdir::WalkDir::new(&source) {
        let entry = entry.map_err(|error| failure(&error))?;
        let relative = entry.path().strip_prefix(&source).unwrap_or(entry.path());
        let destination = base_module.join("cue.mod").join("gen").join(relative);
        if entry.file_type().is_file() && !destination.exists() {
            std::fs::create_dir_all(destination.parent().unwrap_or(base_module))
                .and_then(|()| std::fs::copy(entry.path(), &destination))
                .map_err(|error| failure(&error))?;
            copied = true;
        }
    }
    if !copied {
        return Ok(None);
    }
    let root = base_module.to_path_buf();
    Ok(Some(Module { root }.lockfile()?.infrastructure_providers))
}

#[cfg(test)]
mod tests;
