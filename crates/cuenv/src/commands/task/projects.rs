//! The projects a task run can lead into, and the task scope of a run.
//!
//! A task may depend on a task of another project, reached through that
//! project's import (`#project:task` once loaded). `cuenv task` and
//! `cuenv up` follow those dependencies through one [`TaskScope`], which
//! finds the other projects here: by name among the packages the referring
//! project imports.

use std::path::{Path, PathBuf};

use cuenv_ci::imports::{FilesOwner, ImportsByDirectory};
use cuenv_core::Result;
use cuenv_core::tasks::output_refs::OutputRefDep;
use cuenv_task_exec::{ProjectSource, SourcedProject};

use super::{discovery, list_builder};
use crate::commands::{CommandExecutor, env_file, git_hooks, relative_path_from_root};

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

/// The projects a command's tasks can reach: those the referring project
/// imports, by name.
pub struct ModuleProjects<'source> {
    executor: &'source CommandExecutor,
    contributors: WorkspaceContributors,
    /// What the project in each directory imports, read the first time a
    /// reference of it is bound.
    imports: ImportsByDirectory,
}

impl<'source> ModuleProjects<'source> {
    /// The projects reachable through `executor`'s evaluations.
    pub fn new(executor: &'source CommandExecutor, contributors: WorkspaceContributors) -> Self {
        Self {
            executor,
            contributors,
            imports: ImportsByDirectory::default(),
        }
    }
}

impl ProjectSource for ModuleProjects<'_> {
    fn imported_project(&self, origin: &Path, name: &str) -> std::result::Result<PathBuf, String> {
        // Bound among what the project imports: nothing else is evaluated,
        // so a directory of the module that does not evaluate cannot fail
        // the task.
        let module_root =
            env_file::find_cue_module_root(origin).unwrap_or_else(|| origin.to_path_buf());
        self.imports
            .bind(
                FilesOwner {
                    module_root: &module_root,
                    directory: origin,
                    package: self.executor.package(),
                },
                name,
            )
            .map(|directory| module_root.join(directory))
    }

    fn workspace_root(&self, origin: &Path) -> PathBuf {
        workspace_root(origin)
    }

    fn project_at(&self, root: &Path) -> Result<SourcedProject> {
        // A project is bound through an import of the package cuenv
        // evaluates, so it is read from the evaluation the command holds.
        let package = self.executor.package().to_string();
        let mut manifest = discovery::evaluate_manifest(root, &package, self.executor)?;
        if self.contributors == WorkspaceContributors::Inject {
            list_builder::apply_workspace_contributors(&mut manifest, root);
        }
        Ok(SourcedProject {
            root: root.to_path_buf(),
            module_root: env_file::find_cue_module_root(root).unwrap_or_else(|| root.to_path_buf()),
            manifest,
            output_ref_deps: instance_output_ref_deps(self.executor, root)?,
        })
    }
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

/// The directory a project's inputs are hashed below: the VCS workspace, or
/// the CUE module where there is none.
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
