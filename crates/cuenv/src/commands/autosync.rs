//! Keep generated files in step with their sources on every command.
//!
//! The first time a command evaluates a project, cuenv:
//!
//! - rewrites `scripts.gen.cue` when the files of its `scriptDirs` changed,
//!   then evaluates the project again, so `scripts.<name>` always names the
//!   scripts on disk;
//! - resyncs codegen outputs and `.rules.cue` outputs whose sources changed
//!   since it last wrote them, judged by fingerprints in
//!   `.cuenv/autosync.json`, so an unchanged project costs a few `stat`
//!   calls.
//!
//! When `CI` is set nothing is written, and a stale `scripts.gen.cue` is an
//! error: generated files are committed, not made on a runner.
//! `CUENV_AUTOSYNC=0` turns all of it off, and `cuenv sync`, whose `--check`
//! must see the files as they are, never syncs on its own.

mod store;

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cuenv_core::Result;
use cuenv_core::manifest::Project;
use cuenv_core::scripts::{self, Drift};
use serde_json::Value;

use super::CommandExecutor;
use super::sync::functions::{CodegenSyncOptions, CodegenSyncRequest, execute_sync_codegen};
use store::{CodegenPrint, digest};
pub use store::{FileStamp, RulesPrint, Store, stamps};

/// What this process does about generated files on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncMode {
    /// Rewrite what drifted.
    Write,
    /// Write nothing; a stale `scripts.gen.cue` fails the command.
    Check,
    /// Leave generated files alone.
    Off,
}

impl AutoSyncMode {
    /// `Check` in CI, `Off` when `CUENV_AUTOSYNC` is `0` or `false`, else
    /// `Write`.
    pub fn from_environment() -> Self {
        let disabled = std::env::var("CUENV_AUTOSYNC").is_ok_and(|value| {
            let value = value.trim();
            value == "0" || value.eq_ignore_ascii_case("false")
        });
        let ci = std::env::var("CI").is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        });
        if disabled {
            Self::Off
        } else if ci {
            Self::Check
        } else {
            Self::Write
        }
    }
}

/// Per-process autosync state: the mode and the paths already synced.
#[derive(Debug)]
pub struct AutoSync {
    mode: Mutex<AutoSyncMode>,
    synced: Mutex<HashSet<PathBuf>>,
    rules_synced: Mutex<bool>,
}

impl Default for AutoSync {
    fn default() -> Self {
        Self {
            // Unit tests evaluate fixtures, examples and this repository;
            // none of them may be rewritten by it.
            mode: Mutex::new(if cfg!(test) {
                AutoSyncMode::Off
            } else {
                AutoSyncMode::from_environment()
            }),
            synced: Mutex::new(HashSet::new()),
            rules_synced: Mutex::new(false),
        }
    }
}

impl AutoSync {
    /// Leave generated files alone for the rest of the process.
    pub fn disable(&self) {
        if let Ok(mut mode) = self.mode.lock() {
            *mode = AutoSyncMode::Off;
        }
    }

    fn mode(&self) -> AutoSyncMode {
        self.mode.lock().map_or(AutoSyncMode::Off, |mode| *mode)
    }

    /// True the first time `path` is claimed in this process, unless off.
    fn claim(&self, path: &Path) -> bool {
        self.mode() != AutoSyncMode::Off
            && self
                .synced
                .lock()
                .is_ok_and(|mut synced| synced.insert(path.to_path_buf()))
    }

    fn claim_rules(&self) -> bool {
        self.mode() == AutoSyncMode::Write
            && self.rules_synced.lock().is_ok_and(|mut done| {
                let first = !*done;
                *done = true;
                first
            })
    }
}

/// Whether syncing changed a file CUE evaluates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Evaluated {
    /// The evaluation still holds.
    Current,
    /// `scripts.gen.cue` was rewritten; the project must be evaluated again.
    Stale,
}

impl CommandExecutor {
    /// Sync the generated files of the project at `project_dir` (canonical),
    /// once per process, from its evaluated `value`.
    ///
    /// Must be called without holding a module lock: codegen reads the
    /// project through [`CommandExecutor::module`]. Only a stale
    /// `scripts.gen.cue` in CI fails; any other failure is reported and the
    /// command goes on.
    ///
    /// # Errors
    ///
    /// Returns an error in CI when `scripts.gen.cue` does not list the
    /// project's scripts.
    pub(super) fn autosync(
        &self,
        module_root: &Path,
        project_dir: &Path,
        value: Option<&Value>,
    ) -> Result<Evaluated> {
        if !self.autosync.claim(project_dir) {
            return Ok(Evaluated::Current);
        }
        let evaluated = match value {
            Some(value) => self.sync_scripts(module_root, project_dir, value)?,
            None => Evaluated::Current,
        };
        if self.autosync.mode() == AutoSyncMode::Write && evaluated == Evaluated::Current {
            report(self.autosync_codegen(module_root, project_dir));
        }
        if self.autosync.claim_rules() {
            report(super::sync::providers::rules::autosync(self, project_dir));
        }
        Ok(evaluated)
    }

    /// Bring `scripts.gen.cue` in line with the project's `scriptDirs`.
    fn sync_scripts(
        &self,
        module_root: &Path,
        project_dir: &Path,
        value: &Value,
    ) -> Result<Evaluated> {
        let directories = scripts::directories(value);
        if self.autosync.mode() == AutoSyncMode::Write {
            remember_script_dirs(module_root, project_dir, value.get("scriptDirs"));
        }
        let (drift, scan) = scripts::drift(project_dir, directories.as_deref());
        for clash in &scan.clashes {
            cuenv_events::emit_stderr!(format!("cuenv: {clash}"));
        }
        if drift == Drift::None {
            return Ok(Evaluated::Current);
        }
        if self.autosync.mode() == AutoSyncMode::Check {
            return Err(cuenv_core::Error::configuration(format!(
                "{} does not list the scripts of {}; run `cuenv sync scripts` and commit it",
                scripts::GENERATED_FILE,
                project_dir.display()
            )));
        }
        write_generated(project_dir, &self.package, written(&drift, &scan))?;
        Ok(Evaluated::Stale)
    }

    /// After an evaluation failed: rewrite `scripts.gen.cue` from the
    /// `scriptDirs` the project last evaluated with, so that a reference to
    /// a script added since resolves. True when the file was rewritten.
    pub(super) fn recover_scripts(&self, project_dir: &Path) -> bool {
        if self.autosync.mode() != AutoSyncMode::Write {
            return false;
        }
        let Some(module_root) = super::env_file::find_cue_module_root(project_dir) else {
            return false;
        };
        let Some(declared) =
            known_script_projects(&module_root).remove(&project_dir.display().to_string())
        else {
            return false;
        };
        let directories = scripts::directories(&serde_json::json!({ "scriptDirs": declared }));
        let (drift, scan) = scripts::drift(project_dir, directories.as_deref());
        drift == Drift::Stale && write_generated(project_dir, &self.package, Some(&scan)).is_ok()
    }

    fn autosync_codegen(&self, module_root: &Path, project_dir: &Path) -> Result<()> {
        let Some(project) = self.project_at(module_root, project_dir) else {
            return Ok(());
        };
        let Some(codegen) = &project.codegen else {
            return Ok(());
        };
        let outputs = codegen_outputs(project_dir, codegen);
        let config = digest(&(codegen, &project.formatters));
        let key = project_dir.display().to_string();

        let Some(mut store) = Store::open(module_root) else {
            return Ok(());
        };
        let current = CodegenPrint {
            config: config.clone(),
            outputs: stamps(outputs.iter().map(PathBuf::as_path)),
        };
        if store.prints.codegen.get(&key) == Some(&current) {
            return Ok(());
        }

        let path = project_dir.display().to_string();
        let package = self.package().to_string();
        let output = block_on(execute_sync_codegen(
            CodegenSyncRequest {
                path: &path,
                package: &package,
                options: CodegenSyncOptions {
                    dry_run: cuenv_core::DryRun::No,
                    check: false,
                    diff: false,
                },
            },
            self,
        ))??;
        tracing::debug!(%output, "codegen autosync");
        let written = CodegenPrint {
            config,
            outputs: stamps(outputs.iter().map(PathBuf::as_path)),
        };
        // A project seen for the first time is synced once to learn its
        // fingerprint; only a later change is worth telling the user about.
        let seen = store.prints.codegen.contains_key(&key);
        if seen && written != current {
            cuenv_events::emit_stderr!(format!(
                "cuenv: synced codegen files of project {}",
                project.name
            ));
        }
        store.prints.codegen.insert(key, written);
        store.save();
        Ok(())
    }

    /// The project evaluated at `project_dir`, if it is one.
    fn project_at(&self, module_root: &Path, project_dir: &Path) -> Option<Project> {
        let module = self.module(project_dir).ok()?;
        let relative = super::relative_path_from_root(module_root, project_dir);
        module.get(&relative)?.deserialize().ok()
    }
}

/// Write the generated scripts file of a project for `scan`, or remove it
/// when there is no scan, as for a project that no longer declares
/// `scriptDirs`.
///
/// # Errors
///
/// Returns an error when the file cannot be written or removed.
pub fn write_generated(
    project_dir: &Path,
    package: &str,
    scan: Option<&scripts::Scan>,
) -> Result<()> {
    let path = scripts::generated_path(project_dir);
    match scan {
        None => std::fs::remove_file(&path)
            .map_err(|error| cuenv_core::Error::io_with_path("remove", path.clone(), error))?,
        Some(scan) => {
            let existing = std::fs::read_to_string(&path).ok();
            let package = existing
                .as_deref()
                .and_then(scripts::package_of)
                .unwrap_or(package);
            let text = scripts::render(package, scan);
            let dir = path.parent().unwrap_or(project_dir);
            let written = (|| -> std::io::Result<()> {
                let mut file = tempfile::NamedTempFile::new_in(dir)?;
                std::io::Write::write_all(&mut file, text.as_bytes())?;
                file.persist(&path).map_err(|error| error.error)?;
                Ok(())
            })();
            written
                .map_err(|error| cuenv_core::Error::io_with_path("write", path.clone(), error))?;
        }
    }
    cuenv_events::emit_stderr!(format!("cuenv: updated {}", relative_or_full(&path)));
    Ok(())
}

fn relative_or_full(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// What [`write_generated`] takes for a drift: nothing when the file is to go.
pub fn written<'scan>(drift: &Drift, scan: &'scan scripts::Scan) -> Option<&'scan scripts::Scan> {
    (*drift != Drift::Orphaned).then_some(scan)
}

/// Keep the project's `scriptDirs` for [`CommandExecutor::recover_scripts`],
/// writing the store only when they changed.
fn remember_script_dirs(module_root: &Path, project_dir: &Path, declared: Option<&Value>) {
    let key = project_dir.display().to_string();
    let known = known_script_projects(module_root).remove(&key);
    if known.as_ref() == declared {
        return;
    }
    if let Some(mut store) = Store::open(module_root) {
        match declared {
            Some(declared) => store.prints.scripts.insert(key, declared.clone()),
            None => store.prints.scripts.remove(&key),
        };
        store.save();
    }
}

/// Outside CI, rewrite each stale `scripts.gen.cue` of the module around
/// `start` from the `scriptDirs` its project last evaluated with. Costs a
/// directory scan per project and no evaluation.
pub fn refresh_known_scripts(start: &Path) {
    if AutoSyncMode::from_environment() != AutoSyncMode::Write {
        return;
    }
    let Some(module_root) = super::env_file::find_cue_module_root(start) else {
        return;
    };
    for (project, declared) in known_script_projects(&module_root) {
        let project_dir = PathBuf::from(project);
        let directories = scripts::directories(&serde_json::json!({ "scriptDirs": declared }));
        let (drift, scan) = scripts::drift(&project_dir, directories.as_deref());
        if drift == Drift::Stale {
            report(write_generated(&project_dir, "cuenv", Some(&scan)));
        }
    }
}

/// The `scriptDirs` of each project of a module that declared any, by
/// project directory, as last evaluated.
pub fn known_script_projects(module_root: &Path) -> BTreeMap<String, Value> {
    Store::open_existing(module_root).map_or_else(BTreeMap::new, |store| store.prints.scripts)
}

/// Every file codegen of a project writes, `.gitignore` included.
fn codegen_outputs(
    project_dir: &Path,
    codegen: &cuenv_core::manifest::CodegenConfig,
) -> Vec<PathBuf> {
    codegen
        .files
        .keys()
        .map(|file| project_dir.join(file))
        .chain(std::iter::once(project_dir.join(".gitignore")))
        .collect()
}

fn report(result: Result<()>) {
    if let Err(error) = result {
        cuenv_events::emit_stderr!(format!(
            "cuenv: generated files were not synced: {error}. Run `cuenv sync` to see why."
        ));
    }
}

/// Run a future to completion from synchronous code, inside or outside a
/// Tokio runtime, on a thread of its own.
fn block_on<F>(future: F) -> Result<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|runtime| runtime.block_on(future))
            })
            .join()
    })
    .map_err(|_| cuenv_core::Error::configuration("autosync thread panicked"))?
    .map_err(|error| cuenv_core::Error::io("start autosync runtime", error))
}

#[cfg(test)]
#[path = "autosync_tests.rs"]
mod tests;
