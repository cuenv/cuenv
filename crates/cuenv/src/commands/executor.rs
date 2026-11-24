//! The [`CommandExecutor`], which runs commands with lazily evaluated CUE
//! modules and broadcasts their events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cuenv_core::{InstanceKind, ModuleEvaluation, Result};

use super::{ModuleGuard, env_file, schema_compat};

/// Executes CLI commands with centralized module evaluation.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
pub struct CommandExecutor {
    /// Path-local module evaluations keyed by canonical target directory.
    pub(super) local_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// Workspace-wide module evaluations keyed by canonical module root.
    pub(super) workspace_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv").
    pub(super) package: String,
    /// Generated files synced on the first evaluation of each project.
    pub(super) autosync: super::autosync::AutoSync,
}

impl CommandExecutor {
    /// Create a new executor for the specified package name.
    #[must_use]
    pub fn new(package: String) -> Self {
        Self {
            local_modules: Mutex::new(HashMap::new()),
            workspace_modules: Mutex::new(HashMap::new()),
            package,
            autosync: super::autosync::AutoSync::default(),
        }
    }

    /// Leave generated files alone for the life of this executor, for
    /// callers that only read projects, such as tests over checked-in
    /// examples.
    pub fn disable_autosync(&self) {
        self.autosync.disable();
    }

    /// Get the CUE package name used for evaluation.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
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
    pub fn module(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let target_path = path.canonicalize().map_err(|e| {
            cuenv_core::Error::io_with_path("canonicalize path", path.to_path_buf(), e)
        })?;

        let loaded = match self.load_local_module(&target_path) {
            Ok(loaded) => loaded,
            // A reference to a script added since the last sync fails the
            // evaluation before cuenv can see the script: sync from the
            // directories the project last declared, then try once more.
            Err(error)
                if self.recover_scripts(super::autosync::RecoveryScope::Project(&target_path)) =>
            {
                tracing::debug!(%error, "evaluating again after updating scripts.gen.cue");
                self.load_local_module(&target_path)?
            }
            Err(error) => return Err(error),
        };
        // Syncing reads the project through this method, so it runs with
        // the cache unlocked.
        if let Some((module_root, value)) = loaded {
            let evaluated = self.autosync(&module_root, &target_path, value.as_ref())?;
            if evaluated == super::autosync::Evaluated::Stale {
                self.forget_local_module(&target_path)?;
                self.load_local_module(&target_path)?;
            }
        }

        let guard = self.local_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire local module cache lock")
        })?;
        Ok(ModuleGuard {
            guard,
            key: target_path,
        })
    }

    /// Evaluate `target_path` into the local cache unless it is there,
    /// returning its module root and, for a project, its evaluated value.
    fn load_local_module(
        &self,
        target_path: &Path,
    ) -> Result<Option<(PathBuf, Option<serde_json::Value>)>> {
        let mut guard = self.local_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire local module cache lock")
        })?;
        if !guard.contains_key(target_path) {
            let module = match self.local_module_from_workspace_cache(target_path) {
                Some(module) => module,
                None => self.evaluate_path_module(target_path)?,
            };
            guard.insert(target_path.to_path_buf(), module);
        }
        Ok(guard.get(target_path).map(|module| {
            let relative = super::relative_path_from_root(&module.root, target_path);
            let value = module
                .get(&relative)
                .filter(|instance| instance.kind == InstanceKind::Project)
                .map(|instance| script_fields(&instance.value));
            (module.root.clone(), value)
        }))
    }

    /// Drop every cached evaluation, after a file CUE reads was rewritten.
    fn forget_local_module(&self, target_path: &Path) -> Result<()> {
        self.local_modules
            .lock()
            .map_err(|_| {
                cuenv_core::Error::configuration("Failed to acquire local module cache lock")
            })?
            .remove(target_path);
        self.workspace_modules
            .lock()
            .map_err(|_| {
                cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
            })?
            .clear();
        Ok(())
    }

    /// Drop every cached evaluation, after a file CUE reads was rewritten
    /// outside of evaluation.
    ///
    /// # Errors
    ///
    /// Returns an error when a module cache lock is poisoned.
    pub fn forget_evaluations(&self) -> Result<()> {
        self.local_modules
            .lock()
            .map_err(|_| {
                cuenv_core::Error::configuration("Failed to acquire local module cache lock")
            })?
            .clear();
        self.workspace_modules
            .lock()
            .map_err(|_| {
                cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
            })?
            .clear();
        Ok(())
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
            let rel_path = super::relative_path_from_root(&module.root, target_path);
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
        let projects = match self.load_workspace_module(&module_root) {
            Ok(projects) => projects,
            Err(error)
                if self.recover_scripts(super::autosync::RecoveryScope::Module(&module_root)) =>
            {
                tracing::debug!(%error, "evaluating again after updating scripts.gen.cue");
                self.load_workspace_module(&module_root)?
            }
            Err(error) => return Err(error),
        };
        let mut stale = false;
        for (project, value) in &projects {
            stale |= self.autosync(&module_root, project, Some(value))?
                == super::autosync::Evaluated::Stale;
        }
        if stale {
            self.workspace_modules
                .lock()
                .map_err(|_| {
                    cuenv_core::Error::configuration(
                        "Failed to acquire workspace module cache lock",
                    )
                })?
                .remove(&module_root);
            self.load_workspace_module(&module_root)?;
        }

        let guard = self.workspace_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
        })?;
        Ok(ModuleGuard {
            guard,
            key: module_root,
        })
    }

    /// Evaluate the whole module into the workspace cache unless it is
    /// there, returning each project's directory and evaluated value.
    fn load_workspace_module(
        &self,
        module_root: &Path,
    ) -> Result<Vec<(PathBuf, serde_json::Value)>> {
        let mut guard = self.workspace_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
        })?;
        if !guard.contains_key(module_root) {
            schema_compat::warn_for_module(module_root)?;
            let module = self.evaluate_workspace_module(module_root)?;
            module.require_unique_project_names()?;
            guard.insert(module_root.to_path_buf(), module);
        }
        Ok(guard.get(module_root).map_or_else(Vec::new, |module| {
            module
                .projects()
                .map(|instance| {
                    (
                        module_root.join(&instance.path),
                        script_fields(&instance.value),
                    )
                })
                .collect()
        }))
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
    /// Returns an error if the module has not been loaded yet (call `module` first).
    pub fn relative_path(&self, target: &Path) -> Result<PathBuf> {
        let root = self.module_root().ok_or_else(|| {
            cuenv_core::Error::configuration("Module not loaded; call module first")
        })?;
        Ok(super::relative_path_from_root(&root, target))
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
                let rel_path = super::relative_path_from_root(&module.root, &target_path);
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
}

/// The part of a project's value that syncing its scripts reads, without
/// copying the rest.
fn script_fields(project: &serde_json::Value) -> serde_json::Value {
    let mut fields = serde_json::Map::new();
    if let Some(directories) = project.get("scriptDirs") {
        fields.insert("scriptDirs".to_string(), directories.clone());
    }
    serde_json::Value::Object(fields)
}
