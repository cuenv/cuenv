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

mod context;
mod dag;
mod engine;
mod model;
mod workspace;

pub use context::ContributorContext;
pub use dag::build_expected_dag;
pub use engine::ContributorEngine;
pub use model::{
    AutoAssociate, CONTRIBUTOR_TASK_PREFIX, Contributor, ContributorActivation, ContributorResult,
    ContributorTask,
};
pub use workspace::{
    builtin_workspace_contributors, bun_workspace_contributor, npm_workspace_contributor,
    pnpm_workspace_contributor, yarn_workspace_contributor,
};

#[cfg(test)]
use crate::tasks::{Task, TaskDependency, TaskNode};
#[cfg(test)]
use std::collections::HashMap;

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
                inject_dependency: Some(format!("{CONTRIBUTOR_TASK_PREFIX}{id}.setup")),
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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

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
            ..Default::default()
        };

        // Create a user task that uses the matching command
        let user_task = Task {
            command: "test-cmd".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskNode> = HashMap::new();
        tasks.insert("dev".to_string(), TaskNode::Task(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // User task should now depend on the contributor setup task
        let dev_task = tasks.get("dev").unwrap();
        if let TaskNode::Task(task) = dev_task {
            assert!(
                task.depends_on
                    .iter()
                    .any(|d| d.task_name() == "cuenv:contributor:bun.workspace.setup")
            );
        } else {
            panic!("Expected single task");
        }
    }

    #[test]
    fn test_contributor_auto_association_with_env_prefixed_command() {
        let contrib = create_test_contributor("bun.workspace", vec!["bun"]);
        let ctx = ContributorContext {
            workspace_member: Some("bun".to_string()),
            workspace_root: None,
            task_commands: ["test-cmd".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let user_task = Task {
            command: "env TEST_MODE=1 test-cmd run dev".to_string(),
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskNode> = HashMap::new();
        tasks.insert("dev".to_string(), TaskNode::Task(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        let dev_task = tasks.get("dev").unwrap();
        if let TaskNode::Task(task) = dev_task {
            assert!(
                task.depends_on
                    .iter()
                    .any(|d| d.task_name() == "cuenv:contributor:bun.workspace.setup")
            );
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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

        let task_a = Task {
            command: "echo".to_string(),
            args: vec!["a".to_string()],
            ..Default::default()
        };

        let task_b = Task {
            command: "echo".to_string(),
            args: vec!["b".to_string()],
            depends_on: vec![TaskDependency::from_name("a")],
            ..Default::default()
        };

        tasks.insert("a".to_string(), TaskNode::Task(Box::new(task_a)));
        tasks.insert("b".to_string(), TaskNode::Task(Box::new(task_b)));

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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

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
            ..Default::default()
        };

        // Create a user task that already has the dependency
        let user_task = Task {
            command: "test-cmd".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
            depends_on: vec![TaskDependency::from_name(
                "cuenv:contributor:bun.workspace.setup",
            )],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskNode> = HashMap::new();
        tasks.insert("dev".to_string(), TaskNode::Task(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // Should not have duplicated the dependency
        let dev_task = tasks.get("dev").unwrap();
        if let TaskNode::Task(task) = dev_task {
            let dep_count = task
                .depends_on
                .iter()
                .filter(|d| d.task_name() == "cuenv:contributor:bun.workspace.setup")
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
            ..Default::default()
        };

        // Task with a command that is NOT an exact match
        let user_task = Task {
            command: "test-cmd-extra".to_string(), // Different command
            args: vec!["run".to_string()],
            ..Default::default()
        };

        let mut tasks: HashMap<String, TaskNode> = HashMap::new();
        tasks.insert("other".to_string(), TaskNode::Task(Box::new(user_task)));

        let contributors = [contrib];
        let engine = ContributorEngine::new(&contributors, ctx);
        engine.apply(&mut tasks).unwrap();

        // Should NOT have auto-associated (command doesn't match exactly)
        let other_task = tasks.get("other").unwrap();
        if let TaskNode::Task(task) = other_task {
            assert!(
                !task
                    .depends_on
                    .iter()
                    .any(|d| d.task_name() == "cuenv:contributor:bun.workspace.setup"),
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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

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
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();

        engine.apply(&mut tasks).unwrap();

        // Check that the second task's dependency got prefixed
        let second_task = tasks.get("cuenv:contributor:test.second").unwrap();
        if let TaskNode::Task(task) = second_task {
            assert!(
                task.depends_on
                    .iter()
                    .any(|d| d.task_name() == "cuenv:contributor:test.first"),
                "Internal dependency should be prefixed, got: {:?}",
                task.depends_on
            );
        } else {
            panic!("Expected single task");
        }
    }
}
