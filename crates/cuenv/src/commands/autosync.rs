//! Keep generated files in step with their sources on every command.
//!
//! The first time a command evaluates a project, cuenv compares codegen and
//! `.rules.cue` outputs with fingerprints of what it last wrote
//! (`.cuenv/autosync.json`). An unchanged project costs a few `stat` calls;
//! only drift renders and writes, exactly as `cuenv sync codegen` and
//! `cuenv sync rules -A` would. Nothing is written when `CI` is set or
//! `CUENV_AUTOSYNC=0`, and never during `cuenv sync`, whose `--check` must see
//! the files as they are.

mod store;

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cuenv_core::Result;
use cuenv_core::manifest::Project;

use super::CommandExecutor;
use super::sync::functions::{CodegenSyncOptions, CodegenSyncRequest, execute_sync_codegen};
use store::{CodegenPrint, digest};
pub use store::{FileStamp, RulesPrint, Store, stamps};

/// Whether this process may write generated files on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncMode {
    Write,
    Off,
}

impl AutoSyncMode {
    /// Off in CI, where generated files must be committed rather than made,
    /// and when `CUENV_AUTOSYNC` is `0` or `false`.
    pub fn from_environment() -> Self {
        let set = |name: &str| {
            std::env::var(name).is_ok_and(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
        };
        let disabled = std::env::var("CUENV_AUTOSYNC").is_ok_and(|value| {
            let value = value.trim();
            value == "0" || value.eq_ignore_ascii_case("false")
        });
        if disabled || set("CI") {
            Self::Off
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
            mode: Mutex::new(AutoSyncMode::from_environment()),
            synced: Mutex::new(HashSet::new()),
            rules_synced: Mutex::new(false),
        }
    }
}

impl AutoSync {
    pub fn disable(&self) {
        if let Ok(mut mode) = self.mode.lock() {
            *mode = AutoSyncMode::Off;
        }
    }

    fn writes(&self) -> bool {
        self.mode
            .lock()
            .is_ok_and(|mode| *mode == AutoSyncMode::Write)
    }

    /// True the first time `path` is claimed in this process.
    fn claim(&self, path: &Path) -> bool {
        self.writes()
            && self
                .synced
                .lock()
                .is_ok_and(|mut synced| synced.insert(path.to_path_buf()))
    }

    fn claim_rules(&self) -> bool {
        self.writes()
            && self.rules_synced.lock().is_ok_and(|mut done| {
                let first = !*done;
                *done = true;
                first
            })
    }
}

impl CommandExecutor {
    /// Sync the generated files of the project at `project_dir` (canonical)
    /// and of the repository's `.rules.cue` files, once per process.
    ///
    /// Must be called without holding a module lock: syncing reads the
    /// project through [`CommandExecutor::module`]. Failures are reported and
    /// never fail the command that triggered them.
    pub(super) fn autosync(&self, module_root: &Path, project_dir: &Path) {
        if self.autosync.claim(project_dir) {
            report(self.autosync_codegen(module_root, project_dir));
        }
        if self.autosync.claim_rules() {
            report(super::sync::providers::rules::autosync(self, project_dir));
        }
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
        if output.lines().any(|line| !line.trim().is_empty()) {
            tracing::debug!(%output, "codegen autosync");
        }
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
