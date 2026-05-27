use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{CodegenConfig, ContainerImage, Formatters, Runtime, Service, VcsDependency};
use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::module::Instance;
use crate::tasks::{Input, Mapping, ProjectReference, Task, TaskNode};
use cuenv_hooks::{Hook, Hooks};

// ============================================================================
// Project Type
// ============================================================================

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
    pub fn on_enter_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_enter.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when entering environment, sorted by (order, name)
    pub fn on_enter_hooks(&self) -> Vec<Hook> {
        let map = self.on_enter_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute when exiting environment as a map (name -> hook)
    pub fn on_exit_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_exit.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute when exiting environment, sorted by (order, name)
    pub fn on_exit_hooks(&self) -> Vec<Hook> {
        let map = self.on_exit_hooks_map();
        let mut hooks: Vec<(String, Hook)> = map.into_iter().collect();
        hooks.sort_by(|a, b| a.1.order.cmp(&b.1.order).then(a.0.cmp(&b.0)));
        hooks.into_iter().map(|(_, h)| h).collect()
    }

    /// Get hooks to execute before git push as a map (name -> hook)
    pub fn pre_push_hooks_map(&self) -> HashMap<String, Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.pre_push.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Get hooks to execute before git push, sorted by (order, name)
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
        for (_, task_node) in self.tasks.iter_mut() {
            Self::expand_task_node(task_node);
        }
    }

    fn expand_task_node(node: &mut TaskNode) {
        match node {
            TaskNode::Task(task) => Self::expand_task(task),
            TaskNode::Group(group) => {
                for sub_node in group.children.values_mut() {
                    Self::expand_task_node(sub_node);
                }
            }
            TaskNode::Sequence(steps) => {
                for sub_node in steps {
                    Self::expand_task_node(sub_node);
                }
            }
        }
    }

    fn expand_task(task: &mut Task) {
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
                _ => new_inputs.push(input.clone()),
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

impl TryFrom<&Instance> for Project {
    type Error = crate::Error;

    fn try_from(instance: &Instance) -> Result<Self, Self::Error> {
        let mut project: Project = instance.deserialize()?;
        project.expand_cross_project_references();
        Ok(project)
    }
}
