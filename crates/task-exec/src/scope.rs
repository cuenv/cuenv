//! The tasks one run can reach.
//!
//! A run is asked for tasks of one project. A task there may depend on a task
//! of another project (`#project:task`), which a `#ProjectReference` input
//! implies. A [`TaskScope`] holds the project's own tasks, which a run is
//! asked for by name or by label, and loads the projects their dependencies
//! lead to as it follows them. The tasks of those projects are part of the
//! graph only: they run as dependencies, in their own project's directory,
//! environment and cache identity, and cannot be selected.
//!
//! `cuenv task`, `cuenv ci` and `cuenv up` all build their graphs here, so a
//! dependency on another project means the same thing to each of them. Where
//! the projects come from is a [`ProjectSource`]: today the module's
//! discovered projects, by name or by path.

use crate::workspace::find_workspace_root;
use crate::{
    Input, MappedInput, ProjectReference, Task, TaskGraph, TaskIndex, TaskNode, TaskPath, Tasks,
};
use cuenv_core::contributors::CONTRIBUTOR_TASK_PREFIX;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::output_refs::{OutputRefDep, TaskOutputRef};
use cuenv_core::{Error, Result};
use cuenv_task_graph::{TaskResolution, TaskResolver};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// The name of a task of another project, as a graph spells it:
/// `#project:task`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalTaskName<'name> {
    /// What the project's tasks are qualified with.
    pub project: &'name str,
    /// The task's name within its project.
    pub task: &'name str,
}

impl<'name> ExternalTaskName<'name> {
    /// Read a task name; `None` for a task of the project a run was asked
    /// for, whose names are not qualified.
    #[must_use]
    pub fn parse(name: &'name str) -> Option<Self> {
        let (project, task) = name.strip_prefix('#')?.split_once(':')?;
        (!project.is_empty() && !task.is_empty()).then_some(Self { project, task })
    }

    /// The name as a graph spells it.
    #[must_use]
    pub fn qualified(self) -> String {
        format!("#{}:{}", self.project, self.task)
    }
}

/// A project whose tasks are part of a [`TaskScope`]: what each of its tasks
/// runs with.
#[derive(Debug, Clone)]
pub struct ScopeProject {
    /// The project's directory.
    pub root: PathBuf,
    /// Root of the CUE module the project was evaluated in.
    pub module_root: PathBuf,
    /// The evaluated project: its `env`, `contexts`, `runtime` and hooks.
    pub manifest: Arc<Project>,
}

/// The project a run was asked for.
#[derive(Debug, Clone)]
pub struct RootProject {
    /// The project itself.
    pub project: ScopeProject,
    /// Its tasks by canonical name: what the run can select.
    pub tasks: Tasks,
    /// Dependencies inferred from its tasks' output references.
    pub output_ref_deps: Vec<OutputRefDep>,
}

/// A project as a [`ProjectSource`] evaluated it.
#[derive(Debug, Clone)]
pub struct SourcedProject {
    /// The project's directory.
    pub root: PathBuf,
    /// Root of the CUE module the project was evaluated in.
    pub module_root: PathBuf,
    /// The evaluated project, with whatever contributors the command applies.
    pub manifest: Project,
    /// Dependencies inferred from its tasks' output references.
    pub output_ref_deps: Vec<OutputRefDep>,
}

/// Where the projects that tasks refer to come from.
///
/// A reference names a project (`content`) or spells a path to it
/// (`../content`, or `/content` from the workspace root). A source answers
/// both: which directory a name means, and what the project in a directory
/// is.
pub trait ProjectSource {
    /// The directory of the project a task of the project in `origin`
    /// calls `name`; `None` when the source knows no project by that name.
    /// A name is looked up among the projects of the CUE module `origin`
    /// lies in, and one carried by several directories means the shallowest
    /// of them.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot list those projects, so that
    /// it cannot say whether one of them carries the name.
    fn directory_of(&self, origin: &Path, name: &str) -> Result<Option<PathBuf>>;

    /// The directory a path-shaped reference made in `origin` may not leave:
    /// the VCS workspace, or the CUE module where there is none.
    fn workspace_root(&self, origin: &Path) -> PathBuf;

    /// The project in the directory `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory holds no project or it does not
    /// evaluate.
    fn project_at(&self, root: &Path) -> Result<SourcedProject>;
}

/// The project that owns a task of a [`TaskScope`].
#[derive(Debug, Clone, Copy)]
pub struct TaskOwner<'scope> {
    /// What the project's tasks are qualified with; `None` for the project
    /// the run was asked for.
    pub key: Option<&'scope str>,
    /// The project.
    pub project: &'scope ScopeProject,
    /// The task's name within its project, which is what the project's
    /// contexts and policies match.
    pub task: &'scope str,
}

/// A loaded project other than the one the run was asked for.
#[derive(Debug, Clone)]
struct ExternalProject {
    project: ScopeProject,
    /// Its tasks by their name within the project. The output references
    /// and producers inside them are qualified when the project is loaded,
    /// and the dependencies of each task when the walk reaches it.
    tasks: Tasks,
    /// Contributor steps another loaded project already runs: the name of
    /// each within this project, and the task that runs it.
    shared: BTreeMap<String, String>,
}

/// What a reference to a project resolved to.
#[derive(Debug, Clone)]
struct ResolvedProject {
    /// What its tasks are qualified with; `None` for the run's own project.
    key: Option<String>,
    /// Its directory.
    root: PathBuf,
}

impl ResolvedProject {
    fn task(&self, task: &str) -> String {
        match &self.key {
            Some(project) => ExternalTaskName { project, task }.qualified(),
            None => task.to_string(),
        }
    }
}

/// The tasks a run can reach: see the module documentation.
#[derive(Debug, Clone)]
pub struct TaskScope {
    root: ScopeProject,
    local: Tasks,
    external: BTreeMap<String, ExternalProject>,
    /// What the tasks of the project in a directory are qualified with.
    keys: BTreeMap<PathBuf, String>,
    /// What a reference means as written in a project's directory. A path
    /// means a different project in each directory it is written in.
    declared: BTreeMap<(PathBuf, String), ResolvedProject>,
    output_ref_deps: Vec<OutputRefDep>,
    /// References that could not be followed, as written (`content`), and
    /// why. The tasks that make them fail when a graph reaches them.
    unresolved: BTreeMap<String, String>,
    /// The task that runs a workspace contributor's step, by the workspace
    /// it runs in and the step's name.
    contributors: BTreeMap<(PathBuf, String), String>,
    /// Task names whose dependencies have been followed.
    walked: BTreeSet<String>,
    /// Task names whose node has had its references resolved: on its own,
    /// or with the group or sequence that holds it. A node is resolved
    /// once, while every reference in it is still as its project wrote it.
    resolved: BTreeSet<String>,
}

impl TaskScope {
    /// The scope of a run in `root`, before any dependency is followed.
    #[must_use]
    pub fn new(root: RootProject) -> Self {
        let RootProject {
            project,
            tasks,
            output_ref_deps,
        } = root;
        let mut scope = Self {
            // Directories are compared, and expressed relative to one
            // another, in their canonical spelling.
            root: ScopeProject {
                root: canonical(project.root),
                module_root: canonical(project.module_root),
                manifest: project.manifest,
            },
            local: tasks,
            external: BTreeMap::new(),
            keys: BTreeMap::new(),
            declared: BTreeMap::new(),
            output_ref_deps,
            unresolved: BTreeMap::new(),
            contributors: BTreeMap::new(),
            walked: BTreeSet::new(),
            resolved: BTreeSet::new(),
        };
        if let Some(workspace) = contributor_workspace(&scope.root.root) {
            for name in scope.local.tasks.keys().filter(|name| is_contributor(name)) {
                scope
                    .contributors
                    .insert((workspace.clone(), name.clone()), name.clone());
            }
        }
        scope
    }

    /// Follow the dependencies of the tasks named `names`, loading every
    /// project they lead to.
    ///
    /// A project that cannot be loaded does not fail here: it would stop
    /// tasks that never touch it. The task that refers to it fails when a
    /// graph is built for it (see [`Self::graph`]), with the reason.
    pub fn reach<'name>(
        &mut self,
        names: impl IntoIterator<Item = &'name str>,
        source: &dyn ProjectSource,
    ) {
        let mut pending: Vec<String> = names.into_iter().map(str::to_string).collect();
        while let Some(name) = pending.pop() {
            if !self.walked.insert(name.clone()) {
                continue;
            }
            self.resolve_references(&name, source);
            let Some(node) = self.node(&name) else {
                continue;
            };
            pending.extend(next_names(node, &name));
            pending.extend(
                self.output_ref_deps
                    .iter()
                    .filter(|dependency| dependency.task == name)
                    .map(|dependency| dependency.depends_on.clone()),
            );
        }
    }

    /// Resolve every reference to a project in the node called `name` and
    /// below it, and spell each dependency the one way the graph knows it.
    ///
    /// A node is resolved once, the first time the walk reaches it, on its
    /// own or as part of a group or sequence: every reference in it is then
    /// exactly what its project wrote.
    fn resolve_references(&mut self, name: &str, source: &dyn ProjectSource) {
        let Some(owner) = self.owner_frame(name) else {
            return;
        };
        let Some(survey) = self.survey(name) else {
            return;
        };
        let projects: BTreeMap<String, ResolvedProject> = survey
            .declared
            .iter()
            .filter_map(|project| {
                let target = self.resolve_project(&owner, project, source)?;
                Some((project.clone(), target))
            })
            .collect();
        let Some(node) = self.node_mut(name) else {
            return;
        };
        Rewrite {
            owner: &owner,
            projects: &projects,
            resolved: &survey.resolved,
        }
        .node(name, node);
        self.resolved.extend(survey.resolving);
    }

    /// What resolving the node called `name` has to do: which nodes below
    /// it are still as written, and which projects those refer to.
    fn survey(&self, name: &str) -> Option<Survey> {
        let (tasks, local) = match ExternalTaskName::parse(name) {
            Some(external) => (&self.external.get(external.project)?.tasks, external.task),
            None => (&self.local, name),
        };
        let mut survey = Survey::default();
        self.survey_node(
            SurveyedNode {
                tasks,
                local: local.to_string(),
                name: name.to_string(),
                node: tasks.node(local)?,
            },
            &mut survey,
        );
        Some(survey)
    }

    fn survey_node(&self, surveyed: SurveyedNode<'_>, survey: &mut Survey) {
        let SurveyedNode {
            tasks,
            local,
            name,
            node,
        } = surveyed;
        // A group's children are also indexed under their own names, as
        // copies. The name means the copy; the child inside the group is
        // reached only through the group, and resolved with it.
        let named = tasks
            .node(&local)
            .is_some_and(|by_name| std::ptr::eq(by_name, node));
        if named && self.resolved.contains(&name) {
            survey.resolved.insert(name);
            return;
        }
        if named {
            survey.resolving.push(name.clone());
        }
        let child = |suffix: String, node| SurveyedNode {
            tasks,
            local: format!("{local}{suffix}"),
            name: format!("{name}{suffix}"),
            node,
        };
        match node {
            TaskNode::Task(task) => {
                survey
                    .declared
                    .extend(projects_in_dependencies(&task.depends_on));
                survey.declared.extend(
                    task.iter_project_refs()
                        .map(|reference| reference.project.clone()),
                );
            }
            TaskNode::Group(group) => {
                survey
                    .declared
                    .extend(projects_in_dependencies(&group.depends_on));
                for (child_name, child_node) in &group.children {
                    self.survey_node(child(format!(".{child_name}"), child_node), survey);
                }
            }
            TaskNode::Sequence(steps) => {
                for (index, step) in steps.iter().enumerate() {
                    self.survey_node(child(format!("[{index}]"), step), survey);
                }
            }
        }
    }

    /// The graph that runs the tasks named `names` after everything they
    /// depend on, in this project or another.
    ///
    /// # Errors
    ///
    /// Returns an error when a dependency is missing or cyclic, or when a
    /// name was never passed to [`Self::reach`]. A dependency on a project
    /// that could not be loaded is reported with the reason.
    pub fn graph<'name>(&self, names: impl IntoIterator<Item = &'name str>) -> Result<TaskGraph> {
        let mut graph = TaskGraph::new();
        for name in names {
            // Only a task the walk reached has its dependencies spelled the
            // way the graph knows them.
            if !self.walked.contains(name) {
                return Err(Error::configuration(format!(
                    "Task '{name}' is outside the scope of this run"
                )));
            }
            graph
                .build_for_task(name, self)
                .map_err(|error| self.explain(error))?;
        }
        graph
            .add_output_ref_deps(&self.output_ref_deps, self)
            .map_err(|error| self.explain(error))?;
        Ok(graph)
    }

    /// The tasks of the project the run was asked for: the only ones a run
    /// can select, by name or by label.
    #[must_use]
    pub const fn local(&self) -> &Tasks {
        &self.local
    }

    /// The project the run was asked for.
    #[must_use]
    pub const fn root(&self) -> &ScopeProject {
        &self.root
    }

    /// The other projects loaded so far, by what their tasks are qualified
    /// with.
    pub fn external_projects(&self) -> impl Iterator<Item = (&str, &ScopeProject)> {
        self.external
            .iter()
            .map(|(key, external)| (key.as_str(), &external.project))
    }

    /// The project that owns the task a graph calls `name`; `None` for a
    /// task of a project this scope never loaded.
    #[must_use]
    pub fn owner<'scope>(&'scope self, name: &'scope str) -> Option<TaskOwner<'scope>> {
        match ExternalTaskName::parse(name) {
            Some(external) => {
                let (key, loaded) = self.external.get_key_value(external.project)?;
                Some(TaskOwner {
                    key: Some(key.as_str()),
                    project: &loaded.project,
                    task: external.task,
                })
            }
            None => Some(TaskOwner {
                key: None,
                project: &self.root,
                task: name,
            }),
        }
    }

    /// The node a graph calls `name`, in this project or another.
    #[must_use]
    pub fn node(&self, name: &str) -> Option<&TaskNode> {
        match ExternalTaskName::parse(name) {
            Some(external) => self
                .external
                .get(external.project)?
                .tasks
                .node(external.task),
            None => self.local.node(name),
        }
    }

    /// Dependencies inferred from output references, in every loaded
    /// project.
    #[must_use]
    pub fn output_ref_deps(&self) -> &[OutputRefDep] {
        &self.output_ref_deps
    }

    fn node_mut(&mut self, name: &str) -> Option<&mut TaskNode> {
        match ExternalTaskName::parse(name) {
            Some(external) => self
                .external
                .get_mut(external.project)?
                .tasks
                .node_mut(external.task),
            None => self.local.node_mut(name),
        }
    }

    fn owner_frame(&self, name: &str) -> Option<OwnerFrame> {
        let owner = self.owner(name)?;
        Some(OwnerFrame {
            key: owner.key.map(str::to_string),
            root: owner.project.root.clone(),
            module_root: owner.project.module_root.clone(),
            shared: owner
                .key
                .and_then(|key| self.external.get(key))
                .map(|external| external.shared.clone())
                .unwrap_or_default(),
        })
    }

    /// The project a task of `owner` means by `declared`, loading it when it
    /// is new. A reference that cannot be followed is recorded and `None`.
    fn resolve_project(
        &mut self,
        owner: &OwnerFrame,
        declared: &str,
        source: &dyn ProjectSource,
    ) -> Option<ResolvedProject> {
        let written = (owner.root.clone(), declared.to_string());
        if let Some(known) = self.declared.get(&written) {
            return Some(known.clone());
        }
        match self.locate_and_load(owner, declared, source) {
            Ok(target) => {
                self.declared.insert(written, target.clone());
                Some(target)
            }
            Err(error) => {
                tracing::debug!(project = declared, %error, "cross-project reference not followed");
                self.unresolved
                    .insert(declared.to_string(), error.to_string());
                None
            }
        }
    }

    fn locate_and_load(
        &mut self,
        owner: &OwnerFrame,
        declared: &str,
        source: &dyn ProjectSource,
    ) -> Result<ResolvedProject> {
        let located = locate_project(owner, declared, source)?;
        let root = located.root().to_path_buf();
        if root == self.root.root {
            return Ok(ResolvedProject { key: None, root });
        }
        let key = match self.keys.get(&root) {
            Some(key) => key.clone(),
            None => {
                let key = self.key_for(&located, declared);
                self.merge(source.project_at(&root)?, &key)?;
                key
            }
        };
        Ok(ResolvedProject {
            key: Some(key),
            root,
        })
    }

    /// Add a project's tasks to the graph-only part of the scope, qualified
    /// with `key`.
    fn merge(&mut self, sourced: SourcedProject, key: &str) -> Result<()> {
        let SourcedProject {
            root,
            module_root,
            manifest,
            output_ref_deps,
        } = sourced;
        let root = canonical(root);
        let module_root = canonical(module_root);
        let mut tasks = TaskIndex::build(&manifest.tasks)?.to_tasks();

        // A workspace's install step belongs to the workspace, not to each
        // member: two members of one workspace share the task that runs it.
        let mut shared = BTreeMap::new();
        if let Some(workspace) = contributor_workspace(&root) {
            for name in tasks.tasks.keys().filter(|name| is_contributor(name)) {
                let step = (workspace.clone(), name.clone());
                match self.contributors.get(&step) {
                    Some(owner) => {
                        shared.insert(name.clone(), owner.clone());
                    }
                    None => {
                        let task = ExternalTaskName {
                            project: key,
                            task: name,
                        };
                        self.contributors.insert(step, task.qualified());
                    }
                }
            }
        }
        tasks.tasks.retain(|name, _| !shared.contains_key(name));

        let qualifier = Qualifier {
            key,
            root: &root,
            shared: &shared,
        };
        for node in tasks.tasks.values_mut() {
            qualifier.node(node);
        }
        self.output_ref_deps
            .extend(output_ref_deps.iter().map(|dependency| {
                OutputRefDep::new(
                    qualifier.name(&dependency.task),
                    qualifier.name(&dependency.depends_on),
                )
            }));
        self.keys.insert(root.clone(), key.to_string());
        self.external.insert(
            key.to_string(),
            ExternalProject {
                project: ScopeProject {
                    root,
                    module_root,
                    manifest: Arc::new(manifest),
                },
                tasks,
                shared,
            },
        );
        Ok(())
    }

    /// What to qualify a newly located project's tasks with.
    ///
    /// A project found by its name is qualified with that name, which means
    /// the same project wherever it is written. One found by a path is
    /// qualified with its path from the workspace root (`/services/api`),
    /// which is how any project of the workspace may spell it.
    fn key_for(&self, located: &LocatedProject, declared: &str) -> String {
        let key = match located {
            LocatedProject::ByName(_) => declared.to_string(),
            // A task name is split from its project at the first colon,
            // so the project's part holds none.
            LocatedProject::ByPath { root, workspace } => {
                format!("/{}", forward_slashes(&relative_from(workspace, root))).replace(':', "_")
            }
        };
        if !self.external.contains_key(&key) {
            return key;
        }
        (2_usize..)
            .map(|attempt| format!("{key}~{attempt}"))
            .find(|candidate| !self.external.contains_key(candidate))
            .unwrap_or(key)
    }

    /// Add to a graph error why the project a missing dependency names
    /// could not be loaded.
    fn explain(&self, error: Error) -> Error {
        let message = error.to_string();
        let reasons: Vec<String> = self
            .unresolved
            .iter()
            .filter(|(project, _)| message.contains(&format!("#{project}:")))
            .map(|(project, reason)| format!("Project '{project}' could not be loaded: {reason}"))
            .collect();
        if reasons.is_empty() {
            return error;
        }
        Error::configuration(format!("{message}\n\n{}", reasons.join("\n")))
    }
}

impl TaskResolver<Task> for TaskScope {
    fn resolve(&self, name: &str) -> Option<TaskResolution<Task>> {
        self.node(name).map(|node| node.resolution(name))
    }
}

/// The project that owns a task being resolved: what its references are
/// relative to, what its mapped inputs are relative to, and what the names
/// of its own tasks are qualified with.
#[derive(Debug, Clone)]
struct OwnerFrame {
    /// What the project's tasks are qualified with; `None` for the run's own
    /// project, whose names are not qualified.
    key: Option<String>,
    root: PathBuf,
    module_root: PathBuf,
    /// Contributor steps another loaded project already runs, by name.
    shared: BTreeMap<String, String>,
}

impl OwnerFrame {
    /// The name the graph knows a task of this project by.
    fn task(&self, local: &str) -> String {
        let Some(key) = &self.key else {
            return local.to_string();
        };
        self.shared.get(local).cloned().unwrap_or_else(|| {
            ExternalTaskName {
                project: key,
                task: local,
            }
            .qualified()
        })
    }
}

/// What resolving one node has to do.
#[derive(Debug, Default)]
struct Survey {
    /// The projects the nodes still as written refer to.
    declared: BTreeSet<String>,
    /// Nodes below the surveyed one that are already resolved.
    resolved: BTreeSet<String>,
    /// Names that mean a node this resolves.
    resolving: Vec<String>,
}

/// A node of the tree being surveyed.
struct SurveyedNode<'node> {
    /// The tasks of the project that owns the node.
    tasks: &'node Tasks,
    /// The node's name within its project.
    local: String,
    /// The node's name in the graph.
    name: String,
    node: &'node TaskNode,
}

/// A project's directory, and how a reference to it found it.
#[derive(Debug, Clone)]
enum LocatedProject {
    /// The reference is the project's name.
    ByName(PathBuf),
    /// The reference is a path, within `workspace`.
    ByPath { root: PathBuf, workspace: PathBuf },
}

impl LocatedProject {
    fn root(&self) -> &Path {
        match self {
            Self::ByName(root) | Self::ByPath { root, .. } => root,
        }
    }
}

/// The directory a task of `owner` means by `declared`: a project's name
/// first, then a path.
///
/// A reference that could be a name is looked up as one before anything
/// else. When the names cannot be listed it fails: a directory spelled the
/// same way may not be the project the name means. A reference that can
/// only be a path (`../content`, `/content`) needs no listing at all.
fn locate_project(
    owner: &OwnerFrame,
    declared: &str,
    source: &dyn ProjectSource,
) -> Result<LocatedProject> {
    if may_be_a_name(declared) {
        let named = source
            .directory_of(&owner.root, declared)
            .map_err(|error| {
                Error::configuration(format!(
                    "the project named '{declared}' cannot be looked up: {error}"
                ))
            })?;
        if let Some(directory) = named {
            return std::fs::canonicalize(&directory)
                .map(LocatedProject::ByName)
                .map_err(|error| {
                    Error::io_with_path("canonicalize referenced project", directory, error)
                });
        }
    }
    locate_by_path(owner, declared, source)
}

/// Whether a reference could be a project's name: anything that is not
/// spelled as a path.
fn may_be_a_name(declared: &str) -> bool {
    !declared.is_empty() && !declared.contains(['/', '\\']) && declared != "." && declared != ".."
}

/// Resolve a path-shaped reference: relative to the project that makes it,
/// or else to that project's CUE module.
///
/// A leading slash means "from the workspace root", not the host root.
/// Canonicalization both normalizes `..` and keeps a symlink from leaving
/// the workspace.
fn locate_by_path(
    owner: &OwnerFrame,
    declared: &str,
    source: &dyn ProjectSource,
) -> Result<LocatedProject> {
    let workspace_root = source.workspace_root(&owner.root);
    let workspace = std::fs::canonicalize(&workspace_root).map_err(|error| {
        Error::io_with_path(
            "canonicalize cross-project workspace",
            workspace_root,
            error,
        )
    })?;
    let path = Path::new(declared);
    let candidates = if path.is_absolute() {
        let relative: PathBuf = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part),
                Component::ParentDir => Some(std::ffi::OsStr::new("..")),
                Component::Prefix(_) | Component::RootDir | Component::CurDir => None,
            })
            .collect();
        vec![workspace.join(relative)]
    } else {
        vec![owner.root.join(path), owner.module_root.join(path)]
    };
    let mut missing = None;
    for candidate in candidates {
        match std::fs::canonicalize(&candidate) {
            Ok(root) if root.starts_with(&workspace) => {
                return Ok(LocatedProject::ByPath { root, workspace });
            }
            Ok(_) => {
                return Err(Error::configuration(format!(
                    "cross-project reference '{declared}' resolves outside workspace '{}'",
                    workspace.display()
                )));
            }
            Err(error) => {
                missing.get_or_insert(Error::io_with_path(
                    "canonicalize referenced project",
                    candidate,
                    error,
                ));
            }
        }
    }
    Err(missing.unwrap_or_else(|| {
        Error::configuration(format!(
            "cross-project reference '{declared}' names no path"
        ))
    }))
}

/// The projects a list of dependencies names (`#project:task`).
fn projects_in_dependencies(dependencies: &[crate::TaskDependency]) -> Vec<String> {
    dependencies
        .iter()
        .filter_map(|dependency| ExternalTaskName::parse(dependency.task_name()))
        .map(|external| external.project.to_string())
        .collect()
}

/// The names a graph follows from the node called `name`: its dependencies,
/// or the children of a group or sequence.
fn next_names(node: &TaskNode, name: &str) -> Vec<String> {
    match node.resolution(name) {
        TaskResolution::Single(task) => task.dependency_names().map(str::to_string).collect(),
        TaskResolution::Sequential { children } => children,
        TaskResolution::Parallel {
            mut children,
            depends_on,
        } => {
            children.extend(depends_on);
            children
        }
    }
}

/// Resolves the references of one node, and of everything below it that is
/// still as its project wrote it.
struct Rewrite<'rewrite> {
    owner: &'rewrite OwnerFrame,
    /// What each project the nodes refer to resolved to.
    projects: &'rewrite BTreeMap<String, ResolvedProject>,
    /// Nodes that are already resolved, by their name in the graph.
    resolved: &'rewrite BTreeSet<String>,
}

impl Rewrite<'_> {
    fn node(&self, name: &str, node: &mut TaskNode) {
        if self.resolved.contains(name) {
            return;
        }
        match node {
            TaskNode::Task(task) => self.task(task),
            TaskNode::Group(group) => {
                self.dependencies(&mut group.depends_on);
                for (child_name, child) in &mut group.children {
                    self.node(&format!("{name}.{child_name}"), child);
                }
            }
            TaskNode::Sequence(steps) => {
                for (index, step) in steps.iter_mut().enumerate() {
                    self.node(&format!("{name}[{index}]"), step);
                }
            }
        }
    }

    fn task(&self, task: &mut Task) {
        self.dependencies(&mut task.depends_on);
        task.inputs = std::mem::take(&mut task.inputs)
            .into_iter()
            .flat_map(|input| match input {
                Input::Project(reference) => self.mapped(reference),
                other => vec![other],
            })
            .collect();
    }

    /// Spell every dependency the one way the graph knows it: a task of
    /// another project by what that project is qualified with, however the
    /// reference was written, and a task of the owner's project by what the
    /// owner is qualified with.
    fn dependencies(&self, dependencies: &mut [crate::TaskDependency]) {
        for dependency in dependencies {
            let spelled = match ExternalTaskName::parse(&dependency.name) {
                Some(external) => self
                    .projects
                    .get(external.project)
                    .map(|target| target.task(external.task)),
                None => Some(self.owner.task(&dependency.name)),
            };
            if let Some(spelled) = spelled {
                dependency.name = spelled;
            }
        }
    }

    /// The files a `#ProjectReference` stands for: each `from`, below the
    /// project that produces it, staged at `to` below the project that owns
    /// the consuming task. Left as written when its project did not load.
    fn mapped(&self, reference: ProjectReference) -> Vec<Input> {
        let Some(target) = self.projects.get(&reference.project) else {
            return vec![Input::Project(reference)];
        };
        let source = relative_from(&self.owner.module_root, &target.root);
        let destination = relative_from(&self.owner.module_root, &self.owner.root);
        let task = TaskPath::parse(&reference.task)
            .map_or_else(|_| reference.task.clone(), |path| path.canonical());
        let producer = target.task(&task);
        reference
            .map
            .iter()
            .map(|mapping| {
                Input::Mapped(MappedInput {
                    source: forward_slashes(&source.join(&mapping.from)),
                    destination: forward_slashes(&destination.join(&mapping.to)),
                    producer_task: Some(producer.clone()),
                })
            })
            .collect()
    }
}

/// Marks the tasks of a project other than the run's own as that project's,
/// and qualifies the task names inside them that are not dependencies, so
/// that none can be read as a task of the run's project. Dependencies are
/// spelled when the walk reaches the task that declares them.
struct Qualifier<'qualifier> {
    key: &'qualifier str,
    root: &'qualifier Path,
    /// Contributor steps another loaded project already runs, by name.
    shared: &'qualifier BTreeMap<String, String>,
}

impl Qualifier<'_> {
    fn name(&self, local: &str) -> String {
        if local.starts_with('#') {
            return local.to_string();
        }
        self.shared.get(local).cloned().unwrap_or_else(|| {
            ExternalTaskName {
                project: self.key,
                task: local,
            }
            .qualified()
        })
    }

    fn node(&self, node: &mut TaskNode) {
        match node {
            TaskNode::Task(task) => self.task(task),
            TaskNode::Group(group) => {
                for child in group.children.values_mut() {
                    self.node(child);
                }
            }
            TaskNode::Sequence(steps) => {
                for step in steps {
                    self.node(step);
                }
            }
        }
    }

    fn task(&self, task: &mut Task) {
        for argument in &mut task.args {
            if let Some(qualified) = self.output_reference(argument) {
                *argument = qualified;
            }
        }
        for value in task.env.values_mut() {
            if let Some(qualified) = value.as_str().and_then(|text| self.output_reference(text)) {
                *value = serde_json::Value::String(qualified);
            }
        }
        for input in &mut task.inputs {
            if let Input::Mapped(mapping) = input
                && let Some(producer) = &mapping.producer_task
            {
                mapping.producer_task = Some(self.name(producer));
            }
        }
        task.project_root = Some(self.root.to_path_buf());
    }

    /// An output-reference placeholder with its task qualified; `None` for
    /// any other string.
    fn output_reference(&self, value: &str) -> Option<String> {
        let mut reference = TaskOutputRef::parse(value)?;
        reference.task = self.name(&reference.task);
        Some(reference.to_placeholder())
    }
}

/// Whether a task, by its canonical name, is a workspace contributor's.
fn is_contributor(name: &str) -> bool {
    name.starts_with(&CONTRIBUTOR_TASK_PREFIX.replace(':', "."))
}

/// The workspace a project's contributor steps run in: the root of the
/// package-manager workspace the project is a member of.
fn contributor_workspace(project_root: &Path) -> Option<PathBuf> {
    let manager = *cuenv_workspaces::detect_package_managers(project_root)
        .ok()?
        .first()?;
    Some(find_workspace_root(manager, project_root))
}

/// `target` as a path from `base`, with `..` where it lies outside. Both are
/// absolute.
fn relative_from(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<Component<'_>> = base.components().collect();
    let target: Vec<Component<'_>> = target.components().collect();
    let shared = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    base[shared..]
        .iter()
        .map(|_| Component::ParentDir)
        .chain(target[shared..].iter().copied())
        .collect()
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A directory in its canonical spelling, or as given when it cannot be
/// read (the commands that run in it report that).
fn canonical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
