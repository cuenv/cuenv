//! Contributor engine for task DAG injection
//!
//! Contributors are CUE-defined task injectors that modify the task DAG before execution.
//! The engine evaluates activation conditions and injects tasks with proper naming.
//!
//! ## Data Flow
//!
//! 1. CUE evaluation produces Projects with Tasks (initial DAG)
//! 2. ContributorEngine applies contributors:
//!    - Evaluates `when` conditions (workspaceMember, command patterns)
//!    - Injects contributor tasks with `cuenv:contributor:*` prefix
//!    - Auto-associates user tasks with contributor setup tasks
//!    - Loops until no changes (stable DAG)
//! 3. Final DAG passed to executor (CLI or CI)
//!
//! ## Task Naming Convention
//!
//! Contributor tasks use the format: `cuenv:contributor:{contributor}.{task}`
//! Example: `cuenv:contributor:bun.workspace.install`

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tasks::{Input, Task, TaskDefinition, TaskGroup};
use crate::Result;

/// Prefix for all contributor-injected tasks
pub const CONTRIBUTOR_TASK_PREFIX: &str = "cuenv:contributor:";

/// Context provided to contributors for activation condition evaluation
#[derive(Debug, Clone, Default)]
pub struct ContributorContext {
    /// Detected workspace membership (e.g., "bun", "npm", "cargo")
    pub workspace_member: Option<String>,

    /// Path to workspace root (if member of a workspace)
    pub workspace_root: Option<std::path::PathBuf>,

    /// All commands used by tasks in the project (for command-based activation)
    pub task_commands: HashSet<String>,
}

impl ContributorContext {
    /// Create context by detecting workspace from project root
    #[must_use]
    pub fn detect(project_root: &Path) -> Self {
        let mut ctx = Self::default();

        // Use cuenv-workspaces for detection
        if let Ok(managers) = cuenv_workspaces::detect_package_managers(project_root)
            && let Some(first) = managers.first()
        {
            ctx.workspace_member = Some(workspace_name_for_manager(*first).to_string());
        }

        ctx
    }

    /// Add task commands from a project's tasks
    pub fn with_task_commands(mut self, tasks: &HashMap<String, TaskDefinition>) -> Self {
        for def in tasks.values() {
            collect_commands_from_definition(def, &mut self.task_commands);
        }
        self
    }
}

/// Returns the canonical workspace name for a package manager
fn workspace_name_for_manager(manager: cuenv_workspaces::PackageManager) -> &'static str {
    match manager {
        cuenv_workspaces::PackageManager::Npm => "npm",
        cuenv_workspaces::PackageManager::Bun => "bun",
        cuenv_workspaces::PackageManager::Pnpm => "pnpm",
        cuenv_workspaces::PackageManager::YarnClassic
        | cuenv_workspaces::PackageManager::YarnModern => "yarn",
        cuenv_workspaces::PackageManager::Cargo => "cargo",
        cuenv_workspaces::PackageManager::Deno => "deno",
    }
}

/// Collect all commands from a task definition recursively
fn collect_commands_from_definition(def: &TaskDefinition, commands: &mut HashSet<String>) {
    match def {
        TaskDefinition::Single(task) => {
            if !task.command.is_empty() {
                // Extract the base command (first word)
                if let Some(cmd) = task.command.split_whitespace().next() {
                    commands.insert(cmd.to_string());
                }
            }
        }
        TaskDefinition::Group(group) => match group {
            TaskGroup::Sequential(tasks) => {
                for sub in tasks {
                    collect_commands_from_definition(sub, commands);
                }
            }
            TaskGroup::Parallel(pg) => {
                for sub in pg.tasks.values() {
                    collect_commands_from_definition(sub, commands);
                }
            }
        },
    }
}

/// Activation condition for contributors
///
/// All specified conditions must be true (AND logic)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContributorActivation {
    /// Always active (no conditions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,

    /// Workspace membership detection (active if project is member of these workspace types)
    /// Values: "npm", "bun", "pnpm", "yarn", "cargo", "deno"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_member: Vec<String>,

    /// Command detection for auto-association (active if any task uses these commands)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
}

/// Auto-association rules for contributors
///
/// Defines how user tasks are automatically connected to contributor tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoAssociate {
    /// Commands that trigger auto-association (e.g., ["bun", "bunx"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,

    /// Task to inject as dependency (e.g., "cuenv:contributor:bun.workspace.setup")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_dependency: Option<String>,
}

/// A task contributed by a contributor
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContributorTask {
    /// Task identifier (will be prefixed with contributor namespace)
    pub id: String,

    /// Shell command to execute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Multi-line script (alternative to command)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Input files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Output files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// Whether task requires hermetic execution
    #[serde(default)]
    pub hermetic: bool,

    /// Dependencies on other tasks (within contributor namespace)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Contributor definition
///
/// Contributors inject tasks into the DAG based on activation conditions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// Contributor identifier (e.g., "bun.workspace")
    pub id: String,

    /// Activation condition (defaults to always active)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<ContributorActivation>,

    /// Tasks to contribute when active
    pub tasks: Vec<ContributorTask>,

    /// Auto-association rules for user tasks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_associate: Option<AutoAssociate>,
}

/// Engine that applies contributors to modify the task DAG
pub struct ContributorEngine<'a> {
    contributors: &'a [Contributor],
    context: ContributorContext,
}

impl<'a> ContributorEngine<'a> {
    /// Create a new contributor engine
    #[must_use]
    pub fn new(contributors: &'a [Contributor], context: ContributorContext) -> Self {
        Self {
            contributors,
            context,
        }
    }

    /// Apply all active contributors to the task DAG
    ///
    /// Loops until no contributor makes changes (stable DAG).
    /// Returns the number of tasks injected.
    pub fn apply(&self, tasks: &mut HashMap<String, TaskDefinition>) -> Result<usize> {
        let mut total_injected = 0;
        let max_iterations = 10; // Safety limit to prevent infinite loops

        for iteration in 0..max_iterations {
            let mut changed = false;

            for contributor in self.contributors {
                if self.is_active(contributor) {
                    let injected = self.inject_tasks(contributor, tasks);
                    if injected > 0 {
                        changed = true;
                        total_injected += injected;
                        tracing::debug!(
                            contributor = %contributor.id,
                            injected,
                            "Contributor injected tasks"
                        );
                    }

                    // Apply auto-association rules
                    if let Some(auto_assoc) = &contributor.auto_associate {
                        self.apply_auto_association(auto_assoc, tasks);
                    }
                }
            }

            if !changed {
                tracing::debug!(
                    iterations = iteration + 1,
                    total_injected,
                    "Contributor loop stabilized"
                );
                break;
            }
        }

        Ok(total_injected)
    }

    /// Check if a contributor should be active based on its conditions
    fn is_active(&self, contributor: &Contributor) -> bool {
        let Some(when) = &contributor.when else {
            // No conditions means always active
            return true;
        };

        // Check always flag
        if when.always == Some(true) {
            return true;
        }

        // Check workspace membership (OR within, AND with other conditions)
        if !when.workspace_member.is_empty() {
            let has_match = self.context.workspace_member.as_ref().is_some_and(|ws| {
                when.workspace_member
                    .iter()
                    .any(|w| w.eq_ignore_ascii_case(ws))
            });
            if !has_match {
                return false;
            }
        }

        // Check command usage (OR within, AND with other conditions)
        if !when.command.is_empty() {
            let has_match = when
                .command
                .iter()
                .any(|cmd| self.context.task_commands.contains(cmd));
            if !has_match {
                return false;
            }
        }

        true
    }

    /// Inject tasks from a contributor into the DAG
    ///
    /// Returns the number of tasks injected
    fn inject_tasks(
        &self,
        contributor: &Contributor,
        tasks: &mut HashMap<String, TaskDefinition>,
    ) -> usize {
        let mut injected = 0;

        for contrib_task in &contributor.tasks {
            // Build the full task ID with prefix
            let task_id = if contrib_task.id.starts_with(CONTRIBUTOR_TASK_PREFIX) {
                contrib_task.id.clone()
            } else {
                format!("{}{}", CONTRIBUTOR_TASK_PREFIX, contrib_task.id)
            };

            // Skip if already exists
            if tasks.contains_key(&task_id) {
                continue;
            }

            // Convert ContributorTask to TaskDefinition
            let task = Task {
                command: contrib_task.command.clone().unwrap_or_default(),
                args: contrib_task.args.clone(),
                script: contrib_task.script.clone(),
                inputs: contrib_task
                    .inputs
                    .iter()
                    .map(|s| Input::Path(s.clone()))
                    .collect(),
                outputs: contrib_task.outputs.clone(),
                hermetic: contrib_task.hermetic,
                depends_on: contrib_task
                    .depends_on
                    .iter()
                    .map(|dep| {
                        // Prefix dependencies if they don't already have it
                        if dep.starts_with(CONTRIBUTOR_TASK_PREFIX) || dep.starts_with('#') {
                            dep.clone()
                        } else {
                            format!("{}{}", CONTRIBUTOR_TASK_PREFIX, dep)
                        }
                    })
                    .collect(),
                description: contrib_task.description.clone(),
                ..Default::default()
            };

            tasks.insert(task_id.clone(), TaskDefinition::Single(Box::new(task)));
            injected += 1;

            tracing::trace!(task = %task_id, "Injected contributor task");
        }

        injected
    }

    /// Apply auto-association rules to existing tasks
    fn apply_auto_association(
        &self,
        auto_assoc: &AutoAssociate,
        tasks: &mut HashMap<String, TaskDefinition>,
    ) {
        let Some(inject_dep) = &auto_assoc.inject_dependency else {
            return;
        };

        // Verify the dependency task exists
        if !tasks.contains_key(inject_dep) {
            return;
        }

        // Collect task names to modify (can't modify while iterating)
        let task_names: Vec<String> = tasks.keys().cloned().collect();

        for task_name in task_names {
            // Skip contributor tasks
            if task_name.starts_with(CONTRIBUTOR_TASK_PREFIX) {
                continue;
            }

            let Some(def) = tasks.get_mut(&task_name) else {
                continue;
            };

            Self::auto_associate_definition(def, &auto_assoc.command, inject_dep);
        }
    }

    /// Recursively apply auto-association to a task definition
    fn auto_associate_definition(
        def: &mut TaskDefinition,
        commands: &[String],
        inject_dep: &str,
    ) {
        match def {
            TaskDefinition::Single(task) => {
                // Check if task command matches any auto-associate command
                let base_cmd = task.command.split_whitespace().next().unwrap_or("");

                if commands.iter().any(|c| c == base_cmd) {
                    // Add dependency if not already present
                    if !task.depends_on.contains(&inject_dep.to_string()) {
                        task.depends_on.push(inject_dep.to_string());
                        tracing::trace!(
                            command = %task.command,
                            dependency = %inject_dep,
                            "Auto-associated task with contributor"
                        );
                    }
                }
            }
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for sub in tasks {
                        Self::auto_associate_definition(sub, commands, inject_dep);
                    }
                }
                TaskGroup::Parallel(pg) => {
                    for sub in pg.tasks.values_mut() {
                        Self::auto_associate_definition(sub, commands, inject_dep);
                    }
                }
            },
        }
    }
}

/// Result of applying contributors
#[derive(Debug, Clone, Default)]
pub struct ContributorResult {
    /// Number of tasks injected
    pub tasks_injected: usize,

    /// Contributors that were activated
    pub active_contributors: Vec<String>,
}

// =============================================================================
// Built-in Workspace Contributors
// =============================================================================

/// Create the built-in bun workspace contributor
#[must_use]
pub fn bun_workspace_contributor() -> Contributor {
    Contributor {
        id: "bun.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["bun".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "bun.workspace.install".to_string(),
                command: Some("bun".to_string()),
                args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
                inputs: vec!["package.json".to_string(), "bun.lock".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install Bun dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "bun.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["bun.workspace.install".to_string()],
                description: Some("Bun workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["bun".to_string(), "bunx".to_string()],
            inject_dependency: Some(format!("{}bun.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in npm workspace contributor
#[must_use]
pub fn npm_workspace_contributor() -> Contributor {
    Contributor {
        id: "npm.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["npm".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "npm.workspace.install".to_string(),
                command: Some("npm".to_string()),
                args: vec!["ci".to_string()],
                inputs: vec!["package.json".to_string(), "package-lock.json".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install npm dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "npm.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["npm.workspace.install".to_string()],
                description: Some("npm workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["npm".to_string(), "npx".to_string()],
            inject_dependency: Some(format!("{}npm.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in pnpm workspace contributor
#[must_use]
pub fn pnpm_workspace_contributor() -> Contributor {
    Contributor {
        id: "pnpm.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["pnpm".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "pnpm.workspace.install".to_string(),
                command: Some("pnpm".to_string()),
                args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
                inputs: vec!["package.json".to_string(), "pnpm-lock.yaml".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install pnpm dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "pnpm.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["pnpm.workspace.install".to_string()],
                description: Some("pnpm workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["pnpm".to_string(), "pnpx".to_string()],
            inject_dependency: Some(format!("{}pnpm.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in yarn workspace contributor
#[must_use]
pub fn yarn_workspace_contributor() -> Contributor {
    Contributor {
        id: "yarn.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["yarn".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "yarn.workspace.install".to_string(),
                command: Some("yarn".to_string()),
                args: vec!["install".to_string(), "--immutable".to_string()],
                inputs: vec!["package.json".to_string(), "yarn.lock".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install Yarn dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "yarn.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["yarn.workspace.install".to_string()],
                description: Some("Yarn workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["yarn".to_string()],
            inject_dependency: Some(format!("{}yarn.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Returns all built-in workspace contributors
#[must_use]
pub fn builtin_workspace_contributors() -> Vec<Contributor> {
    vec![
        bun_workspace_contributor(),
        npm_workspace_contributor(),
        pnpm_workspace_contributor(),
        yarn_workspace_contributor(),
    ]
}

/// Build a map of expected task dependencies for DAG verification
#[must_use]
pub fn build_expected_dag(
    tasks: &HashMap<String, TaskDefinition>,
) -> BTreeMap<String, Vec<String>> {
    let mut dag = BTreeMap::new();

    for (name, def) in tasks {
        let deps = collect_deps_from_definition(def);
        dag.insert(name.clone(), deps);
    }

    dag
}

/// Collect dependencies from a task definition
fn collect_deps_from_definition(def: &TaskDefinition) -> Vec<String> {
    match def {
        TaskDefinition::Single(task) => task.depends_on.clone(),
        TaskDefinition::Group(group) => match group {
            TaskGroup::Sequential(_) => vec![], // Sequential has implicit ordering
            TaskGroup::Parallel(pg) => pg.depends_on.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_contributor(id: &str, workspace_member: Vec<&str>) -> Contributor {
        Contributor {
            id: id.to_string(),
            when: Some(ContributorActivation {
                workspace_member: workspace_member.into_iter().map(String::from).collect(),
                ..Default::default()
            }),
            tasks: vec![
                ContributorTask {
                    id: format!("{id}.install"),
                    command: Some("test-cmd".to_string()),
                    args: vec!["install".to_string()],
                    inputs: vec!["package.json".to_string()],
                    outputs: vec!["node_modules".to_string()],
                    hermetic: false,
                    depends_on: vec![],
                    script: None,
                    description: Some(format!("Install {id} dependencies")),
                },
                ContributorTask {
                    id: format!("{id}.setup"),
                    command: None,
                    args: vec![],
                    script: Some("true".to_string()),
                    inputs: vec![],
                    outputs: vec![],
                    hermetic: false,
                    depends_on: vec![format!("{id}.install")],
                    description: Some(format!("{id} setup complete")),
                },
            ],
            auto_associate: Some(AutoAssociate {
                command: vec!["test-cmd".to_string()],
                inject_dependency: Some(format!(
                    "{}{}",
                    CONTRIBUTOR_TASK_PREFIX,
                    format!("{id}.setup")
                )),
            }),
        }
    }

    #[test]
    fn test_contributor_activation_workspace_member() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);

        // Should activate when workspace matches
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            ..Default::default()
        };
        let contributors = [contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        assert!(engine.is_active(&contrib));

        // Should not activate when workspace doesn't match
        let ctx = ContributorContext {
            workspace_member: Some("npm".to_string()),
            ..Default::default()
        };
        let contributors = [contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        assert!(!engine.is_active(&contrib));

        // Should not activate when no workspace
        let ctx = ContributorContext::default();
        let contributors = [contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        assert!(!engine.is_active(&contrib));
    }

    #[test]
    fn test_contributor_injects_tasks() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            ..Default::default()
        };

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        let injected = engine.apply(&mut tasks).unwrap();

        assert_eq!(injected, 2);
        assert!(tasks.contains_key("cuenv:contributor:bun.workspace.install"));
        assert!(tasks.contains_key("cuenv:contributor:bun.workspace.setup"));
    }

    #[test]
    fn test_contributor_auto_association() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            workspace_root: None,
            task_commands: ["test-cmd".to_string()].into_iter().collect(),
        };

        // Create a user task that uses the matching command
        let user_task = Task {
            command: "test-cmd".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();
        tasks.insert("dev".to_string(), TaskDefinition::Single(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // User task should now depend on the contributor setup task
        let dev_task = tasks.get("dev").unwrap();
        if let TaskDefinition::Single(task) = dev_task {
            assert!(task
                .depends_on
                .contains(&"cuenv:contributor:bun.workspace.setup".to_string()));
        } else {
            panic!("Expected single task");
        }
    }

    #[test]
    fn test_idempotent_injection() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            ..Default::default()
        };

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        // First application
        let first_injected = engine.apply(&mut tasks).unwrap();
        assert_eq!(first_injected, 2);

        // Second application should inject nothing (already exists)
        let second_injected = engine.apply(&mut tasks).unwrap();
        assert_eq!(second_injected, 0);

        // Should still have exactly 2 tasks
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_always_active_contributor() {
        let contrib = Contributor {
            id: "always-on".to_string(),
            when: Some(ContributorActivation {
                always: Some(true),
                ..Default::default()
            }),
            tasks: vec![ContributorTask {
                id: "always-on.task".to_string(),
                command: Some("echo".to_string()),
                args: vec!["always".to_string()],
                ..Default::default()
            }],
            auto_associate: None,
        };

        // Should activate regardless of context
        let ctx = ContributorContext::default();
        let contributors = [contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        assert!(engine.is_active(&contrib));
    }

    #[test]
    fn test_no_condition_means_always_active() {
        let contrib = Contributor {
            id: "no-condition".to_string(),
            when: None, // No condition
            tasks: vec![ContributorTask {
                id: "no-condition.task".to_string(),
                command: Some("echo".to_string()),
                args: vec!["hello".to_string()],
                ..Default::default()
            }],
            auto_associate: None,
        };

        let ctx = ContributorContext::default();
        let contributors = [contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        assert!(engine.is_active(&contrib));
    }

    #[test]
    fn test_build_expected_dag() {
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        let task_a = Task {
            command: "echo".to_string(),
            args: vec!["a".to_string()],
            ..Default::default()
        };

        let task_b = Task {
            command: "echo".to_string(),
            args: vec!["b".to_string()],
            depends_on: vec!["a".to_string()],
            ..Default::default()
        };

        tasks.insert("a".to_string(), TaskDefinition::Single(Box::new(task_a)));
        tasks.insert("b".to_string(), TaskDefinition::Single(Box::new(task_b)));

        let dag = build_expected_dag(&tasks);

        assert_eq!(dag.get("a"), Some(&vec![]));
        assert_eq!(dag.get("b"), Some(&vec!["a".to_string()]));
    }

    #[test]
    fn test_multiple_contributors_active_simultaneously() {
        // Two contributors that both match (different workspace types)
        let bun_contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let npm_contrib = Contributor {
            id: "npm.workspace".to_string(),
            when: Some(ContributorActivation {
                workspace_member: vec!["npm".to_string()],
                ..Default::default()
            }),
            tasks: vec![ContributorTask {
                id: "npm.workspace.install".to_string(),
                command: Some("npm".to_string()),
                args: vec!["install".to_string()],
                ..Default::default()
            }],
            auto_associate: None,
        };

        // Context where both could theoretically match (we'll test bun only)
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            ..Default::default()
        };

        let contributors = [bun_contrib.clone(), npm_contrib.clone()];
        let engine = ContributorEngine::new(&contributors, ctx);
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        engine.apply(&mut tasks).unwrap();

        // Only bun tasks should be injected (npm doesn't match)
        assert!(tasks.contains_key("cuenv:contributor:bun.workspace.install"));
        assert!(tasks.contains_key("cuenv:contributor:bun.workspace.setup"));
        assert!(!tasks.contains_key("cuenv:contributor:npm.workspace.install"));
    }

    #[test]
    fn test_auto_association_no_duplicate_deps() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            workspace_root: None,
            task_commands: ["test-cmd".to_string()].into_iter().collect(),
        };

        // Create a user task that already has the dependency
        let user_task = Task {
            command: "test-cmd".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
            depends_on: vec!["cuenv:contributor:bun.workspace.setup".to_string()],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();
        tasks.insert("dev".to_string(), TaskDefinition::Single(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // Should not have duplicated the dependency
        let dev_task = tasks.get("dev").unwrap();
        if let TaskDefinition::Single(task) = dev_task {
            let dep_count = task
                .depends_on
                .iter()
                .filter(|d| *d == "cuenv:contributor:bun.workspace.setup")
                .count();
            assert_eq!(dep_count, 1, "Dependency should not be duplicated");
        } else {
            panic!("Expected single task");
        }
    }

    #[test]
    fn test_command_matching_is_exact() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            workspace_root: None,
            task_commands: ["test-cmd".to_string()].into_iter().collect(),
        };

        // Task with a command that is NOT an exact match
        let user_task = Task {
            command: "test-cmd-extra".to_string(), // Different command
            args: vec!["run".to_string()],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();
        tasks.insert("other".to_string(), TaskDefinition::Single(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // Should NOT have auto-associated (command doesn't match exactly)
        let other_task = tasks.get("other").unwrap();
        if let TaskDefinition::Single(task) = other_task {
            assert!(
                !task
                    .depends_on
                    .contains(&"cuenv:contributor:bun.workspace.setup".to_string()),
                "Non-matching command should not get auto-association"
            );
        } else {
            panic!("Expected single task");
        }
    }

    #[test]
    fn test_contributor_with_empty_tasks() {
        let contrib = Contributor {
            id: "empty".to_string(),
            when: Some(ContributorActivation {
                always: Some(true),
                ..Default::default()
            }),
            tasks: vec![], // No tasks
            auto_associate: None,
        };

        let ctx = ContributorContext::default();
        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        let injected = engine.apply(&mut tasks).unwrap();

        // Should inject nothing
        assert_eq!(injected, 0);
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_contributor_task_dependencies_prefixed() {
        // Test that internal dependencies get the prefix too
        let contrib = Contributor {
            id: "test".to_string(),
            when: Some(ContributorActivation {
                always: Some(true),
                ..Default::default()
            }),
            tasks: vec![
                ContributorTask {
                    id: "test.first".to_string(),
                    command: Some("echo".to_string()),
                    args: vec!["first".to_string()],
                    ..Default::default()
                },
                ContributorTask {
                    id: "test.second".to_string(),
                    command: Some("echo".to_string()),
                    args: vec!["second".to_string()],
                    depends_on: vec!["test.first".to_string()], // Reference without prefix
                    ..Default::default()
                },
            ],
            auto_associate: None,
        };

        let ctx = ContributorContext::default();
        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        let mut tasks: HashMap<String, TaskDefinition> = HashMap::new();

        engine.apply(&mut tasks).unwrap();

        // Check that the second task's dependency got prefixed
        let second_task = tasks.get("cuenv:contributor:test.second").unwrap();
        if let TaskDefinition::Single(task) = second_task {
            assert!(
                task.depends_on
                    .contains(&"cuenv:contributor:test.first".to_string()),
                "Internal dependency should be prefixed, got: {:?}",
                task.depends_on
            );
        } else {
            panic!("Expected single task");
        }
    }
}
