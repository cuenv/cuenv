//! The projects a task run can lead into, and the task scope of a run.
//!
//! A task may depend on a task of another project (`#project:task`).
//! `cuenv task` and `cuenv up` follow those dependencies through one
//! [`TaskScope`], which finds the other projects here: by name among the
//! module's projects, or by path.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cuenv_core::Result;
use cuenv_core::module::{ModuleEvaluation, NamedProject, declaring_directories};
use cuenv_core::tasks::output_refs::OutputRefDep;
use cuenv_task_exec::{ProjectSource, SourcedProject};

use super::{discovery, list_builder};
use crate::commands::{CommandExecutor, env_file, git_hooks, relative_path_from_root};

/// The directory each project name of one CUE module means, or why its
/// projects could not be listed.
type NamedDirectories = std::result::Result<BTreeMap<String, PathBuf>, String>;

/// Whether the projects a run reaches get the tasks of the workspace
/// contributors (a package manager's install step), as the command gives
/// them to the project it runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceContributors {
    /// `cuenv task` injects them into every project.
    Inject,
    /// `cuenv up` runs tasks as they are declared.
    Omit,
}

/// The projects a command's tasks can reach: those of a CUE module, by name,
/// and any project of the workspace, by path.
pub struct ModuleProjects<'source> {
    executor: &'source CommandExecutor,
    contributors: WorkspaceContributors,
    /// The names of each module a reference was made in, by module root,
    /// listed the first time one of its names is looked up.
    directories: RefCell<BTreeMap<PathBuf, NamedDirectories>>,
}

impl<'source> ModuleProjects<'source> {
    /// The projects reachable through `executor`'s evaluations.
    pub const fn new(
        executor: &'source CommandExecutor,
        contributors: WorkspaceContributors,
    ) -> Self {
        Self {
            executor,
            contributors,
            directories: RefCell::new(BTreeMap::new()),
        }
    }

    /// List the projects of the module `origin` lies in. Naming a project
    /// needs every directory of the module to evaluate: one that does not
    /// could carry the name.
    fn discover(&self, origin: &Path) -> NamedDirectories {
        self.executor
            .discover_all_modules(origin)
            .map(|module| project_directories(&module))
            .map_err(|error| error.to_string())
    }
}

impl ProjectSource for ModuleProjects<'_> {
    fn directory_of(&self, origin: &Path, name: &str) -> Result<Option<PathBuf>> {
        // A name means a project of the module it is written in.
        let module_root =
            env_file::find_cue_module_root(origin).unwrap_or_else(|| origin.to_path_buf());
        let mut directories = self.directories.borrow_mut();
        let named = directories
            .entry(module_root)
            .or_insert_with(|| self.discover(origin));
        match named {
            Ok(named) => Ok(named.get(name).cloned()),
            Err(reason) => Err(cuenv_core::Error::configuration(format!(
                "the projects of this module could not be listed: {reason}"
            ))),
        }
    }

    fn workspace_root(&self, origin: &Path) -> PathBuf {
        workspace_root(origin)
    }

    fn project_at(&self, root: &Path) -> Result<SourcedProject> {
        // A project reached by path may be another package, or another
        // module. One of this module and package is read from the
        // evaluation the command already holds.
        let package = cue_package_name(root)?;
        let other_package;
        let executor = if package == self.executor.package() {
            self.executor
        } else {
            other_package = CommandExecutor::new(package.clone());
            &other_package
        };
        let mut manifest = discovery::evaluate_manifest(root, &package, executor)?;
        if self.contributors == WorkspaceContributors::Inject {
            list_builder::apply_workspace_contributors(&mut manifest, root);
        }
        Ok(SourcedProject {
            root: root.to_path_buf(),
            module_root: env_file::find_cue_module_root(root).unwrap_or_else(|| root.to_path_buf()),
            manifest,
            output_ref_deps: instance_output_ref_deps(executor, root)?,
        })
    }
}

/// The directory each project name of a module means: the shallowest that
/// carries it (see [`declaring_directories`]).
fn project_directories(module: &ModuleEvaluation) -> BTreeMap<String, PathBuf> {
    let named = module.projects().filter_map(|instance| {
        instance.project_name().map(|name| NamedProject {
            path: instance.path.as_path(),
            name,
        })
    });
    declaring_directories(named)
        .into_iter()
        .map(|(name, path)| (name.to_string(), module.root.join(path)))
        .collect()
}

/// The dependencies inferred from output references in the project at
/// `project_root`.
pub fn instance_output_ref_deps(
    executor: &CommandExecutor,
    project_root: &Path,
) -> Result<Vec<OutputRefDep>> {
    let module = executor.module(project_root)?;
    let relative = relative_path_from_root(&module.root, project_root);
    Ok(module
        .get(&relative)
        .map_or_else(Vec::new, |instance| instance.output_ref_deps.clone()))
}

/// The directory a project's inputs are hashed below, and which a
/// path-shaped project reference may not leave: the VCS workspace, or the
/// CUE module where there is none.
pub fn workspace_root(project_root: &Path) -> PathBuf {
    git_hooks::find_git_root(project_root)
        .ok()
        .or_else(|| {
            project_root
                .ancestors()
                .find(|ancestor| ancestor.join(".git").exists())
                .map(Path::to_path_buf)
        })
        .or_else(|| env_file::find_cue_module_root(project_root))
        .unwrap_or_else(|| project_root.to_path_buf())
}

fn cue_package_name(project_root: &Path) -> Result<String> {
    let env_file = project_root.join("env.cue");
    let contents = std::fs::read_to_string(&env_file).map_err(|error| {
        cuenv_core::Error::io_with_path("read referenced project package", env_file.clone(), error)
    })?;
    contents
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("package ").map(str::trim))
        .filter(|package| !package.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "referenced project '{}' has no package declaration in env.cue",
                project_root.display()
            ))
        })
}
