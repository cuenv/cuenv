//! CI trigger condition and path derivation.

use super::{Compiler, CompilerError};
use crate::configuration::{directory_filter, is_special_in_github_filter};
use crate::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
use cuenv_core::ci::{ManualTrigger, Pipeline};
use cuenv_core::tasks::file_references::{for_task, path_problem};
use cuenv_core::tasks::{FilesAnchor, InputPattern, TaskNode, anchored_path_problem};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// What the tasks a pipeline reaches read, as trigger derivation sees it.
#[derive(Default)]
pub(super) struct PipelineInputs {
    /// Every path input of every reached task, relative to the project.
    patterns: HashSet<String>,
    /// Every path of a file reference of every reached task: its anchor's
    /// directory relative to the module root (empty for the root), and the
    /// path relative to that directory.
    anchored: HashSet<(String, String)>,
    /// A reached task has no inputs, or does not resolve. Such a task
    /// always runs, so any file of the project can affect the pipeline.
    whole_project: bool,
    /// Tasks already walked, so a dependency cycle ends.
    visited: HashSet<String>,
    /// Why a reached node cannot run: problems recorded against it when the
    /// project's value was read, and file references that do not bind.
    problems: Vec<String>,
}

impl Compiler {
    /// Walk what `pipeline` reaches: the inputs trigger derivation reads,
    /// with every file reference bound to its anchor's directory.
    ///
    /// # Errors
    ///
    /// Fails when a reached node cannot run: the pipeline or a reached task
    /// has a problem recorded when the project's value was read, or a file
    /// reference names a project the module does not have or a path that
    /// breaks the rules every reference path follows. Whether a path
    /// matches any file is not checked: `cuenv sync ci` must not depend on
    /// a working tree.
    pub(super) fn reached_inputs(
        &self,
        pipeline: &Pipeline,
    ) -> Result<PipelineInputs, CompilerError> {
        let mut inputs = PipelineInputs::default();
        let pipeline_name = self.options.pipeline_name.as_deref().unwrap_or_default();
        if !pipeline_name.is_empty() {
            inputs
                .problems
                .extend_from_slice(self.project.diagnostics.pipeline(pipeline_name));
        }
        for task in &pipeline.tasks {
            self.collect_task_inputs(task.task_name(), &mut inputs);
        }
        if inputs.problems.is_empty() {
            Ok(inputs)
        } else {
            Err(CompilerError::UnrunnableNodes {
                pipeline: pipeline_name.to_string(),
                problems: std::mem::take(&mut inputs.problems),
            })
        }
    }

    /// Build trigger condition for a pipeline from its configuration.
    pub(super) fn build_trigger_condition(
        &self,
        pipeline: &Pipeline,
        inputs: &PipelineInputs,
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
            self.derive_trigger_paths(inputs)
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
    /// They are a superset of the changes that affect a task of the
    /// pipeline, by construction. The one exception is an input with a `..`
    /// segment inside the repository: it is emitted at the path it resolves
    /// to, while `cuenv ci` selects its task on any change until such inputs
    /// are rejected. GitHub's filter syntax is not the
    /// glob dialect inputs are matched with at runtime (`*` stops at `/`
    /// there, and `?`, `+`, `[]` and a leading `!` mean something else), so
    /// no glob is translated: an input contributes its literal directory
    /// prefix, and `cuenv ci` does the precise selection once the workflow
    /// has started. A workflow that starts too often costs a run that
    /// selects nothing; one that does not start skips work silently.
    fn derive_trigger_paths(&self, inputs: &PipelineInputs) -> Vec<String> {
        let project_path = self.options.project_path.as_deref();
        let mut paths = HashSet::new();
        // A file reference's paths start from its anchor, in the same
        // module-relative frame as every other path here, and take the same
        // two shapes; a glob with no literal prefix falls back to the
        // anchor's own directory.
        for (anchor, path) in &inputs.anchored {
            paths.extend(
                trigger_paths_for_input(Some(anchor), path)
                    .unwrap_or_else(|| vec![directory_filter(&[])]),
            );
        }
        for input in &inputs.patterns {
            // `cuenv ci` selects a task with an absolute input or an invalid
            // glob on any change, and no narrower filter covers what such an
            // input reads.
            let matches_anything = matches!(
                cuenv_core::unmatchable_pattern(input),
                Some(
                    cuenv_core::UnmatchablePattern::Absolute
                        | cuenv_core::UnmatchablePattern::InvalidGlob
                )
            );
            match trigger_paths_for_input(project_path, input) {
                Some(derived) if !matches_anything => paths.extend(derived),
                _ => {
                    paths.insert(directory_filter(&[]));
                }
            }
        }

        if inputs.whole_project || (inputs.patterns.is_empty() && inputs.anchored.is_empty()) {
            add_project_fallback_path(project_path, &mut paths);
        }

        self.add_implicit_cue_trigger_paths(&mut paths);
        self.add_workspace_dependency_paths(&mut paths);

        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    /// Add the path filters for a change to the project's configuration.
    ///
    /// A change to the CUE a project is built from selects every task of the
    /// project at runtime, so the workflow has to start for it: the project's
    /// own files, the same-package files of the directories above it, every
    /// package it imports, what its `@embed` attributes read, and `cue.mod`.
    /// They come from the CUE loader. Where the instance cannot be loaded
    /// (the compiler was given no module root, as when it compiles a project
    /// that is not on disk), the project's `env.cue` and `schema/` stand in.
    fn add_implicit_cue_trigger_paths(&self, paths: &mut HashSet<String>) {
        if let Some(sources) = self.configuration_sources() {
            paths.extend(crate::configuration::trigger_paths(&sources));
            return;
        }

        for input in ["env.cue", "schema/**"] {
            if let Some(derived) =
                trigger_paths_for_input(self.options.project_path.as_deref(), input)
            {
                paths.extend(derived);
            }
        }

        paths.insert("cue.mod/**".to_string());
    }

    /// What the project's instance is built from, when it can be loaded.
    fn configuration_sources(&self) -> Option<cuengine::InstanceSources> {
        const PACKAGE: &str = "cuenv";
        let module_root = self.options.module_root.as_ref()?;
        let directory = match self.options.project_path.as_deref() {
            Some(project_path) if !project_path.is_empty() && project_path != "." => {
                module_root.join(project_path)
            }
            _ => module_root.clone(),
        };
        cuengine::instance_sources(module_root, &directory, PACKAGE)
            .inspect_err(|error| {
                tracing::debug!(
                    directory = %directory.display(),
                    %error,
                    "Could not list the project's CUE files; using its env.cue for triggers"
                );
            })
            .ok()
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
            let dep_path = dep_path.to_string_lossy();
            paths.extend(
                trigger_paths_for_input(Some(&dep_path), "")
                    .unwrap_or_else(|| vec![directory_filter(&[])]),
            );
        }
    }

    fn collect_task_inputs(&self, task_name: &str, inputs: &mut PipelineInputs) {
        if !inputs.visited.insert(task_name.to_string()) {
            return;
        }
        match self.find_task_node(task_name) {
            Some(node) => self.collect_inputs_from_node(task_name, node, inputs),
            // Nothing is known about what an unresolved task reads.
            None => inputs.whole_project = true,
        }
    }

    fn collect_inputs_from_node(&self, name: &str, node: &TaskNode, inputs: &mut PipelineInputs) {
        inputs
            .problems
            .extend_from_slice(self.project.diagnostics.task(name));
        for dep in node.depends_on() {
            self.collect_task_inputs(dep.task_name(), inputs);
        }
        match node {
            TaskNode::Task(task) => {
                let mut declared = 0_usize;
                for pattern in task.input_patterns() {
                    declared += 1;
                    match pattern {
                        InputPattern::Project(path) => {
                            inputs.patterns.insert(path.to_owned());
                        }
                        InputPattern::Anchored { anchor, path } => match self.bind(anchor, path) {
                            Ok(directory) => {
                                inputs.anchored.insert((directory, path.to_owned()));
                            }
                            Err(problem) => {
                                inputs.problems.push(for_task(
                                    name,
                                    task.source.as_ref(),
                                    &problem,
                                ));
                            }
                        },
                    }
                }
                // A task without inputs always runs.
                if declared == 0 {
                    inputs.whole_project = true;
                }
            }
            TaskNode::Group(group) => {
                for (child, child_node) in &group.children {
                    self.collect_inputs_from_node(&format!("{name}.{child}"), child_node, inputs);
                }
            }
            TaskNode::Sequence(steps) => {
                for (index, child_node) in steps.iter().enumerate() {
                    self.collect_inputs_from_node(&format!("{name}[{index}]"), child_node, inputs);
                }
            }
        }
    }

    /// The module-relative directory one path of a file reference starts
    /// from, once the path is known to follow the rules every reference
    /// path follows.
    fn bind(&self, anchor: &FilesAnchor, path: &str) -> Result<String, String> {
        if let Some(problem) = anchored_path_problem(path) {
            return Err(path_problem(anchor, path, problem));
        }
        match anchor {
            FilesAnchor::Module => Ok(String::new()),
            // Bound among what the project imports, as everywhere else; no
            // working tree is read.
            FilesAnchor::Project(name) => self
                .imported_projects()?
                .bind(name)
                .map(|directory| directory.to_string_lossy().replace('\\', "/")),
        }
    }
}

/// Everything beneath the project's directory, cut where GitHub would read a
/// segment as a pattern.
fn add_project_fallback_path(project_path: Option<&str>, paths: &mut HashSet<String>) {
    paths.extend(
        trigger_paths_for_input(project_path, "").unwrap_or_else(|| vec![directory_filter(&[])]),
    );
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
