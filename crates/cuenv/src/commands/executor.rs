//! The [`CommandExecutor`], which runs commands with lazily evaluated CUE
//! modules and broadcasts their events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cuenv_core::{InstanceKind, ModuleEvaluation, Result};
use tokio::time::{Duration, sleep};

use super::{ModuleGuard, env_file, relative_path_from_root, schema_compat, version};
use crate::events::{Event, EventSender};

const VERSION_PROGRESS_STEPS: &[(f32, &str)] = &[
    (0.0, "Initializing..."),
    (0.2, "Loading version info..."),
    (0.4, "Checking build metadata..."),
    (0.6, "Gathering system info..."),
    (0.8, "Formatting output..."),
    (1.0, "Complete"),
];

/// Executes CLI commands with centralized module evaluation and event handling.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
pub struct CommandExecutor {
    /// Channel sender for broadcasting events to UI renderers.
    pub(super) event_sender: EventSender,
    /// Path-local module evaluations keyed by canonical target directory.
    pub(super) local_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// Workspace-wide module evaluations keyed by canonical module root.
    pub(super) workspace_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv").
    pub(super) package: String,
}

impl CommandExecutor {
    /// Create a new executor with the specified event sender and package name.
    #[must_use]
    pub fn new(event_sender: EventSender, package: String) -> Self {
        Self {
            event_sender,
            local_modules: Mutex::new(HashMap::new()),
            workspace_modules: Mutex::new(HashMap::new()),
            package,
        }
    }

    /// Get the CUE package name used for evaluation.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Create an executor that shares this executor's event channel but
    /// evaluates a different CUE package.
    #[must_use]
    pub fn for_package(&self, package: String) -> Self {
        Self::new(self.event_sender.clone(), package)
    }

    /// Get or load a path-local module evaluation (cached by target directory).
    ///
    /// This evaluates only the requested directory (`target_dir`) with
    /// `recursive: false` and does not scan sibling projects.
    ///
    /// # Arguments
    /// * `path` - Directory to evaluate
    ///
    /// # Returns
    /// A `ModuleGuard` that provides direct access to the `ModuleEvaluation`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The module lock cannot be acquired (poisoned mutex)
    /// - The path cannot be canonicalized
    /// - No CUE module root (cue.mod/) is found starting from the given path
    /// - CUE evaluation fails for the target directory
    pub fn get_module(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let target_path = path.canonicalize().map_err(|e| {
            cuenv_core::Error::io_with_path("canonicalize path", path.to_path_buf(), e)
        })?;

        let mut guard = self.local_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire local module cache lock")
        })?;

        if !guard.contains_key(&target_path) {
            let module = match self.local_module_from_workspace_cache(&target_path) {
                Some(module) => module,
                None => self.evaluate_path_module(&target_path)?,
            };
            guard.insert(target_path.clone(), module);
        }

        Ok(ModuleGuard {
            guard,
            key: target_path,
        })
    }

    /// Derive a path-local module evaluation from a cached workspace evaluation.
    ///
    /// Workspace evaluations (built by `discover_all_modules`) already contain
    /// the per-directory instance for every `env.cue` in the module, evaluated
    /// with the same options as a path-local evaluation. Reusing that cached
    /// instance avoids a redundant CUE evaluation per project during
    /// workspace-wide operations such as `cuenv sync -A`.
    ///
    /// Returns `None` when no cached workspace evaluation covers `target_path`,
    /// in which case the caller falls back to a fresh path-local evaluation.
    pub(super) fn local_module_from_workspace_cache(
        &self,
        target_path: &Path,
    ) -> Option<ModuleEvaluation> {
        let workspace = self.workspace_modules.lock().ok()?;
        workspace.values().find_map(|module| {
            target_path.strip_prefix(&module.root).ok()?;
            let rel_path = relative_path_from_root(&module.root, target_path);
            let instance = module.get(&rel_path)?;
            Some(ModuleEvaluation {
                direct_references: module.direct_references.clone(),
                root: module.root.clone(),
                instances: HashMap::from([(rel_path, instance.clone())]),
            })
        })
    }

    /// Discover and load all modules in the current workspace (cached by module root).
    ///
    /// This scans all matching `env.cue` files in the module and evaluates each
    /// directory individually with `recursive: false`, then merges the results.
    ///
    /// Use this only for explicitly workspace-wide commands (for example `sync -A`).
    ///
    /// # Errors
    ///
    /// Returns an error if module discovery/evaluation fails.
    pub fn discover_all_modules(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let target_path = path.canonicalize().map_err(|e| {
            cuenv_core::Error::io_with_path("canonicalize path", path.to_path_buf(), e)
        })?;

        let module_root = env_file::find_cue_module_root(&target_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                target_path.display()
            ))
        })?;
        let mut guard = self.workspace_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
        })?;

        if !guard.contains_key(&module_root) {
            schema_compat::warn_for_module(&module_root)?;
            let module = self.evaluate_workspace_module(&module_root)?;
            module.require_unique_project_names()?;
            guard.insert(module_root.clone(), module);
        }

        Ok(ModuleGuard {
            guard,
            key: module_root,
        })
    }

    /// Get the module root path if at least one module has been loaded.
    ///
    /// Returns `None` if no module has been loaded yet.
    #[must_use]
    pub fn module_root(&self) -> Option<PathBuf> {
        let local_root = self
            .local_modules
            .lock()
            .ok()
            .and_then(|g| g.values().next().map(|m| m.root.clone()));
        if local_root.is_some() {
            return local_root;
        }
        self.workspace_modules
            .lock()
            .ok()
            .and_then(|g| g.values().next().map(|m| m.root.clone()))
    }

    /// Compute the relative path from module root to target directory.
    ///
    /// This is a convenience wrapper around `relative_path_from_root` that
    /// uses the cached module root. Returns an error if the module hasn't
    /// been loaded yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the module has not been loaded yet (call `get_module` first).
    pub fn relative_path(&self, target: &Path) -> Result<PathBuf> {
        let root = self.module_root().ok_or_else(|| {
            cuenv_core::Error::configuration("Module not loaded; call get_module first")
        })?;
        Ok(relative_path_from_root(&root, target))
    }

    /// Check if a path is a Project (vs Base) using schema unification.
    ///
    /// This uses the CUE schema verification performed during module evaluation
    /// to determine if an instance conforms to `schema.#Project`.
    #[must_use]
    pub fn is_project(&self, path: &Path) -> bool {
        let Ok(target_path) = path.canonicalize() else {
            return false;
        };

        let check = |modules: &HashMap<PathBuf, ModuleEvaluation>| -> Option<bool> {
            modules.values().find_map(|module| {
                let rel_path = relative_path_from_root(&module.root, &target_path);
                module
                    .get(&rel_path)
                    .map(|instance| instance.kind == InstanceKind::Project)
            })
        };

        if let Ok(modules) = self.local_modules.lock()
            && let Some(result) = check(&modules)
        {
            return result;
        }

        self.workspace_modules
            .lock()
            .ok()
            .and_then(|modules| check(&modules))
            .unwrap_or(false)
    }

    /// Execute version command with progress events.
    pub(super) async fn execute_version(&self) -> Result<()> {
        let command_name = "version";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        for (index, (progress, message)) in VERSION_PROGRESS_STEPS.iter().enumerate() {
            self.send_event(Event::CommandProgress {
                command: command_name.to_string(),
                progress: *progress,
                message: (*message).to_string(),
            });

            if index + 1 < VERSION_PROGRESS_STEPS.len() {
                sleep(Duration::from_millis(200)).await;
            }
        }

        let version_info = version::get_version_info();

        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: version_info,
        });

        Ok(())
    }

    /// Send an event to all registered UI renderers.
    ///
    /// This method broadcasts events through the event channel. If sending fails
    /// (e.g., all receivers have been dropped), the error is silently ignored
    /// since this is expected when the channel is not being consumed.
    pub(crate) fn send_event(&self, event: Event) {
        // Silently ignore send failures - this is expected when no receiver is attached
        let _ = self.event_sender.send(event);
    }
}
