use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use super::{CodegenConfig, ContainerImage, Formatters, Runtime, Service, VcsDependency};
use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::tasks::{
    Input, MappedInput, Mapping, ProjectReference, Task, TaskDirectoryBase, TaskNode,
};
use cuenv_hooks::{Hook, Hooks};

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

    /// Returns self unchanged.
    ///
    /// Workspace detection and task injection now happens via auto-detection
    /// from lockfiles in the task executor. This method is kept for API compatibility.
    #[must_use]
    pub fn with_implicit_tasks(self) -> Self {
        self
    }

    /// Expand shorthand cross-project references in inputs and implicit dependencies.
    ///
    /// Handles inputs in the format: "#project:task:path/to/file"
    /// Converts them to explicit ProjectReference inputs.
    /// Also adds implicit dependsOn entries for all project references.
    pub fn expand_cross_project_references(&mut self) {
        let declared_outputs = self.declared_outputs_by_task();
        for task_node in self.tasks.values_mut() {
            Self::expand_task_node(task_node, &declared_outputs);
        }
    }

    /// Map every task's dotted path to the outputs it declares.
    ///
    /// Consuming a task's output is resolved against this: the consumer's
    /// inputs become the producer's output paths, so the consumer's cache key
    /// is a function of what the producer actually built.
    fn declared_outputs_by_task(&self) -> HashMap<String, DeclaredTaskOutputs> {
        let mut declared = HashMap::new();
        for (name, node) in &self.tasks {
            Self::collect_declared_outputs(name, node, &mut declared);
        }
        declared
    }

    fn collect_declared_outputs(
        path: &str,
        node: &TaskNode,
        declared: &mut HashMap<String, DeclaredTaskOutputs>,
    ) {
        match node {
            TaskNode::Task(task) => {
                declared.insert(
                    path.to_string(),
                    DeclaredTaskOutputs {
                        outputs: task.outputs.clone(),
                        base: task_output_base(task),
                    },
                );
            }
            TaskNode::Group(group) => {
                for (child, sub_node) in &group.children {
                    Self::collect_declared_outputs(&format!("{path}.{child}"), sub_node, declared);
                }
            }
            TaskNode::Sequence(steps) => {
                for (index, sub_node) in steps.iter().enumerate() {
                    Self::collect_declared_outputs(&format!("{path}[{index}]"), sub_node, declared);
                }
            }
        }
    }

    fn expand_task_node(
        node: &mut TaskNode,
        declared_outputs: &HashMap<String, DeclaredTaskOutputs>,
    ) {
        match node {
            TaskNode::Task(task) => Self::expand_task(task, declared_outputs),
            TaskNode::Group(group) => {
                for sub_node in group.children.values_mut() {
                    Self::expand_task_node(sub_node, declared_outputs);
                }
            }
            TaskNode::Sequence(steps) => {
                for sub_node in steps {
                    Self::expand_task_node(sub_node, declared_outputs);
                }
            }
        }
    }

    fn expand_task(
        task: &mut Task,
        declared_outputs: &HashMap<String, DeclaredTaskOutputs>,
    ) {
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
                    } else if parts.len() == 2 {
                        // Handle "#project:task" as pure dependency?
                        // The prompt says: `["#projectName:taskName"]` for dependsOn
                        // For inputs, it likely expects a file mapping.
                        // If user puts `["#p:t"]` in inputs, it's invalid as an input unless it maps something.
                        // Assuming `#p:t:f` is the requirement for inputs.
                        // Keeping original if not matching pattern (or maybe warning?)
                        new_inputs.push(input.clone());
                    } else {
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

                    // Rewrite to the producer's concrete output paths so the
                    // consumer's cache key is a function of what the producer
                    // built. Hashing the produced *content* rather than the
                    // producer's own key is what gives early cutoff: a
                    // producer that reruns and emits identical bytes leaves
                    // every consumer's key unchanged.
                    match declared_outputs.get(&task_output.task) {
                        Some(declared) => match &declared.base {
                            Some(base) => {
                                let mappings = task_output.map.clone().unwrap_or_else(|| {
                                    declared
                                        .outputs
                                        .iter()
                                        .map(|output| Mapping {
                                            from: output.clone(),
                                            to: output.clone(),
                                        })
                                        .collect()
                                });
                                new_inputs.extend(mappings.into_iter().map(|mapping| {
                                    Input::Mapped(MappedInput {
                                        source: path_to_forward_slashes(
                                            &base.join(&mapping.from),
                                        ),
                                        destination: mapping.to,
                                    })
                                }));
                            }
                            None => new_inputs.push(input.clone()),
                        },
                        // An unresolvable reference is left alone rather than
                        // silently dropped: the cache layer reports it.
                        None => new_inputs.push(input.clone()),
                    }
                }
                // An ordinary path input, already concrete.
                Input::Path(_) | Input::Mapped(_) => new_inputs.push(input.clone()),
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
}

fn task_output_base(task: &Task) -> Option<PathBuf> {
    let source_base = |caller: bool| {
        let source = if caller {
            task.caller_source.as_ref()
        } else {
            task.source.as_ref()
        };
        source
            .and_then(|location| location.directory())
            .map_or_else(PathBuf::new, PathBuf::from)
    };

    let joined = match &task.directory {
        Some(directory) => {
            let base = match directory.from {
                TaskDirectoryBase::Definition => source_base(false),
                TaskDirectoryBase::Caller => source_base(true),
                TaskDirectoryBase::Module => PathBuf::new(),
            };
            base.join(&directory.path)
        }
        None => source_base(false),
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
