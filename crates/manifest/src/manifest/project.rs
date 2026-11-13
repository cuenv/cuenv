use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use super::{
    CodegenConfig, ContainerImage, Formatters, NodeDiagnostics, Runtime, Service, VcsDependency,
};
use crate::ci::CI;
use crate::config::Config;
use crate::context::ContextDefinition;
use crate::environment::Env;
use crate::hook::{Hook, Hooks};
use crate::tasks::{
    Input, MappedInput, Mapping, ProjectReference, SourceLocation, Task, TaskDirectoryBase,
    TaskNode, TaskOutput, literal_path_prefix,
};

// ============================================================================
// Project Type
// ============================================================================

#[derive(Clone)]
struct DeclaredTaskOutputs {
    outputs: Vec<String>,
    base: Option<PathBuf>,
}

/// Root Project configuration structure (leaf node - cannot unify with other projects)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Project {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Project name (unique identifier, required by the CUE schema)
    pub name: String,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Named contexts, selected from the facts of an invocation
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub contexts: HashMap<String, ContextDefinition>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// Cuenv-managed VCS dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vcs: HashMap<String, VcsDependency>,

    /// CI configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CI>,

    /// Project-level cache settings (where the cache lives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<super::Cache>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskNode>,

    /// Services configuration — long-running supervised processes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub services: HashMap<String, Service>,

    /// Container image build definitions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub images: HashMap<String, ContainerImage>,

    /// Codegen configuration for code generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codegen: Option<CodegenConfig>,

    /// Runtime configuration (project-level default for all tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Formatters configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatters: Option<Formatters>,

    /// Raw infrastructure configuration. Ordinary commands do not consume it
    /// and must tolerate unselected environments with incomplete CUE values.
    /// Infrastructure commands decode the selected configuration strictly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infrastructure: Option<serde_json::Value>,

    /// Problems found in single nodes while the evaluated value was read,
    /// raised where a graph, affected detection or CI compilation reaches
    /// the node (see [`NodeDiagnostics`]). Written by cuenv, never by CUE.
    #[serde(
        default,
        rename = "_cuenvDiagnostics",
        skip_serializing_if = "NodeDiagnostics::is_empty"
    )]
    pub diagnostics: NodeDiagnostics,
}

impl Project {
    /// Create a new Project configuration with a required name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Get hooks to execute when entering environment as a map (name -> hook)
    #[must_use]
    pub fn on_enter_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_enter.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when entering environment, sorted by (order, name)
    #[must_use]
    pub fn on_enter_hooks(&self) -> Vec<Hook> {
        let map = self.on_enter_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute when exiting environment as a map (name -> hook)
    #[must_use]
    pub fn on_exit_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_exit.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when exiting environment, sorted by (order, name)
    #[must_use]
    pub fn on_exit_hooks(&self) -> Vec<Hook> {
        let map = self.on_exit_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute before git push as a map (name -> hook)
    #[must_use]
    pub fn pre_push_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.pre_push.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute before git push, sorted by (order, name)
    #[must_use]
    pub fn pre_push_hooks(&self) -> Vec<Hook> {
        let map = self.pre_push_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Expand shorthand cross-project references in inputs and implicit dependencies.
    ///
    /// Handles inputs in the format: "#project:task:path/to/file"
    /// Converts them to explicit ProjectReference inputs.
    /// Also adds implicit dependsOn entries for all project references, and
    /// rewrites each same-project task-output input to the files its producer
    /// declares (see [`MappedInput`]).
    ///
    /// `instance_path` is the project's directory relative to the CUE module
    /// root (`.` or empty for the root): an explicit `map` places its `to`
    /// below it, and a task without source metadata is taken to run there.
    pub fn expand_cross_project_references(&mut self, instance_path: &Path) {
        let project_path = normalize_relative(instance_path).unwrap_or_default();
        let declared_outputs = self.declared_outputs_by_task(&project_path);
        let expansion = TaskExpansion {
            declared_outputs: &declared_outputs,
            project_path: &project_path,
        };
        for task_node in self.tasks.values_mut() {
            expansion.expand_node(task_node);
        }
    }

    /// Map every task's dotted path to the outputs it declares.
    ///
    /// Consuming a task's output is resolved against this: the consumer's
    /// inputs become the producer's output paths, so the consumer's cache key
    /// is a function of what the producer actually built.
    fn declared_outputs_by_task(
        &self,
        project_path: &Path,
    ) -> HashMap<String, DeclaredTaskOutputs> {
        let mut declared = HashMap::new();
        for (name, node) in &self.tasks {
            collect_declared_outputs(
                DeclaredNode {
                    path: name,
                    node,
                    project_path,
                },
                &mut declared,
            );
        }
        declared
    }
}

/// A node of the task tree while its declared outputs are collected.
#[derive(Clone, Copy)]
struct DeclaredNode<'node> {
    /// The node's dotted path.
    path: &'node str,
    node: &'node TaskNode,
    /// The project's directory, relative to the CUE module root.
    project_path: &'node Path,
}

fn collect_declared_outputs(
    declared_node: DeclaredNode<'_>,
    declared: &mut HashMap<String, DeclaredTaskOutputs>,
) {
    let DeclaredNode {
        path,
        node,
        project_path,
    } = declared_node;
    match node {
        TaskNode::Task(task) => {
            declared.insert(
                path.to_string(),
                DeclaredTaskOutputs {
                    outputs: task.outputs.clone(),
                    base: task_output_base(task, project_path),
                },
            );
        }
        TaskNode::Group(group) => {
            for (child, sub_node) in &group.children {
                collect_declared_outputs(
                    DeclaredNode {
                        path: &format!("{path}.{child}"),
                        node: sub_node,
                        project_path,
                    },
                    declared,
                );
            }
        }
        TaskNode::Sequence(steps) => {
            for (index, sub_node) in steps.iter().enumerate() {
                collect_declared_outputs(
                    DeclaredNode {
                        path: &format!("{path}[{index}]"),
                        node: sub_node,
                        project_path,
                    },
                    declared,
                );
            }
        }
    }
}

/// What rewriting a project's inputs reads: the outputs its tasks declare
/// and where the project lies in its CUE module.
#[derive(Clone, Copy)]
struct TaskExpansion<'expansion> {
    declared_outputs: &'expansion HashMap<String, DeclaredTaskOutputs>,
    project_path: &'expansion Path,
}

impl TaskExpansion<'_> {
    fn expand_node(self, node: &mut TaskNode) {
        match node {
            TaskNode::Task(task) => self.expand_task(task),
            TaskNode::Group(group) => {
                for sub_node in group.children.values_mut() {
                    self.expand_node(sub_node);
                }
            }
            TaskNode::Sequence(steps) => {
                for sub_node in steps {
                    self.expand_node(sub_node);
                }
            }
        }
    }

    fn expand_task(self, task: &mut Task) {
        let mut new_inputs = Vec::new();
        let mut implicit_deps = Vec::new();

        // Process existing inputs
        for input in &task.inputs {
            match input {
                Input::Path(path) if path.starts_with('#') => {
                    // Parse "#project:task:path"
                    // Remove leading #
                    let parts: Vec<&str> = path[1..].split(':').collect();
                    if parts.len() >= 3 {
                        let project = parts[0].to_string();
                        let task_name = parts[1].to_string();
                        // Rejoin the rest as the path (it might contain colons)
                        let file_path = parts[2..].join(":");

                        new_inputs.push(Input::Project(ProjectReference {
                            project: project.clone(),
                            task: task_name.clone(),
                            map: vec![Mapping {
                                from: file_path.clone(),
                                to: file_path,
                            }],
                        }));

                        // Add implicit dependency
                        implicit_deps.push(format!("#{}:{}", project, task_name));
                    } else {
                        // `#project:task` without a file is a dependency
                        // reference, not an input mapping: keep it verbatim.
                        new_inputs.push(input.clone());
                    }
                }
                Input::Project(proj_ref) => {
                    // Add implicit dependency for explicit project references too
                    implicit_deps.push(format!("#{}:{}", proj_ref.project, proj_ref.task));
                    new_inputs.push(input.clone());
                }
                Input::Task(task_output) => {
                    // Consuming a task's output is a dependency on it. Bazel
                    // and buck2 both derive the edge from the reference rather
                    // than making you declare it twice; without this the
                    // consumer can be scheduled alongside its producer.
                    implicit_deps.push(task_output.task.clone());
                    match self.mapped_task_output(task_output) {
                        Some(mapped) => new_inputs.extend(mapped.into_iter().map(Input::Mapped)),
                        // An unresolvable reference is left alone rather than
                        // silently dropped: the cache layer reports it.
                        None => new_inputs.push(input.clone()),
                    }
                }
                // An ordinary path input, already concrete. Files of another
                // project or of the module are no task's output, so they add
                // no dependency; they are bound to a directory once the
                // projects they name are known.
                Input::Path(_) | Input::Mapped(_) | Input::Files(_) => {
                    new_inputs.push(input.clone());
                }
            }
        }

        task.inputs = new_inputs;

        // Add unique implicit dependencies
        for dep in implicit_deps {
            if !task.depends_on.iter().any(|d| d.task_name() == dep) {
                task.depends_on
                    .push(crate::tasks::TaskDependency::from_name(dep));
            }
        }
    }

    /// The files a task-output input stands for, or `None` when its producer
    /// is unknown or runs outside the module.
    ///
    /// Rewriting to the producer's concrete output paths makes the consumer's
    /// cache key a function of what the producer built. Hashing the produced
    /// *content* rather than the producer's own key is what gives early
    /// cutoff: a producer that reruns and emits identical bytes leaves every
    /// consumer's key unchanged.
    ///
    /// Without `map` each output is staged where the producer writes it. An
    /// explicit `map` stages `from` at `to` below the consumer's project.
    fn mapped_task_output(self, task_output: &TaskOutput) -> Option<Vec<MappedInput>> {
        let declared = self.declared_outputs.get(&task_output.task)?;
        let base = declared.base.as_ref()?;
        let mapped = |source: &str, destination: PathBuf| MappedInput {
            source: path_to_forward_slashes(&base.join(source)),
            destination: path_to_forward_slashes(&destination),
            producer_task: Some(task_output.task.clone()),
            anchor: None,
        };
        Some(match &task_output.map {
            Some(mappings) => mappings
                .iter()
                .map(|mapping| mapped(&mapping.from, self.project_path.join(&mapping.to)))
                .collect(),
            None => declared
                .outputs
                .iter()
                .map(|output| mapped(output, base.join(literal_path_prefix(output))))
                .collect(),
        })
    }
}

/// The directory a task writes its outputs below, relative to the CUE module
/// root: its `dir`, else the directory of the file that defines it, else the
/// project's own directory. `None` when it lies outside the module.
fn task_output_base(task: &Task, project_path: &Path) -> Option<PathBuf> {
    let directory_of = |source: Option<&SourceLocation>| {
        source.map_or_else(
            || project_path.to_path_buf(),
            |location| {
                location
                    .directory()
                    .map_or_else(PathBuf::new, PathBuf::from)
            },
        )
    };

    let joined = match &task.directory {
        Some(directory) => {
            let base = match directory.from {
                TaskDirectoryBase::Definition => directory_of(task.source.as_ref()),
                TaskDirectoryBase::Caller => directory_of(task.caller_source.as_ref()),
                TaskDirectoryBase::Module => PathBuf::new(),
            };
            base.join(&directory.path)
        }
        None => directory_of(task.source.as_ref()),
    };
    normalize_relative(&joined)
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
