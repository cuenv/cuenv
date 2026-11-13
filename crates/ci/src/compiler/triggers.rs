//! CI trigger condition and path derivation.

use super::Compiler;
use crate::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
use cuenv_core::ci::{CI, ManualTrigger, Pipeline};
use cuenv_core::tasks::TaskNode;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// What the tasks a pipeline reaches read, as trigger derivation sees it.
#[derive(Default)]
struct PipelineInputs {
    /// Every path input of every reached task.
    patterns: HashSet<String>,
    /// A reached task has no path inputs, or does not resolve. Such a task
    /// always runs, so any file of the project can affect the pipeline.
    whole_project: bool,
    /// Tasks already walked, so a dependency cycle ends.
    visited: HashSet<String>,
}

impl Compiler {
    /// Build trigger condition for a pipeline from its configuration.
    pub(super) fn build_trigger_condition(
        &self,
        pipeline: &Pipeline,
        _ci_config: &CI,
    ) -> TriggerCondition {
        let when = pipeline.when.as_ref();

        let branches = when
            .and_then(|w| w.branch.as_ref())
            .map(cuenv_core::ci::StringOrVec::to_vec)
            .unwrap_or_default();

        let tags = when
            .and_then(|w| w.tag.as_ref())
            .map(cuenv_core::ci::StringOrVec::to_vec)
            .unwrap_or_default();

        let pull_request = when.and_then(|w| w.pull_request);

        let scheduled = when
            .and_then(|w| w.scheduled.as_ref())
            .map(cuenv_core::ci::StringOrVec::to_vec)
            .unwrap_or_default();

        let release = when.and_then(|w| w.release.clone()).unwrap_or_default();

        let manual = when.and_then(|w| w.manual.as_ref()).map(|m| match m {
            ManualTrigger::Enabled(enabled) => ManualTriggerConfig {
                enabled: *enabled,
                inputs: BTreeMap::new(),
            },
            ManualTrigger::WithInputs(inputs) => ManualTriggerConfig {
                enabled: true,
                inputs: inputs
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            WorkflowDispatchInputDef {
                                description: v.description.clone(),
                                required: v.required.unwrap_or(false),
                                default: v.default.clone(),
                                input_type: v.input_type.clone(),
                                options: v.options.clone().unwrap_or_default(),
                            },
                        )
                    })
                    .collect(),
            },
        });

        let should_derive_paths = pipeline
            .derive_paths
            .unwrap_or_else(|| !branches.is_empty() || pull_request.is_some());

        let paths = if should_derive_paths {
            self.derive_trigger_paths(pipeline)
        } else {
            Vec::new()
        };

        TriggerCondition {
            branches,
            tags,
            pull_request,
            scheduled,
            release,
            manual,
            paths,
        }
    }

    /// The path filters under which a workflow for `pipeline` must start.
    ///
    /// They are a superset of what `cuenv ci` treats as affecting a task of
    /// the pipeline, by construction. GitHub's filter syntax is not the
    /// glob dialect inputs are matched with at runtime (`*` stops at `/`
    /// there, and `?`, `+`, `[]` and a leading `!` mean something else), so
    /// no glob is translated: an input contributes its literal directory
    /// prefix, and `cuenv ci` does the precise selection once the workflow
    /// has started. A workflow that starts too often costs a run that
    /// selects nothing; one that does not start skips work silently.
    fn derive_trigger_paths(&self, pipeline: &Pipeline) -> Vec<String> {
        let mut inputs = PipelineInputs::default();

        for task in &pipeline.tasks {
            self.collect_task_inputs(task.task_name(), &mut inputs);
        }

        let project_path = self.options.project_path.as_deref();
        let mut paths = HashSet::new();
        for input in &inputs.patterns {
            match trigger_paths_for_input(project_path, input) {
                Some(derived) => paths.extend(derived),
                None => {
                    tracing::warn!(
                        project_path = project_path.unwrap_or("."),
                        input = input.as_str(),
                        "Skipping task input that escapes the repository root or is absolute; \
                         it will not contribute to derived GitHub path filters",
                    );
                }
            }
        }

        if inputs.whole_project || inputs.patterns.is_empty() {
            add_project_fallback_path(project_path, &mut paths);
        }

        self.add_implicit_cue_trigger_paths(&mut paths);
        self.add_workspace_dependency_paths(&mut paths);

        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    fn add_implicit_cue_trigger_paths(&self, paths: &mut HashSet<String>) {
        if let Some(path) =
            repo_relative_trigger_path(self.options.project_path.as_deref(), "env.cue")
        {
            paths.insert(path);
        }
        if let Some(path) =
            repo_relative_trigger_path(self.options.project_path.as_deref(), "schema/**")
        {
            paths.insert(path);
        }

        paths.insert("cue.mod/**".to_string());
    }

    /// Adds paths for workspace member dependencies (direct and transitive).
    ///
    /// If the current project is a member of a workspace (JS/npm, pnpm, or Cargo),
    /// this finds all other workspace members that this project depends on and
    /// adds their paths to the trigger paths.
    ///
    /// # Supported Workspace Types
    /// - npm/yarn workspaces (package.json)
    /// - pnpm workspaces (pnpm-workspace.yaml)
    /// - Cargo workspaces (Cargo.toml)
    ///
    /// # Testing
    /// Core dependency resolution logic is tested in `cuenv_workspaces::Workspace`.
    /// See `crates/workspaces/src/core/types.rs` for unit tests covering direct,
    /// transitive, and circular dependency resolution.
    fn add_workspace_dependency_paths(&self, paths: &mut HashSet<String>) {
        use cuenv_workspaces::{
            CargoTomlDiscovery, PackageJsonDiscovery, PnpmWorkspaceDiscovery, Workspace,
            WorkspaceDiscovery,
        };

        let Some(ref project_path) = self.options.project_path else {
            return;
        };

        if project_path == "." {
            return;
        }

        let module_root = self
            .options
            .module_root
            .clone()
            .or_else(|| self.options.project_root.clone())
            .unwrap_or_else(|| PathBuf::from("."));

        let workspace: Option<Workspace> = PackageJsonDiscovery
            .discover(&module_root)
            .ok()
            .or_else(|| PnpmWorkspaceDiscovery.discover(&module_root).ok())
            .or_else(|| CargoTomlDiscovery.discover(&module_root).ok());

        let Some(workspace) = workspace else {
            return;
        };

        let project_path_buf = Path::new(project_path);
        let Some(current_member) = workspace.find_member_by_path(project_path_buf) else {
            return;
        };

        for dep_path in workspace.resolve_workspace_dependency_paths(&current_member.name) {
            let mut pattern = dep_path.clone();
            pattern.push("**");
            paths.insert(pattern.to_string_lossy().into_owned());
        }
    }

    fn collect_task_inputs(&self, task_name: &str, inputs: &mut PipelineInputs) {
        if !inputs.visited.insert(task_name.to_string()) {
            return;
        }
        match self.find_task_node(task_name) {
            Some(node) => self.collect_inputs_from_node(node, inputs),
            // Nothing is known about what an unresolved task reads.
            None => inputs.whole_project = true,
        }
    }

    fn collect_inputs_from_node(&self, node: &TaskNode, inputs: &mut PipelineInputs) {
        for dep in node.depends_on() {
            self.collect_task_inputs(dep.task_name(), inputs);
        }
        match node {
            TaskNode::Task(task) => {
                let before = inputs.patterns.len();
                let mut declared = 0_usize;
                for input in task.iter_path_inputs() {
                    declared += 1;
                    inputs.patterns.insert(input.to_owned());
                }
                // A task without path inputs always runs.
                if declared == 0 {
                    inputs.whole_project = true;
                }
                debug_assert!(inputs.patterns.len() >= before);
            }
            TaskNode::Group(group) => {
                for child_node in group.children.values() {
                    self.collect_inputs_from_node(child_node, inputs);
                }
            }
            TaskNode::Sequence(steps) => {
                for child_node in steps {
                    self.collect_inputs_from_node(child_node, inputs);
                }
            }
        }
    }
}

fn add_project_fallback_path(project_path: Option<&str>, paths: &mut HashSet<String>) {
    match project_path {
        Some(".") | None => {
            paths.insert("**".to_string());
        }
        Some(project_path) => {
            paths.insert(format!("{project_path}/**"));
        }
    }
}

/// Convert a project-relative input glob into a repo-relative trigger path.
///
/// GitHub Actions path filters compare changed file names as repo-relative
/// strings. They do not normalize `server/../flake.nix`, so derived trigger
/// paths must be cleaned before emission.
fn repo_relative_trigger_path(project_path: Option<&str>, input: &str) -> Option<String> {
    let mut path = PathBuf::new();

    if let Some(project_path) = project_path.filter(|p| !p.is_empty() && *p != ".")
        && !push_relative_components(&mut path, Path::new(project_path))
    {
        return None;
    }

    if !push_relative_components(&mut path, Path::new(input)) {
        return None;
    }

    let rendered = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");

    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// The path filters one input of a project contributes; `None` when the
/// input is absolute or leaves the repository.
///
/// Only two shapes are ever emitted:
///
/// - a path that is not a glob, as itself and as `<path>/**`, since at
///   runtime it stands for that file or for everything beneath that
///   directory;
/// - a glob as the directory it is rooted in, `<literal prefix>/**`.
///
/// A segment that holds a character GitHub's filter syntax gives a meaning
/// to cannot be emitted literally, so the path is cut at its parent, which
/// becomes a `/**` filter. The same goes for the project's own directory.
fn trigger_paths_for_input(project_path: Option<&str>, input: &str) -> Option<Vec<String>> {
    let input = cuenv_vcs::pattern::normalize(input);
    let project = project_path.filter(|path| !path.is_empty() && *path != ".");

    // Module-relative segments of the input. `..` is resolved lexically
    // until such inputs are rejected.
    let mut joined = PathBuf::new();
    if let Some(project) = project
        && !push_relative_components(&mut joined, Path::new(project))
    {
        return None;
    }
    if !push_relative_components(&mut joined, Path::new(&input)) {
        return None;
    }
    let segments: Vec<String> = joined
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    let literal: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .take_while(|segment| {
            !cuenv_vcs::pattern::is_glob(segment) && !is_special_in_github_filter(segment)
        })
        .collect();
    let is_plain_path = literal.len() == segments.len() && !input.is_empty();

    if is_plain_path {
        let path = literal.join("/");
        return Some(vec![format!("{path}/**"), path]);
    }
    Some(vec![directory_filter(&literal)])
}

/// The filter for everything beneath a directory given by its segments.
fn directory_filter(segments: &[&str]) -> String {
    if segments.is_empty() {
        "**".to_string()
    } else {
        format!("{}/**", segments.join("/"))
    }
}

/// Whether a literal path segment would be read as a pattern by GitHub's
/// `paths` filters: `*`, `?`, `+` and `[]` are operators, `\` escapes, and a
/// leading `!` negates.
fn is_special_in_github_filter(segment: &str) -> bool {
    segment.contains(['*', '?', '+', '[', ']', '\\']) || segment.starts_with('!')
}

fn push_relative_components(path: &mut PathBuf, input: &Path) -> bool {
    for component in input.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => path.push(part),
            Component::ParentDir => {
                if !path.pop() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}
