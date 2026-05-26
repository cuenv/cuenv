//! CI trigger condition and path derivation.

use super::Compiler;
use crate::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
use cuenv_core::ci::{CI, ManualTrigger, Pipeline};
use cuenv_core::tasks::TaskNode;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

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
            pull_request,
            scheduled,
            release,
            manual,
            paths,
        }
    }

    fn derive_trigger_paths(&self, pipeline: &Pipeline) -> Vec<String> {
        let mut task_inputs = HashSet::new();

        for task in &pipeline.tasks {
            self.collect_task_inputs(task.task_name(), &mut task_inputs);
        }

        let mut paths = HashSet::new();
        self.add_task_input_trigger_paths(&task_inputs, &mut paths);

        if task_inputs.is_empty() {
            add_project_fallback_path(self.options.project_path.as_deref(), &mut paths);
        }

        self.add_implicit_cue_trigger_paths(&mut paths);
        self.add_workspace_dependency_paths(&mut paths);

        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    fn add_task_input_trigger_paths(
        &self,
        task_inputs: &HashSet<String>,
        paths: &mut HashSet<String>,
    ) {
        for input in task_inputs {
            match repo_relative_trigger_path(self.options.project_path.as_deref(), input) {
                Some(path) => {
                    if is_simple_path_pattern(input) {
                        paths.insert(format!("{path}/**"));
                    }
                    paths.insert(path);
                }
                None => {
                    tracing::warn!(
                        project_path = self.options.project_path.as_deref().unwrap_or("."),
                        input = input.as_str(),
                        "Skipping task input that escapes the repository root or is absolute; \
                         it will not contribute to derived GitHub path filters",
                    );
                }
            }
        }
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

    fn collect_task_inputs(&self, task_name: &str, paths: &mut HashSet<String>) {
        if let Some(node) = self.find_task_node(task_name) {
            self.collect_inputs_from_node(node, paths);
        }
    }

    fn collect_inputs_from_node(&self, node: &TaskNode, paths: &mut HashSet<String>) {
        match node {
            TaskNode::Task(task) => {
                paths.extend(task.iter_path_inputs().cloned());
                for dep in &task.depends_on {
                    self.collect_task_inputs(dep.task_name(), paths);
                }
            }
            TaskNode::Group(group) => {
                for child_node in group.children.values() {
                    self.collect_inputs_from_node(child_node, paths);
                }
            }
            TaskNode::Sequence(steps) => {
                for child_node in steps {
                    self.collect_inputs_from_node(child_node, paths);
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

/// True when `input` contains none of the glob metacharacters that
/// `cuenv_core::affected::matches_pattern` uses to switch between glob
/// matching and prefix matching. Kept in sync with that function so derived
/// GitHub trigger paths stay aligned with local affected detection.
fn is_simple_path_pattern(input: &str) -> bool {
    !input.contains('*') && !input.contains('?') && !input.contains('[')
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
