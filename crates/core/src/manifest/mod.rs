//! Root Cuenv configuration type
//!
//! Based on schema/cuenv.cue

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ci::CI;
use crate::config::Config;
use crate::environment::Env;
use crate::hooks::Hook;
use crate::tasks::{Input, Mapping, ProjectReference, TaskGroup};
use crate::tasks::{Task, TaskDefinition};

/// Workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    /// Enable or disable the workspace
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional: manually specify the root of the workspace relative to env.cue
    pub root: Option<String>,

    /// Optional: manually specify the package manager
    pub package_manager: Option<String>,

    /// Workspace lifecycle hooks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<WorkspaceHooks>,
}

/// Workspace lifecycle hooks for pre/post install
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHooks {
    /// Tasks or references to run before workspace install
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_install: Option<Vec<HookItem>>,

    /// Tasks or references to run after workspace install
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_install: Option<Vec<HookItem>>,
}

/// A hook step to run as part of workspace lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum HookItem {
    /// Reference to a task in another project
    TaskRef(TaskRef),
    /// Discovery-based hook step that expands a TaskMatcher into concrete tasks
    Match(MatchHook),
    /// Inline task definition
    Task(Box<Task>),
}

/// Hook step that expands to tasks discovered via TaskMatcher.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHook {
    /// Optional stable name used for task naming/logging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Task matcher to select tasks across the workspace
    #[serde(rename = "match")]
    pub matcher: TaskMatcher,
}

/// Reference to a task in another env.cue project by its name property
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskRef {
    /// Format: "#project-name:task-name" where project-name is the `name` field in env.cue
    /// Example: "#projen-generator:bun.install"
    #[serde(rename = "ref")]
    pub ref_: String,
}

impl TaskRef {
    /// Parse the TaskRef into project name and task name
    /// Returns None if the format is invalid
    pub fn parse(&self) -> Option<(String, String)> {
        let ref_str = self.ref_.strip_prefix('#')?;
        let parts: Vec<&str> = ref_str.splitn(2, ':').collect();
        if parts.len() == 2 {
            Some((parts[0].to_string(), parts[1].to_string()))
        } else {
            None
        }
    }
}

/// Match tasks across workspace by metadata for discovery-based execution
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskMatcher {
    /// Limit to specific workspaces (by name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<String>>,

    /// Match tasks with these labels (all must match)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,

    /// Match tasks whose command matches this value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Match tasks whose args contain specific patterns
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<ArgMatcher>>,

    /// Run matched tasks in parallel (default: true)
    #[serde(default = "default_true")]
    pub parallel: bool,
}

/// Pattern matcher for task arguments
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ArgMatcher {
    /// Match if any arg contains this substring
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,

    /// Match if any arg matches this regex pattern
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Collection of hooks that can be executed
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Hooks {
    /// Named hooks to execute when entering an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onEnter")]
    pub on_enter: Option<HashMap<String, Hook>>,

    /// Named hooks to execute when exiting an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onExit")]
    pub on_exit: Option<HashMap<String, Hook>>,
}

/// Root Cuenv configuration structure
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Cuenv {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Project name (unique identifier)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// Workspaces configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,

    /// CI configuration (uses hidden field _ci in CUE to prevent inheritance)
    #[serde(rename = "_ci", skip_serializing_if = "Option::is_none")]
    pub ci: Option<CI>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskDefinition>,
}

impl Cuenv {
    /// Create a new empty Cuenv configuration
    pub fn new() -> Self {
        Self::default()
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

    /// Inject implicit tasks and dependencies based on workspace declarations.
    ///
    /// When a workspace is declared (e.g., `workspaces: bun: {}`), this method:
    /// 1. Creates an install task for that workspace if one doesn't already exist
    ///
    /// This ensures users don't need to manually define common tasks like
    /// `bun.install` or manually wire up dependencies.
    pub fn with_implicit_tasks(mut self) -> Self {
        fn get_task_mut_by_path<'a>(
            tasks: &'a mut HashMap<String, TaskDefinition>,
            raw_path: &str,
        ) -> Option<&'a mut Task> {
            let normalized = raw_path.replace(':', ".");
            let mut segments = normalized
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::trim)
                .collect::<Vec<_>>();
            if segments.is_empty() {
                return None;
            }

            let first = segments.remove(0);
            let mut current = tasks.get_mut(first)?;
            for seg in segments {
                match current {
                    TaskDefinition::Group(TaskGroup::Parallel(group)) => {
                        current = group.tasks.get_mut(seg)?;
                    }
                    _ => return None,
                }
            }

            match current {
                TaskDefinition::Single(task) => Some(task.as_mut()),
                _ => None,
            }
        }

        let Some(workspaces) = &self.workspaces else {
            return self;
        };

        // Clone workspaces to avoid borrow issues
        let workspaces = workspaces.clone();

        for (name, config) in &workspaces {
            if !config.enabled {
                continue;
            }

            // Only known workspace types get implicit install tasks
            if !matches!(name.as_str(), "bun" | "npm" | "pnpm" | "yarn" | "cargo") {
                continue;
            }

            // Only process workspace if at least one task explicitly uses it
            let workspace_used = self
                .tasks
                .values()
                .any(|task_def| task_def.uses_workspace(name));
            if !workspace_used {
                tracing::debug!("Skipping workspace '{}' - no tasks declare usage", name);
                continue;
            }

            let install_task_name = format!("{}.install", name);

            // Don't override user-defined install tasks (including nested `tasks: bun: install: {}`)
            if get_task_mut_by_path(&mut self.tasks, &install_task_name).is_some() {
                continue;
            }

            // Create implicit install task
            if let Some(task) = Self::create_implicit_install_task(name) {
                self.tasks
                    .insert(install_task_name, TaskDefinition::Single(Box::new(task)));
            }
        }

        self
    }

    /// Create an implicit install task for a known workspace type.
    fn create_implicit_install_task(workspace_name: &str) -> Option<Task> {
        let (command, args, description, inputs, outputs) = match workspace_name {
            "bun" => (
                "bun",
                vec!["install"],
                "Install bun dependencies",
                vec![
                    Input::Path("package.json".to_string()),
                    Input::Path("bun.lock".to_string()),
                ],
                vec!["node_modules".to_string()],
            ),
            "npm" => (
                "npm",
                vec!["install"],
                "Install npm dependencies",
                vec![
                    Input::Path("package.json".to_string()),
                    Input::Path("package-lock.json".to_string()),
                ],
                vec!["node_modules".to_string()],
            ),
            "pnpm" => (
                "pnpm",
                vec!["install"],
                "Install pnpm dependencies",
                vec![
                    Input::Path("package.json".to_string()),
                    Input::Path("pnpm-lock.yaml".to_string()),
                ],
                vec!["node_modules".to_string()],
            ),
            "yarn" => (
                "yarn",
                vec!["install"],
                "Install yarn dependencies",
                vec![
                    Input::Path("package.json".to_string()),
                    Input::Path("yarn.lock".to_string()),
                ],
                vec!["node_modules".to_string()],
            ),
            "cargo" => (
                "cargo",
                vec!["fetch"],
                "Fetch cargo dependencies",
                vec![
                    Input::Path("Cargo.toml".to_string()),
                    Input::Path("Cargo.lock".to_string()),
                ],
                vec![], // cargo fetch doesn't produce local outputs (uses shared cache)
            ),
            _ => return None, // Unknown workspace type, don't create implicit task
        };

        Some(Task {
            command: command.to_string(),
            args: args.into_iter().map(String::from).collect(),
            workspaces: vec![workspace_name.to_string()],
            hermetic: false, // Install tasks must run in real workspace root
            description: Some(description.to_string()),
            inputs,
            outputs,
            ..Default::default()
        })
    }

    /// Expand shorthand cross-project references in inputs and implicit dependencies.
    ///
    /// Handles inputs in the format: "#project:task:path/to/file"
    /// Converts them to explicit ProjectReference inputs.
    /// Also adds implicit dependsOn entries for all project references.
    pub fn expand_cross_project_references(&mut self) {
        for (_, task_def) in self.tasks.iter_mut() {
            Self::expand_task_definition(task_def);
        }
    }

    fn expand_task_definition(task_def: &mut TaskDefinition) {
        match task_def {
            TaskDefinition::Single(task) => Self::expand_task(task),
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for sub_task in tasks {
                        Self::expand_task_definition(sub_task);
                    }
                }
                TaskGroup::Parallel(group) => {
                    for sub_task in group.tasks.values_mut() {
                        Self::expand_task_definition(sub_task);
                    }
                }
            },
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
            if !task.depends_on.contains(&dep) {
                task.depends_on.push(dep);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{ParallelGroup, TaskIndex};

    #[test]
    fn test_expand_cross_project_references() {
        let task = Task {
            inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
            ..Default::default()
        };

        let mut cuenv = Cuenv::new();
        cuenv
            .tasks
            .insert("deploy".into(), TaskDefinition::Single(Box::new(task)));

        cuenv.expand_cross_project_references();

        let task_def = cuenv.tasks.get("deploy").unwrap();
        let task = task_def.as_single().unwrap();

        // Check inputs expansion
        assert_eq!(task.inputs.len(), 1);
        match &task.inputs[0] {
            Input::Project(proj_ref) => {
                assert_eq!(proj_ref.project, "myproj");
                assert_eq!(proj_ref.task, "build");
                assert_eq!(proj_ref.map.len(), 1);
                assert_eq!(proj_ref.map[0].from, "dist/app.js");
                assert_eq!(proj_ref.map[0].to, "dist/app.js");
            }
            _ => panic!("Expected ProjectReference"),
        }

        // Check implicit dependency
        assert_eq!(task.depends_on.len(), 1);
        assert_eq!(task.depends_on[0], "#myproj:build");
    }

    #[test]
    fn test_implicit_bun_install_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // Add a task that uses the bun workspace
        cuenv.tasks.insert(
            "dev".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "bun".to_string(),
                args: vec!["run".to_string(), "dev".to_string()],
                workspaces: vec!["bun".to_string()],
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();
        assert!(cuenv.tasks.contains_key("bun.install"));

        let task_def = cuenv.tasks.get("bun.install").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.command, "bun");
        assert_eq!(task.args, vec!["install"]);
        assert_eq!(task.workspaces, vec!["bun"]);
    }

    #[test]
    fn test_implicit_npm_install_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "npm".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // Add a task that uses the npm workspace
        cuenv.tasks.insert(
            "build".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "npm".to_string(),
                args: vec!["run".to_string(), "build".to_string()],
                workspaces: vec!["npm".to_string()],
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();
        assert!(cuenv.tasks.contains_key("npm.install"));
    }

    #[test]
    fn test_implicit_cargo_fetch_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "cargo".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // Add a task that uses the cargo workspace
        cuenv.tasks.insert(
            "build".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                workspaces: vec!["cargo".to_string()],
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();
        assert!(cuenv.tasks.contains_key("cargo.install"));

        let task_def = cuenv.tasks.get("cargo.install").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.command, "cargo");
        assert_eq!(task.args, vec!["fetch"]);
    }

    #[test]
    fn test_no_override_user_defined_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // User defines their own bun.install task
        let user_task = Task {
            command: "custom-bun".to_string(),
            args: vec!["custom-install".to_string()],
            ..Default::default()
        };
        cuenv.tasks.insert(
            "bun.install".into(),
            TaskDefinition::Single(Box::new(user_task)),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // User's task should not be overridden
        let task_def = cuenv.tasks.get("bun.install").unwrap();
        let task = task_def.as_single().unwrap();
        assert_eq!(task.command, "custom-bun");
    }

    #[test]
    fn test_no_override_user_defined_nested_install_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // User defines nested bun.install via tasks: bun: install: {}
        cuenv.tasks.insert(
            "bun".into(),
            TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                tasks: HashMap::from([(
                    "install".into(),
                    TaskDefinition::Single(Box::new(Task {
                        command: "custom-bun".to_string(),
                        args: vec!["custom-install".to_string()],
                        ..Default::default()
                    })),
                )]),
                depends_on: vec![],
            })),
        );

        // Add a task that uses the bun workspace (so implicit wiring runs)
        cuenv.tasks.insert(
            "dev".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "echo".to_string(),
                args: vec!["dev".to_string()],
                workspaces: vec!["bun".to_string()],
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // Should not have created a top-level bun.install (nested one should count).
        assert!(!cuenv.tasks.contains_key("bun.install"));

        // The nested bun.install should remain.
        let idx = TaskIndex::build(&cuenv.tasks).unwrap();
        let bun_install = idx.resolve("bun.install").unwrap();
        let TaskDefinition::Single(t) = &bun_install.definition else {
            panic!("expected bun.install to be a single task");
        };
        assert_eq!(t.command, "custom-bun");
    }

    #[test]
    fn test_disabled_workspace_no_implicit_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: false,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        let cuenv = cuenv.with_implicit_tasks();
        assert!(!cuenv.tasks.contains_key("bun.install"));
    }

    #[test]
    fn test_unknown_workspace_no_implicit_task() {
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "unknown-package-manager".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        let cuenv = cuenv.with_implicit_tasks();
        assert!(!cuenv.tasks.contains_key("unknown-package-manager.install"));
    }

    #[test]
    fn test_no_workspaces_unchanged() {
        let cuenv = Cuenv::new();
        let cuenv = cuenv.with_implicit_tasks();
        assert!(cuenv.tasks.is_empty());
    }

    #[test]
    fn test_no_workspace_tasks_when_unused() {
        // When no task uses a workspace, the implicit install tasks should not be created
        let mut cuenv = Cuenv::new();
        cuenv.workspaces = Some(HashMap::from([(
            "bun".into(),
            WorkspaceConfig {
                enabled: true,
                root: None,
                package_manager: None,
                hooks: None,
            },
        )]));

        // Add a task that does NOT use the bun workspace
        cuenv.tasks.insert(
            "build".into(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                workspaces: vec![], // No workspace usage
                ..Default::default()
            })),
        );

        let cuenv = cuenv.with_implicit_tasks();

        // bun.install should NOT be created since no task uses it
        assert!(
            !cuenv.tasks.contains_key("bun.install"),
            "Should not create bun.install when no task uses bun workspace"
        );
    }
}
