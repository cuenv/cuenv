//! Scripts sync provider.
//!
//! Writes `scripts.gen.cue`, the list of a project's `scriptDirs` scripts,
//! beside each project that declares `scriptDirs`, and removes it from a
//! project that no longer does. Every other cuenv command does the same on
//! its own; this provider is the explicit form, and the check CI runs.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::scripts::{self, Drift, ScriptDirectory};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::commands::CommandExecutor;
use crate::commands::autosync;
use crate::commands::sync::provider::{SyncMode, SyncProvider, SyncRequest, SyncResult, SyncScope};

/// Sync provider for `scripts.gen.cue`.
pub struct ScriptsSyncProvider;

#[async_trait]
impl SyncProvider for ScriptsSyncProvider {
    fn name(&self) -> &'static str {
        "scripts"
    }

    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult> {
        let projects = declared_projects(request.executor, request.path, request.scope)?;
        let lines = projects
            .iter()
            .map(|(project_dir, directories)| {
                sync_project(project_dir, directories.as_deref(), request)
            })
            .collect::<Result<Vec<_>>>()?;
        if lines.iter().any(|line| line.changed) && request.options.mode == SyncMode::Write {
            // The files CUE evaluates changed under the cached evaluations.
            request.executor.forget_evaluations()?;
        }
        let output = lines
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(SyncResult::success(if output.is_empty() {
            "No projects with scriptDirs.".to_string()
        } else {
            output
        }))
    }
}

/// A project directory and the `scriptDirs` it declares.
type Declared = (PathBuf, Option<Vec<ScriptDirectory>>);

/// The projects in scope with the `scriptDirs` each declares.
///
/// When evaluation fails, which a reference to a script missing from a
/// stale `scripts.gen.cue` causes, the `scriptDirs` each project last
/// evaluated with stand in.
fn declared_projects(
    executor: &CommandExecutor,
    path: &Path,
    scope: SyncScope,
) -> Result<Vec<Declared>> {
    let evaluated = match scope {
        SyncScope::Path => executor.module(path),
        SyncScope::Workspace => executor.discover_all_modules(path),
    };
    match evaluated {
        Ok(module) => Ok(module
            .projects()
            .map(|instance| {
                (
                    module.root.join(&instance.path),
                    scripts::directories(&instance.value),
                )
            })
            .collect()),
        Err(error) => {
            let canonical = path.canonicalize().map_err(|io| {
                cuenv_core::Error::io_with_path("canonicalize path", path.to_path_buf(), io)
            })?;
            let Some(module_root) = crate::commands::env_file::find_cue_module_root(&canonical)
            else {
                return Err(error);
            };
            let known: Vec<_> = autosync::known_script_projects(&module_root)
                .into_iter()
                .map(|(project, declared)| (PathBuf::from(project), declared))
                .filter(|(project, _)| match scope {
                    SyncScope::Path => *project == canonical,
                    SyncScope::Workspace => true,
                })
                .map(|(project, declared)| {
                    let directories =
                        scripts::directories(&serde_json::json!({ "scriptDirs": declared }));
                    (project, directories)
                })
                .collect();
            if known.is_empty() {
                return Err(error);
            }
            tracing::debug!(%error, "syncing scripts from the scriptDirs last evaluated");
            Ok(known)
        }
    }
}

struct Line {
    text: String,
    changed: bool,
}

fn sync_project(
    project_dir: &Path,
    directories: Option<&[ScriptDirectory]>,
    request: SyncRequest<'_>,
) -> Result<Line> {
    let (drift, scan) = scripts::drift(project_dir, directories);
    let file = scripts::generated_path(project_dir);
    let shown = file
        .strip_prefix(std::env::current_dir().unwrap_or_default())
        .unwrap_or(&file)
        .display()
        .to_string();
    let clashes = scan.clashes.iter().fold(String::new(), |mut text, clash| {
        let _ = write!(text, "\n  {clash}");
        text
    });
    if request.options.mode == SyncMode::Check && !scan.clashes.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "files of scriptDirs clash; none of them is a script until renamed:{clashes}"
        )));
    }
    let text = match (&drift, &request.options.mode) {
        (Drift::None, _) if directories.is_none() => {
            return Ok(Line {
                text: String::new(),
                changed: false,
            });
        }
        (Drift::None, _) => format!("{shown}: in sync"),
        (_, SyncMode::Check) => {
            return Err(cuenv_core::Error::configuration(format!(
                "{shown} does not match the project's scriptDirs; run `cuenv sync scripts` and commit it{clashes}"
            )));
        }
        (Drift::Orphaned, SyncMode::DryRun) => format!("{shown}: Would remove"),
        (_, SyncMode::DryRun) => format!("{shown}: Would update"),
        (_, SyncMode::Write) => {
            autosync::write_generated(
                project_dir,
                request.package,
                autosync::written(&drift, &scan),
            )?;
            if drift == Drift::Orphaned {
                format!("{shown}: Removed")
            } else {
                format!("{shown}: Updated")
            }
        }
    };
    Ok(Line {
        text: format!("{text}{clashes}"),
        changed: drift != Drift::None,
    })
}
