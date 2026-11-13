//! The projects a CI run can reach, and what each runs with.
//!
//! A pipeline task may depend on a task of another project
//! (`#project:task`), in or out of the directory `--path` names. That task
//! runs in its own project: its directory, its configuration, the
//! environment its own hooks and `env` provide. This module finds those
//! projects for the [`TaskScope`](cuenv_task_exec::TaskScope) of a run, and
//! builds each project's hook environment once per run.

use cuenv_core::Result;
use cuenv_core::manifest::Project;
use cuenv_core::module::{NamedProject, declaring_directories};
use cuenv_task_exec::{ProjectSource, SourcedProject};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::OnceCell;

/// The environment a project's hooks and static `env` provide.
pub(super) type HookEnvironment = Arc<BTreeMap<String, String>>;

/// Every project of the module a run was started in, in or out of scope.
pub(super) struct CiProjects {
    module_root: PathBuf,
    /// The projects by directory.
    by_directory: BTreeMap<PathBuf, Project>,
    /// The directory each project name means.
    by_name: BTreeMap<String, PathBuf>,
    /// The checkout the module lies in, looked up once.
    workspace_root: OnceLock<PathBuf>,
    /// Hook environments built so far in this run, by project directory.
    hook_environments: Mutex<HashMap<PathBuf, Arc<OnceCell<HookEnvironment>>>>,
}

impl CiProjects {
    /// The projects of the module rooted at `module_root`, each with its
    /// directory.
    pub(super) fn new(module_root: &Path, projects: &[(PathBuf, Project)]) -> Self {
        let directories: Vec<(PathBuf, &Project)> = projects
            .iter()
            .map(|(path, project)| (canonical(path), project))
            .collect();
        // A name means the shallowest directory that carries it: the rule
        // affected detection resolves dependencies by.
        let by_name =
            declaring_directories(directories.iter().map(|(path, project)| NamedProject {
                path,
                name: &project.name,
            }))
            .into_iter()
            .map(|(name, path)| (name.to_string(), path.to_path_buf()))
            .collect();
        Self {
            module_root: canonical(module_root),
            by_directory: directories
                .into_iter()
                .map(|(path, project)| (path, project.clone()))
                .collect(),
            by_name,
            workspace_root: OnceLock::new(),
            hook_environments: Mutex::default(),
        }
    }

    /// The root of the CUE module the run was started in.
    pub(super) fn module_root(&self) -> &Path {
        &self.module_root
    }

    /// The directory task inputs are hashed below: the checkout, or the
    /// module where there is no repository.
    pub(super) fn workspace_root(&self) -> &Path {
        self.workspace_root.get_or_init(|| {
            gix::discover(&self.module_root)
                .ok()
                .and_then(|repository| repository.workdir().map(canonical))
                .unwrap_or_else(|| self.module_root.clone())
        })
    }

    /// The environment the hooks and static `env` of the project in
    /// `project_root` provide. Hooks run once per project per run, whichever
    /// pipeline reaches the project first.
    ///
    /// # Errors
    ///
    /// Returns an error when a hook fails.
    pub(super) async fn hook_environment(
        &self,
        project: HookProject<'_>,
    ) -> Result<HookEnvironment> {
        let cell = {
            let mut known = self.hook_environments.lock().map_err(|_| {
                cuenv_core::Error::configuration("hook environments are unavailable")
            })?;
            Arc::clone(known.entry(canonical(project.root)).or_default())
        };
        let environment = cell
            .get_or_try_init(|| async {
                super::hook_env::build_hook_environment(
                    project.root,
                    project.config,
                    project.ancestor_configs,
                )
                .await
                .map(Arc::new)
            })
            .await?;
        Ok(Arc::clone(environment))
    }
}

/// The project whose hooks provide an environment.
#[derive(Clone, Copy)]
pub(super) struct HookProject<'project> {
    /// The project's directory.
    pub(super) root: &'project Path,
    pub(super) config: &'project Project,
    /// The projects in scope by directory, whose hooks a project below them
    /// may inherit.
    pub(super) ancestor_configs: &'project HashMap<PathBuf, Project>,
}

impl ProjectSource for CiProjects {
    fn directory_of(&self, _origin: &Path, name: &str) -> Result<Option<PathBuf>> {
        // Every project a CI run knows lies in the one module it runs in.
        Ok(self.by_name.get(name).cloned())
    }

    fn workspace_root(&self, _origin: &Path) -> PathBuf {
        Self::workspace_root(self).to_path_buf()
    }

    fn project_at(&self, root: &Path) -> Result<SourcedProject> {
        let manifest = self.by_directory.get(&canonical(root)).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "'{}' is not a project of the module at '{}'",
                root.display(),
                self.module_root.display()
            ))
        })?;
        Ok(SourcedProject {
            root: root.to_path_buf(),
            module_root: self.module_root.clone(),
            manifest: manifest.clone(),
            output_ref_deps: Vec::new(),
        })
    }
}

/// A directory in its canonical spelling, or as given when it cannot be
/// read.
fn canonical(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
