use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskNode;
use cuenv_core::{AffectedBy, PatternMatch, UnmatchablePattern};
use cuenv_task_exec::TaskIndex;
use cuenv_vcs::WalkHasher;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The frame changed files are reported in: the root of the checkout.
///
/// A provider reports changed files relative to the root of the checkout,
/// and a task's inputs are relative to its project. To compare them, a
/// project's directory is placed in the checkout by an exact prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutFrame {
    /// Canonical, so a project reached through a symlink is still placed.
    root: PathBuf,
}

impl CheckoutFrame {
    /// The checkout that holds `directory`: the root of its git worktree,
    /// or `directory` itself outside a repository, where no file is
    /// reported as changed and the frame only has to be consistent.
    #[must_use]
    pub fn around(directory: &Path) -> Self {
        let root = crate::provider::git::Checkout::around(directory)
            .and_then(|checkout| checkout.root())
            .unwrap_or_else(|| directory.to_path_buf());
        Self::at(&root)
    }

    /// The frame of a checkout whose root is known.
    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self {
            root: canonical(root),
        }
    }

    /// The root of the checkout.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `directory` relative to the root of the checkout, empty for the root
    /// itself; `None` when the directory is not inside the checkout.
    #[must_use]
    pub fn place(&self, directory: &Path) -> Option<PathBuf> {
        canonical(directory)
            .strip_prefix(&self.root)
            .ok()
            .map(Path::to_path_buf)
    }
}

/// The canonical form of a path, or the path as given when it cannot be
/// resolved (it does not exist, as in a dry description of a checkout).
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Changed files and the project whose inputs they are compared with, in
/// one frame: both relative to the root of the checkout.
#[derive(Clone, Copy)]
pub struct ChangedInputs<'changes> {
    /// Files changed for the current event.
    pub files: &'changes [PathBuf],
    /// The project's directory (see [`CheckoutFrame::place`]); `.` or empty
    /// for a project at the root of the checkout.
    pub project_root: &'changes Path,
}

/// Project data used to resolve direct and cross-project dependencies.
pub struct AffectedProjects<'projects, S> {
    /// Project whose pipeline tasks are being selected.
    pub project: &'projects Project,
    /// Workspace projects available to external task references, each with
    /// its directory relative to the root of the checkout.
    pub all_projects: &'projects HashMap<String, (PathBuf, Project), S>,
    /// The root of the checkout, when the files of a project can be looked
    /// at. It is only used to tell a root-relative input someone relies on
    /// from an accident of names (see [`PatternMatch::OnlyFromCheckoutRoot`]).
    pub checkout_root: Option<&'projects Path>,
    /// The directories, relative to the root of the checkout, of the
    /// projects whose CUE configuration a changed file can change (see
    /// [`crate::configuration`]). Every task of such a project is affected.
    pub changed_configuration: Option<&'projects HashSet<PathBuf>>,
}

struct ExternalDependencyContext<'context, S> {
    projects: &'context HashMap<String, (PathBuf, Project), S>,
    checkout_root: Option<&'context Path>,
    changed_configuration: Option<&'context HashSet<PathBuf>>,
    changed_files: &'context [PathBuf],
    cache: &'context mut HashMap<String, bool>,
    /// Dependencies whose check has started and not finished. Meeting one
    /// again means the dependencies form a cycle.
    visiting: &'context mut HashSet<String>,
    index_cache: &'context mut HashMap<String, Option<TaskIndex>>,
}

/// Return pipeline tasks affected by changed files or their dependencies.
///
/// Results follow the pipeline's task order. Whatever cannot be shown to be
/// unaffected is reported as affected, with a warning: a task or dependency
/// that does not resolve, an external project that is unknown, and inputs
/// that cannot be compared with changed files.
#[must_use]
pub fn compute_affected_tasks<S>(
    changed: ChangedInputs<'_>,
    pipeline_tasks: &[String],
    projects: &AffectedProjects<'_, S>,
) -> Vec<String>
where
    S: BuildHasher,
{
    let ChangedInputs {
        files: changed_files,
        project_root,
    } = changed;
    let AffectedProjects {
        project: config,
        all_projects,
        checkout_root,
        changed_configuration,
    } = projects;
    tracing::trace!(
        project_root = %project_root.display(),
        pipeline_tasks = ?pipeline_tasks,
        changed_files = ?changed_files,
        "Computing affected tasks for CI pipeline"
    );

    // A task's command, inputs and dependencies are computed from CUE. When
    // that CUE changed, no declared input says which tasks changed with it.
    if configuration_changed(*changed_configuration, project_root) {
        tracing::info!(
            project = %display_project(project_root),
            "The project's CUE configuration changed; every pipeline task is selected"
        );
        return pipeline_tasks.to_vec();
    }

    // A directory below a project inherits the project's name and is
    // planned on its own. Its inherited inputs are written for the
    // directory that declares the name, so the root-relative reading is no
    // sign of a mistake there.
    let warning_root = checkout_root.filter(|_| {
        all_projects
            .get(&config.name)
            .is_none_or(|(declaring, _)| declaring == project_root)
    });

    let mut affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();
    let mut visiting_external: HashSet<String> = HashSet::new();
    let mut external_index_cache: HashMap<String, Option<TaskIndex>> = HashMap::new();

    // Build task index for resolving nested task names
    let index = match TaskIndex::build(&config.tasks) {
        Ok(index) => index,
        Err(error) => {
            // Fail closed: without an index nothing can be shown to be
            // unaffected, and reporting "no affected tasks" would turn CI
            // green without running anything.
            tracing::warn!(
                %error,
                "Failed to build task index; treating every pipeline task as affected"
            );
            return pipeline_tasks.to_vec();
        }
    };

    // Expand pipeline tasks to the full DAG so every dependency is evaluated
    let all_dag_tasks = collect_dag_tasks(pipeline_tasks, &index);
    tracing::trace!(all_dag_tasks = ?all_dag_tasks, "Expanded pipeline task DAG");

    // 1. Identify directly affected tasks across the entire DAG
    for task_name in &all_dag_tasks {
        let directly_affected = if let Ok(entry) = index.resolve(task_name) {
            warn_about_unmatchable_inputs(project_root, task_name, &entry.node);
            warn_about_root_relative_inputs(
                changed,
                warning_root,
                NamedNode {
                    name: task_name,
                    node: &entry.node,
                },
            );
            entry.node.is_affected_by(changed_files, project_root)
        } else {
            // The executor reports the missing task; selecting it here is
            // what lets that error surface.
            warn_once(&format!(
                "task '{task_name}' of project {} does not resolve, so it cannot be shown to \
                 be unaffected and is selected",
                display_project(project_root)
            ));
            true
        };
        tracing::trace!(
            task = task_name,
            directly_affected,
            "Evaluated direct task affect"
        );
        if directly_affected {
            affected.insert(task_name.clone());
        }
    }

    // 2. Propagate affected status through dependencies (fix-point loop)
    // A task is transitively affected if any of its dependencies are affected.
    let mut changed = true;
    while changed {
        changed = false;
        for task_name in &all_dag_tasks {
            if affected.contains(task_name) {
                continue;
            }
            let Ok(entry) = index.resolve(task_name) else {
                continue;
            };

            let affected_dependency =
                node_dependency_names(&entry.node)
                    .into_iter()
                    .find(|dep_name| {
                        if dep_name.starts_with('#') {
                            check_external_dependency(
                                &named_dependency(
                                    dep_name,
                                    DependencyOrigin {
                                        project_root,
                                        projects: all_projects,
                                    },
                                ),
                                &mut ExternalDependencyContext {
                                    projects: all_projects,
                                    checkout_root: *checkout_root,
                                    changed_configuration: *changed_configuration,
                                    changed_files,
                                    cache: &mut visited_external_cache,
                                    visiting: &mut visiting_external,
                                    index_cache: &mut external_index_cache,
                                },
                            )
                        } else {
                            affected.contains(dep_name)
                        }
                    });

            if let Some(dependency) = affected_dependency {
                tracing::trace!(
                    task = task_name,
                    dependency,
                    "Marked task transitively affected via dependency"
                );
                affected.insert(task_name.clone());
                changed = true;
            }
        }
    }

    // Return in pipeline order, filtered to pipeline tasks only.
    // The executor handles dependency resolution when running each task.
    let result: Vec<String> = pipeline_tasks
        .iter()
        .filter(|t| affected.contains(*t))
        .cloned()
        .collect();
    tracing::trace!(affected_tasks = ?result, "Affected task computation complete");
    result
}

/// Every dependency a node declares, on itself or on anything beneath it.
///
/// A group or a sequence is selected as a whole, so what any of its members
/// depends on can affect it: the group's own `dependsOn`, and those of every
/// child, step and nested group.
fn node_dependency_names(node: &TaskNode) -> BTreeSet<String> {
    let own = node
        .depends_on()
        .iter()
        .map(|dependency| dependency.task_name().to_string());
    let nested: Vec<&TaskNode> = match node {
        TaskNode::Task(_) => Vec::new(),
        TaskNode::Group(group) => group.children.values().collect(),
        TaskNode::Sequence(steps) => steps.iter().collect(),
    };
    own.chain(nested.into_iter().flat_map(node_dependency_names))
        .collect()
}

/// Walk the dependency graph from `roots` and collect every task reachable
/// through `depends_on`, including the roots themselves, in a stable order.
fn collect_dag_tasks(roots: &[String], index: &TaskIndex) -> Vec<String> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();

    for root in roots {
        if visited.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }

    while let Some(task_name) = queue.pop_front() {
        let Ok(entry) = index.resolve(&task_name) else {
            continue;
        };
        for dep_name in node_dependency_names(&entry.node) {
            // Only follow internal dependencies; external (#project:task) are
            // handled separately during propagation.
            if !dep_name.starts_with('#') && visited.insert(dep_name.clone()) {
                queue.push_back(dep_name);
            }
        }
    }

    visited.into_iter().collect()
}

/// Return the input patterns of `task_name` that match the changed files.
///
/// Returns an empty list when the task or its task index cannot be resolved.
/// For a group or a sequence, the patterns are those of every task beneath it.
#[must_use]
pub fn matched_inputs_for_task(
    task_name: &str,
    config: &Project,
    changed: ChangedInputs<'_>,
) -> Vec<String> {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return Vec::new();
    };

    let Ok(entry) = index.resolve(task_name) else {
        return Vec::new();
    };

    let mut matched: Vec<String> = Vec::new();
    for pattern in entry.node.input_patterns() {
        if cuenv_core::matches_pattern(changed.files, changed.project_root, pattern)
            && !matched.iter().any(|seen| seen == pattern)
        {
            matched.push(pattern.to_owned());
        }
    }
    matched
}

/// Check if a task is directly affected by file changes.
///
/// Uses the [`AffectedBy`] trait implementation from cuenv-core, which handles:
/// - Single tasks: affected if any input pattern matches changed files
/// - Task groups: affected if ANY subtask is affected
/// - Tasks with no inputs: always considered affected (safe default)
///
/// This function uses `TaskIndex` to resolve nested task names like "deploy.preview"
/// which are stored in CUE as hierarchical structures (e.g., `deploy: preview: {...}`).
#[cfg(test)]
fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed: ChangedInputs<'_>,
) -> bool {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return false;
    };

    index.resolve(task_name).is_ok_and(|entry| {
        entry
            .node
            .is_affected_by(changed.files, changed.project_root)
    })
}

/// Check if an external dependency (cross-project task) is affected by file changes.
///
/// External dependencies are specified in the format `#project:task`. This function
/// recursively checks if the referenced task or any of its dependencies are affected.
///
/// It fails closed. A reference that is malformed, names an unknown project
/// or a task that project does not have, or whose project's tasks cannot be
/// indexed, cannot be shown to be unaffected: it counts as affected, with a
/// warning. So does a dependency cycle, which is met when a dependency is
/// reached again while its own check is still running.
fn check_external_dependency<S>(dep: &str, context: &mut ExternalDependencyContext<'_, S>) -> bool
where
    S: BuildHasher,
{
    // dep format: "#project:task"; task may itself contain ':' for nested paths.
    if let Some(result) = context.cache.get(dep) {
        return *result;
    }

    if !context.visiting.insert(dep.to_string()) {
        warn_once(&format!(
            "dependency '{dep}' is part of a cycle, so it cannot be shown to be unaffected and \
             counts as affected"
        ));
        return true;
    }

    let result = external_dependency_is_affected(dep, context);
    context.visiting.remove(dep);
    context.cache.insert(dep.to_string(), result);
    result
}

fn external_dependency_is_affected<S>(
    dep: &str,
    context: &mut ExternalDependencyContext<'_, S>,
) -> bool
where
    S: BuildHasher,
{
    let Some((project_name, task_name)) = dep
        .strip_prefix('#')
        .and_then(|reference| reference.split_once(':'))
        .filter(|(project, task)| !project.is_empty() && !task.is_empty())
    else {
        warn_once(&format!(
            "dependency '{dep}' is not of the form '#project:task', so it cannot be shown to \
             be unaffected and counts as affected"
        ));
        return true;
    };

    let projects = context.projects;
    let Some((project_path, project_config)) = projects.get(project_name) else {
        warn_once(&format!(
            "dependency '{dep}' names project '{project_name}', which is not in this module, \
             so it cannot be shown to be unaffected and counts as affected"
        ));
        return true;
    };

    // The producer's tasks are computed from its own CUE, which the consumer
    // does not import when it names the producer by `#project:task`.
    if configuration_changed(context.changed_configuration, project_path) {
        return true;
    }

    let dependency_names = {
        let Some(index) = cached_task_index(context.index_cache, project_name, project_config)
        else {
            warn_once(&format!(
                "the tasks of project '{project_name}' cannot be indexed, so dependency \
                 '{dep}' cannot be shown to be unaffected and counts as affected"
            ));
            return true;
        };

        let Ok(entry) = index.resolve(task_name) else {
            warn_once(&format!(
                "dependency '{dep}' names a task project '{project_name}' does not have, so \
                 it cannot be shown to be unaffected and counts as affected"
            ));
            return true;
        };

        warn_about_unmatchable_inputs(project_path, task_name, &entry.node);
        warn_about_root_relative_inputs(
            ChangedInputs {
                files: context.changed_files,
                project_root: project_path,
            },
            context.checkout_root,
            NamedNode {
                name: task_name,
                node: &entry.node,
            },
        );
        if entry
            .node
            .is_affected_by(context.changed_files, project_path)
        {
            return true;
        }

        node_dependency_names(&entry.node)
    };

    dependency_names.into_iter().any(|sub_dep_name| {
        let dependency_ref = if sub_dep_name.starts_with('#') {
            // A path is relative to the project that writes it: this one.
            named_dependency(
                &sub_dep_name,
                DependencyOrigin {
                    project_root: project_path,
                    projects,
                },
            )
        } else {
            format!("#{project_name}:{sub_dep_name}")
        };
        check_external_dependency(&dependency_ref, context)
    })
}

/// Where a dependency on another project is written, and the projects it
/// can name.
struct DependencyOrigin<'origin, S> {
    /// The directory of the project that declares the dependency, relative
    /// to the root of the checkout.
    project_root: &'origin Path,
    /// The projects of the module by name, each with its directory relative
    /// to the root of the checkout.
    projects: &'origin HashMap<String, (PathBuf, Project), S>,
}

// Derived, these would require `S: Copy`; only references are copied.
impl<S> Clone for DependencyOrigin<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for DependencyOrigin<'_, S> {}

/// Spell a dependency on another project by that project's name.
///
/// `#project:task` may spell its project as a path instead: relative to the
/// project that writes it (`#../content:articles`), or from the root of the
/// checkout (`#/content:articles`). The projects of a run are known by name,
/// each with its directory in the checkout, so a path to one of those
/// directories is the dependency on that project, and is compared with
/// changed files like any other.
///
/// Anything else is returned as written and counts as affected where it
/// does not resolve: a name no project has, a path that leaves the checkout,
/// and a path to a directory that only inherits a project's name.
fn named_dependency<S>(dep: &str, origin: DependencyOrigin<'_, S>) -> String
where
    S: BuildHasher,
{
    let DependencyOrigin {
        project_root,
        projects,
    } = origin;
    let Some((project, task)) = dep
        .strip_prefix('#')
        .and_then(|reference| reference.split_once(':'))
    else {
        return dep.to_string();
    };
    // A name is looked up as a name before anything else, as it is where
    // the dependency runs.
    if projects.contains_key(project) {
        return dep.to_string();
    }
    let Some(directory) = placed_reference(project, project_root) else {
        return dep.to_string();
    };
    projects
        .iter()
        .filter(|(_, (placed, _))| placed_reference(".", placed).as_ref() == Some(&directory))
        .map(|(name, _)| name)
        .min()
        .map_or_else(|| dep.to_string(), |name| format!("#{name}:{task}"))
}

/// The directory a path-shaped project reference names, relative to the
/// root of the checkout: from that root when the reference starts with `/`,
/// and from the project that writes it otherwise. `None` when it leaves the
/// checkout.
fn placed_reference(reference: &str, project_root: &Path) -> Option<PathBuf> {
    let reference = Path::new(reference);
    let joined = if reference.has_root() {
        reference.to_path_buf()
    } else {
        project_root.join(reference)
    };
    joined
        .components()
        .try_fold(PathBuf::new(), |mut placed, component| {
            match component {
                std::path::Component::Normal(part) => placed.push(part),
                std::path::Component::ParentDir => {
                    if !placed.pop() {
                        return None;
                    }
                }
                std::path::Component::CurDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_) => {}
            }
            Some(placed)
        })
}

fn cached_task_index<'a>(
    cache: &'a mut HashMap<String, Option<TaskIndex>>,
    project_name: &str,
    project_config: &Project,
) -> Option<&'a TaskIndex> {
    cache
        .entry(project_name.to_string())
        .or_insert_with(|| TaskIndex::build(&project_config.tasks).ok())
        .as_ref()
}

/// Warn about inputs of `node` that cannot be compared with changed files.
///
/// Such an input makes its task run whenever anything changed, which is
/// correct and may be slow, so the user is told why and what to expect.
fn warn_about_unmatchable_inputs(project_root: &Path, task_name: &str, node: &TaskNode) {
    for pattern in node.input_patterns() {
        let Some(reason) = cuenv_core::unmatchable_pattern(pattern) else {
            continue;
        };
        let explanation = match reason {
            UnmatchablePattern::ParentSegment => {
                "`..` inputs cannot be matched against changes, so this task always runs; they \
                 will be rejected once CUE-native cross-project references ship (#611)"
            }
            UnmatchablePattern::Absolute => {
                "absolute inputs cannot be matched against changes, which are relative to the \
                 repository, so this task always runs"
            }
            UnmatchablePattern::InvalidGlob => {
                "this input is not a valid glob, so it cannot be matched against changes and \
                 this task always runs"
            }
        };
        let task = WarnedTask {
            project_root,
            task_name,
        };
        warn_once_per_task(task, &format!("{reason:?}"), || {
            format!(
                "task '{task_name}' of project {} has input '{pattern}': {explanation}",
                display_project(project_root)
            )
        });
    }
}

/// Warn about inputs of `node` that someone relies on being read from the
/// root of the checkout.
///
/// An input is relative to its project, yet a changed file outside the
/// project has always been compared with the pattern as well. That keeps a
/// task affected in two different situations:
///
/// - The pattern selects nothing under its project (`shared/**` in `app`,
///   with a `shared/` directory only at the root). The root-relative reading
///   is what the user relies on, and it is going away, so they are told.
/// - The pattern does select files under its project, and the root happens
///   to have files of the same name (`package.json`, `src/**`). The
///   configuration is correct and the extra run is an accident. Nothing is
///   said.
///
/// Changed files alone cannot tell the two apart; the files of the project
/// can. Without a checkout to look at, nothing is said.
fn warn_about_root_relative_inputs(
    changed: ChangedInputs<'_>,
    checkout_root: Option<&Path>,
    task: NamedNode<'_>,
) {
    let Some(checkout_root) = checkout_root else {
        return;
    };
    let NamedNode {
        name: task_name,
        node,
    } = task;
    let warned = WarnedTask {
        project_root: changed.project_root,
        task_name,
    };
    for pattern in root_relative_inputs(changed, checkout_root, node) {
        warn_once_per_task(warned, "root-relative", || {
            format!(
                "task '{task_name}' of project {project} has input '{pattern}', which selects \
                 nothing under {project} and matched a changed file only when read from the \
                 root of the repository. Inputs are relative to their project; this \
                 root-relative reading still selects the task for now and is going away (#611)",
                project = display_project(changed.project_root)
            )
        });
    }
}

/// The inputs of `node` that matched a changed file only when read from the
/// root of the checkout and that select no file under their own project.
fn root_relative_inputs<'node>(
    changed: ChangedInputs<'_>,
    checkout_root: &Path,
    node: &'node TaskNode,
) -> Vec<&'node str> {
    let project_directory = checkout_root.join(changed.project_root);
    let walker = WalkHasher::new(&project_directory);
    node.input_patterns()
        .into_iter()
        .filter(|pattern| {
            cuenv_core::match_pattern(changed.files, changed.project_root, pattern)
                == PatternMatch::OnlyFromCheckoutRoot
        })
        // A pattern the walker cannot read selects nothing under the project.
        .filter(|pattern| !walker.selects_any(pattern).unwrap_or(false))
        .collect()
}

/// Whether a changed file can change the CUE configuration of the project
/// in `project_root`.
fn configuration_changed(
    changed_configuration: Option<&HashSet<PathBuf>>,
    project_root: &Path,
) -> bool {
    changed_configuration.is_some_and(|changed| {
        // `.` and the empty path both name a project at the checkout root.
        let project_root = if project_root == Path::new(".") {
            Path::new("")
        } else {
            project_root
        };
        changed.contains(project_root)
    })
}

/// A project's directory as a warning names it.
fn display_project(project_root: &Path) -> String {
    if project_root.as_os_str().is_empty() {
        ".".to_string()
    } else {
        project_root.display().to_string()
    }
}

/// A task node and the name it is selected by.
#[derive(Clone, Copy)]
struct NamedNode<'node> {
    name: &'node str,
    node: &'node TaskNode,
}

/// The task a warning is about.
#[derive(Clone, Copy)]
struct WarnedTask<'task> {
    project_root: &'task Path,
    task_name: &'task str,
}

/// Emit `message` unless the same kind of warning was already given for this
/// task in this run.
fn warn_once_per_task(task: WarnedTask<'_>, kind: &str, message: impl FnOnce() -> String) {
    let WarnedTask {
        project_root,
        task_name,
    } = task;
    let key = format!("{}\0{task_name}\0{kind}", project_root.display());
    if first_time(key) {
        cuenv_events::emit_stderr!(format!("warning: {}", message()));
    }
}

/// Emit a warning unless the same one was already given in this run.
fn warn_once(message: &str) {
    if first_time(message.to_string()) {
        cuenv_events::emit_stderr!(format!("warning: {message}"));
    }
}

/// Whether `key` is new to this run. A poisoned lock only means another
/// thread panicked while warning; the warning is then repeated.
fn first_time(key: String) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Mutex::default)
        .lock()
        .map_or(true, |mut seen| seen.insert(key))
}

#[cfg(test)]
#[path = "affected_tests.rs"]
mod tests;
