//! Advanced DAG builder tests focusing on cross-project references, hooks, and synthetic tasks.
//!
//! These tests extend the basic graph.rs tests to cover more complex scenarios:
//! - Cross-project task references (TaskRef, ProjectReference)
//! - Synthetic task generation from workspace hooks
//! - HookItem variants (TaskRef, MatchHook, inline Task)
//! - Complex dependency chains across projects
//! - Task discovery and matcher integration

use super::*;
use crate::test_utils::{
    create_task, create_task_ref, create_task_with_project_ref, create_workspace_task,
};

// ============================================================================
// Basic Cross-Project Reference Tests
// ============================================================================

#[test]
fn test_dag_with_task_ref_placeholder() {
    // Test that a task with a TaskRef can be added to the graph
    let mut graph = TaskGraph::new();

    let task1 = create_task("task1", vec![], vec![]);
    let task2 = create_task_ref("#other-project:build", vec!["task1"]);

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["task1", "task2"]);
}

#[test]
fn test_dag_with_project_reference_input() {
    // Test that a task with project reference input is properly added
    let mut graph = TaskGraph::new();

    let build_task = create_task("build", vec![], vec![]);
    let deploy_task = create_task_with_project_ref(
        "deploy",
        vec!["build"],
        "../other-project",
        "compile",
        vec![("dist/app.js", "vendor/app.js")],
    );

    graph.add_task("build", build_task).unwrap();
    graph.add_task("deploy", deploy_task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted[0].name, "build");
    assert_eq!(sorted[1].name, "deploy");
}

#[test]
fn test_dag_with_multiple_project_references() {
    // Test DAG with multiple cross-project dependencies
    let mut graph = TaskGraph::new();

    // Project A tasks
    let task_a1 = create_task("projectA.build", vec![], vec![]);
    let task_a2 = create_task("projectA.test", vec!["projectA.build"], vec![]);

    // Project B tasks
    let task_b1 = create_task("projectB.build", vec![], vec![]);
    let task_b2 = create_task("projectB.test", vec!["projectB.build"], vec![]);

    // Cross-project integration task
    let integration = create_task(
        "integration",
        vec!["projectA.test", "projectB.test"],
        vec![],
    );

    graph.add_task("projectA.build", task_a1).unwrap();
    graph.add_task("projectA.test", task_a2).unwrap();
    graph.add_task("projectB.build", task_b1).unwrap();
    graph.add_task("projectB.test", task_b2).unwrap();
    graph.add_task("integration", integration).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 5);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // Level 0: projectA.build, projectB.build (parallel)
    // Level 1: projectA.test, projectB.test (parallel)
    // Level 2: integration
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 2);
    assert_eq!(groups[1].len(), 2);
    assert_eq!(groups[2].len(), 1);
}

// ============================================================================
// Synthetic Task Tests (Workspace Hooks)
// ============================================================================

#[test]
fn test_synthetic_hook_task_naming() {
    // Test that synthetic hook tasks follow the expected naming convention
    let mut graph = TaskGraph::new();

    // Simulating workspace hook structure: ws_name.hooks.beforeInstall[step_idx]
    let hook_task1 = create_task("bun.hooks.beforeInstall[0]", vec![], vec![]);
    let hook_task2 = create_task(
        "bun.hooks.beforeInstall[1]",
        vec!["bun.hooks.beforeInstall[0]"],
        vec![],
    );
    let install_task = create_task("bun.install", vec!["bun.hooks.beforeInstall[1]"], vec![]);
    let setup_task = create_task("bun.setup", vec!["bun.install"], vec![]);

    graph
        .add_task("bun.hooks.beforeInstall[0]", hook_task1)
        .unwrap();
    graph
        .add_task("bun.hooks.beforeInstall[1]", hook_task2)
        .unwrap();
    graph.add_task("bun.install", install_task).unwrap();
    graph.add_task("bun.setup", setup_task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "bun.hooks.beforeInstall[0]",
            "bun.hooks.beforeInstall[1]",
            "bun.install",
            "bun.setup"
        ]
    );
}

#[test]
fn test_synthetic_hook_with_named_matcher() {
    // Test synthetic tasks from MatchHook with name: ws_name.hooks.beforeInstall.{step_name}[i]
    let mut graph = TaskGraph::new();

    // Named matcher "projen" matching 2 tasks
    let hook1 = create_task("bun.hooks.beforeInstall.projen[0]", vec![], vec!["projen"]);
    let hook2 = create_task("bun.hooks.beforeInstall.projen[1]", vec![], vec!["projen"]);

    // Install depends on both matched tasks
    let install = create_task(
        "bun.install",
        vec![
            "bun.hooks.beforeInstall.projen[0]",
            "bun.hooks.beforeInstall.projen[1]",
        ],
        vec![],
    );

    graph
        .add_task("bun.hooks.beforeInstall.projen[0]", hook1)
        .unwrap();
    graph
        .add_task("bun.hooks.beforeInstall.projen[1]", hook2)
        .unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 3);
    assert!(!graph.has_cycles());

    // Both hook tasks can run in parallel (parallel: true by default)
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), 2); // Both projen hooks in parallel
    assert_eq!(groups[1].len(), 1); // bun.install
}

#[test]
fn test_synthetic_hook_sequential_matcher() {
    // Test when matcher.parallel = false, tasks chain sequentially
    let mut graph = TaskGraph::new();

    // Sequential matcher (parallel: false) - tasks chain
    let hook1 = create_task("bun.hooks.beforeInstall.codegen[0]", vec![], vec![]);
    let hook2 = create_task(
        "bun.hooks.beforeInstall.codegen[1]",
        vec!["bun.hooks.beforeInstall.codegen[0]"],
        vec![],
    );
    let hook3 = create_task(
        "bun.hooks.beforeInstall.codegen[2]",
        vec!["bun.hooks.beforeInstall.codegen[1]"],
        vec![],
    );

    let install = create_task(
        "bun.install",
        vec!["bun.hooks.beforeInstall.codegen[2]"],
        vec![],
    );

    graph
        .add_task("bun.hooks.beforeInstall.codegen[0]", hook1)
        .unwrap();
    graph
        .add_task("bun.hooks.beforeInstall.codegen[1]", hook2)
        .unwrap();
    graph
        .add_task("bun.hooks.beforeInstall.codegen[2]", hook3)
        .unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    // All tasks are sequential
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 4);
    for group in &groups {
        assert_eq!(group.len(), 1);
    }
}

#[test]
fn test_multiple_hook_steps_in_order() {
    // Test that multiple hook steps (TaskRef, Match, Task) chain correctly
    let mut graph = TaskGraph::new();

    // Step 0: TaskRef
    let step0 = create_task_ref("#projen-generator:types", vec![]);

    // Step 1: Match (expands to 2 tasks in parallel)
    let step1_a = create_task(
        "bun.hooks.beforeInstall.projen[0]",
        vec!["bun.hooks.beforeInstall[0]"],
        vec![],
    );
    let step1_b = create_task(
        "bun.hooks.beforeInstall.projen[1]",
        vec!["bun.hooks.beforeInstall[0]"],
        vec![],
    );

    // Step 2: Inline Task
    let step2 = create_task(
        "bun.hooks.beforeInstall[2]",
        vec![
            "bun.hooks.beforeInstall.projen[0]",
            "bun.hooks.beforeInstall.projen[1]",
        ],
        vec![],
    );

    // Install depends on all hooks
    let install = create_task(
        "bun.install",
        vec![
            "bun.hooks.beforeInstall[0]",
            "bun.hooks.beforeInstall.projen[0]",
            "bun.hooks.beforeInstall.projen[1]",
            "bun.hooks.beforeInstall[2]",
        ],
        vec![],
    );

    graph.add_task("bun.hooks.beforeInstall[0]", step0).unwrap();
    graph
        .add_task("bun.hooks.beforeInstall.projen[0]", step1_a)
        .unwrap();
    graph
        .add_task("bun.hooks.beforeInstall.projen[1]", step1_b)
        .unwrap();
    graph.add_task("bun.hooks.beforeInstall[2]", step2).unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 5);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();

    // Verify ordering: step0 -> step1_a/step1_b (parallel) -> step2 -> install
    let pos_step0 = names
        .iter()
        .position(|&n| n == "bun.hooks.beforeInstall[0]")
        .unwrap();
    let pos_step1a = names
        .iter()
        .position(|&n| n == "bun.hooks.beforeInstall.projen[0]")
        .unwrap();
    let pos_step1b = names
        .iter()
        .position(|&n| n == "bun.hooks.beforeInstall.projen[1]")
        .unwrap();
    let pos_step2 = names
        .iter()
        .position(|&n| n == "bun.hooks.beforeInstall[2]")
        .unwrap();
    let pos_install = names.iter().position(|&n| n == "bun.install").unwrap();

    assert!(pos_step0 < pos_step1a);
    assert!(pos_step0 < pos_step1b);
    assert!(pos_step1a < pos_step2);
    assert!(pos_step1b < pos_step2);
    assert!(pos_step2 < pos_install);
}

// ============================================================================
// Workspace Setup Chain Tests
// ============================================================================

#[test]
fn test_workspace_setup_chain() {
    // Test the full workspace setup chain: hooks -> install -> setup -> user_task
    let mut graph = TaskGraph::new();

    let hook1 = create_task("bun.hooks.beforeInstall[0]", vec![], vec![]);
    let install = create_task("bun.install", vec!["bun.hooks.beforeInstall[0]"], vec![]);
    let setup = create_task("bun.setup", vec!["bun.install"], vec![]);
    let user_task = create_workspace_task("dev", vec!["bun.setup"], vec!["bun"]);

    graph.add_task("bun.hooks.beforeInstall[0]", hook1).unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph.add_task("bun.setup", setup).unwrap();
    graph.add_task("dev", user_task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "bun.hooks.beforeInstall[0]",
            "bun.install",
            "bun.setup",
            "dev"
        ]
    );
}

#[test]
fn test_multiple_workspaces_setup() {
    // Test multiple workspace setup chains that can run in parallel
    let mut graph = TaskGraph::new();

    // Bun workspace chain
    let bun_install = create_task("bun.install", vec![], vec![]);
    let bun_setup = create_task("bun.setup", vec!["bun.install"], vec![]);

    // Cargo workspace chain
    let cargo_install = create_task("cargo.install", vec![], vec![]);
    let cargo_setup = create_task("cargo.setup", vec!["cargo.install"], vec![]);

    // Task using both workspaces
    let build = create_workspace_task(
        "build",
        vec!["bun.setup", "cargo.setup"],
        vec!["bun", "cargo"],
    );

    graph.add_task("bun.install", bun_install).unwrap();
    graph.add_task("bun.setup", bun_setup).unwrap();
    graph.add_task("cargo.install", cargo_install).unwrap();
    graph.add_task("cargo.setup", cargo_setup).unwrap();
    graph.add_task("build", build).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 5);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // Level 0: bun.install, cargo.install (parallel)
    // Level 1: bun.setup, cargo.setup (parallel)
    // Level 2: build
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 2);
    assert_eq!(groups[1].len(), 2);
    assert_eq!(groups[2].len(), 1);
}

// ============================================================================
// Cross-Project Hook Dependency Tests
// ============================================================================

#[test]
fn test_hook_with_cross_project_task_ref() {
    // Test hook that references a task from another project
    let mut graph = TaskGraph::new();

    // External project tasks (would be discovered and added to global DAG)
    let external_install = create_task("projen-generator::install", vec![], vec![]);
    let external_types = create_task(
        "projen-generator::types",
        vec!["projen-generator::install"],
        vec![],
    );

    // Current project hook referencing external task
    let hook = create_task(
        "website.bun.hooks.beforeInstall[0]",
        vec!["projen-generator::types"], // Cross-project dependency
        vec![],
    );

    let install = create_task(
        "website.bun.install",
        vec!["website.bun.hooks.beforeInstall[0]"],
        vec![],
    );

    graph
        .add_task("projen-generator::install", external_install)
        .unwrap();
    graph
        .add_task("projen-generator::types", external_types)
        .unwrap();
    graph
        .add_task("website.bun.hooks.beforeInstall[0]", hook)
        .unwrap();
    graph.add_task("website.bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();

    // Verify cross-project ordering
    let pos_ext_install = names
        .iter()
        .position(|&n| n == "projen-generator::install")
        .unwrap();
    let pos_ext_types = names
        .iter()
        .position(|&n| n == "projen-generator::types")
        .unwrap();
    let pos_hook = names
        .iter()
        .position(|&n| n == "website.bun.hooks.beforeInstall[0]")
        .unwrap();
    let pos_install = names
        .iter()
        .position(|&n| n == "website.bun.install")
        .unwrap();

    assert!(pos_ext_install < pos_ext_types);
    assert!(pos_ext_types < pos_hook);
    assert!(pos_hook < pos_install);
}

#[test]
fn test_transitive_cross_project_dependencies() {
    // Test: projectA -> projectB -> projectC (transitive chain)
    let mut graph = TaskGraph::new();

    // Project C (no dependencies)
    let c_build = create_task("projectC::build", vec![], vec![]);

    // Project B depends on C
    let b_build = create_task("projectB::build", vec!["projectC::build"], vec![]);

    // Project A depends on B
    let a_build = create_task("projectA::build", vec!["projectB::build"], vec![]);
    let a_deploy = create_task("projectA::deploy", vec!["projectA::build"], vec![]);

    graph.add_task("projectC::build", c_build).unwrap();
    graph.add_task("projectB::build", b_build).unwrap();
    graph.add_task("projectA::build", a_build).unwrap();
    graph.add_task("projectA::deploy", a_deploy).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "projectC::build",
            "projectB::build",
            "projectA::build",
            "projectA::deploy"
        ]
    );
}

// ============================================================================
// Complex Scenario Tests
// ============================================================================

#[test]
fn test_diamond_dependency_with_hooks() {
    // Diamond pattern with hooks:
    //        hook
    //         |
    //       install
    //        / \
    //    taskA   taskB
    //        \ /
    //       final
    let mut graph = TaskGraph::new();

    let hook = create_task("bun.hooks.beforeInstall[0]", vec![], vec![]);
    let install = create_task("bun.install", vec!["bun.hooks.beforeInstall[0]"], vec![]);
    let setup = create_task("bun.setup", vec!["bun.install"], vec![]);
    let task_a = create_task("taskA", vec!["bun.setup"], vec![]);
    let task_b = create_task("taskB", vec!["bun.setup"], vec![]);
    let final_task = create_task("final", vec!["taskA", "taskB"], vec![]);

    graph.add_task("bun.hooks.beforeInstall[0]", hook).unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph.add_task("bun.setup", setup).unwrap();
    graph.add_task("taskA", task_a).unwrap();
    graph.add_task("taskB", task_b).unwrap();
    graph.add_task("final", final_task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 6);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // Level 0: hook
    // Level 1: install
    // Level 2: setup
    // Level 3: taskA, taskB (parallel)
    // Level 4: final
    assert_eq!(groups.len(), 5);
    assert_eq!(groups[3].len(), 2); // taskA and taskB in parallel
}

#[test]
fn test_multiple_projects_with_hooks() {
    // Multiple projects each with their own hook chains
    let mut graph = TaskGraph::new();

    // Project A hooks
    let a_hook = create_task("projectA::bun.hooks.beforeInstall[0]", vec![], vec![]);
    let a_install = create_task(
        "projectA::bun.install",
        vec!["projectA::bun.hooks.beforeInstall[0]"],
        vec![],
    );

    // Project B hooks
    let b_hook = create_task("projectB::npm.hooks.beforeInstall[0]", vec![], vec![]);
    let b_install = create_task(
        "projectB::npm.install",
        vec!["projectB::npm.hooks.beforeInstall[0]"],
        vec![],
    );

    // Cross-project integration
    let integration = create_task(
        "integration",
        vec!["projectA::bun.install", "projectB::npm.install"],
        vec![],
    );

    graph
        .add_task("projectA::bun.hooks.beforeInstall[0]", a_hook)
        .unwrap();
    graph.add_task("projectA::bun.install", a_install).unwrap();
    graph
        .add_task("projectB::npm.hooks.beforeInstall[0]", b_hook)
        .unwrap();
    graph.add_task("projectB::npm.install", b_install).unwrap();
    graph.add_task("integration", integration).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 5);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // Level 0: projectA hook, projectB hook (parallel)
    // Level 1: projectA install, projectB install (parallel)
    // Level 2: integration
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 2);
    assert_eq!(groups[1].len(), 2);
    assert_eq!(groups[2].len(), 1);
}

#[test]
fn test_hook_with_afterinstall() {
    // Test both beforeInstall and afterInstall hooks
    let mut graph = TaskGraph::new();

    let before_hook = create_task("bun.hooks.beforeInstall[0]", vec![], vec![]);
    let install = create_task("bun.install", vec!["bun.hooks.beforeInstall[0]"], vec![]);
    let after_hook = create_task("bun.hooks.afterInstall[0]", vec!["bun.install"], vec![]);
    let setup = create_task("bun.setup", vec!["bun.hooks.afterInstall[0]"], vec![]);

    graph
        .add_task("bun.hooks.beforeInstall[0]", before_hook)
        .unwrap();
    graph.add_task("bun.install", install).unwrap();
    graph
        .add_task("bun.hooks.afterInstall[0]", after_hook)
        .unwrap();
    graph.add_task("bun.setup", setup).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "bun.hooks.beforeInstall[0]",
            "bun.install",
            "bun.hooks.afterInstall[0]",
            "bun.setup"
        ]
    );
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn test_cross_project_cycle_detection() {
    // Test cycle detection across project boundaries
    let mut graph = TaskGraph::new();

    // Create a cycle: projectA::build -> projectB::build -> projectA::build
    let a_build = create_task("projectA::build", vec!["projectB::build"], vec![]);
    let b_build = create_task("projectB::build", vec!["projectA::build"], vec![]);

    graph.add_task("projectA::build", a_build).unwrap();
    graph.add_task("projectB::build", b_build).unwrap();
    graph.add_dependency_edges().unwrap();

    assert!(graph.has_cycles());
    assert!(graph.topological_sort().is_err());
}

#[test]
fn test_missing_cross_project_dependency() {
    // Test error when referencing a non-existent cross-project task
    let mut graph = TaskGraph::new();

    let task = create_task("myTask", vec!["nonexistent-project::build"], vec![]);
    graph.add_task("myTask", task).unwrap();

    let result = graph.add_dependency_edges();
    assert!(result.is_err());

    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Missing dependencies"));
    assert!(err_msg.contains("nonexistent-project::build"));
}

#[test]
fn test_self_referencing_hook() {
    // Test that a hook cannot depend on itself
    let mut graph = TaskGraph::new();

    let hook = create_task(
        "bun.hooks.beforeInstall[0]",
        vec!["bun.hooks.beforeInstall[0]"],
        vec![],
    );
    graph.add_task("bun.hooks.beforeInstall[0]", hook).unwrap();
    graph.add_dependency_edges().unwrap();

    assert!(graph.has_cycles());
}

#[test]
fn test_empty_hook_list() {
    // Test workspace with no hooks still creates proper setup chain
    let mut graph = TaskGraph::new();

    let install = create_task("bun.install", vec![], vec![]);
    let setup = create_task("bun.setup", vec!["bun.install"], vec![]);
    let user_task = create_workspace_task("dev", vec!["bun.setup"], vec!["bun"]);

    graph.add_task("bun.install", install).unwrap();
    graph.add_task("bun.setup", setup).unwrap();
    graph.add_task("dev", user_task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 3);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    let names: Vec<&str> = sorted.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["bun.install", "bun.setup", "dev"]);
}

// ============================================================================
// Task Labels and Matching Tests
// ============================================================================

#[test]
fn test_tasks_with_labels() {
    // Test that tasks with labels are properly added to the graph
    let mut graph = TaskGraph::new();

    let codegen1 = create_task("codegen1", vec![], vec!["projen", "codegen"]);
    let codegen2 = create_task("codegen2", vec![], vec!["projen", "codegen"]);
    let build = create_task("build", vec!["codegen1", "codegen2"], vec![]);

    graph.add_task("codegen1", codegen1).unwrap();
    graph.add_task("codegen2", codegen2).unwrap();
    graph.add_task("build", build).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 3);
    assert!(!graph.has_cycles());

    // Verify labels are preserved
    let sorted = graph.topological_sort().unwrap();
    let codegen1_node = sorted.iter().find(|n| n.name == "codegen1").unwrap();
    assert_eq!(codegen1_node.task.labels, vec!["projen", "codegen"]);
}

#[test]
fn test_parallel_groups_with_labels() {
    // Test that parallel groups compute correctly with labeled tasks
    let mut graph = TaskGraph::new();

    // Multiple codegen tasks (would be matched by TaskMatcher)
    let gen1 = create_task("gen.project1", vec![], vec!["codegen"]);
    let gen2 = create_task("gen.project2", vec![], vec!["codegen"]);
    let gen3 = create_task("gen.project3", vec![], vec!["codegen"]);

    // All codegen tasks run before build
    let build = create_task(
        "build",
        vec!["gen.project1", "gen.project2", "gen.project3"],
        vec![],
    );

    graph.add_task("gen.project1", gen1).unwrap();
    graph.add_task("gen.project2", gen2).unwrap();
    graph.add_task("gen.project3", gen3).unwrap();
    graph.add_task("build", build).unwrap();
    graph.add_dependency_edges().unwrap();

    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), 3); // All gen tasks in parallel
    assert_eq!(groups[1].len(), 1); // build
}

// ============================================================================
// build_for_task Tests with Cross-Project Dependencies
// ============================================================================

#[test]
fn test_build_for_task_includes_cross_project_deps() {
    // Test that build_for_task includes cross-project dependencies
    let mut tasks = Tasks::new();

    // External project task
    tasks.tasks.insert(
        "external::build".to_string(),
        TaskDefinition::Single(Box::new(create_task("external::build", vec![], vec![]))),
    );

    // Local task depending on external
    tasks.tasks.insert(
        "deploy".to_string(),
        TaskDefinition::Single(Box::new(create_task(
            "deploy",
            vec!["external::build"],
            vec![],
        ))),
    );

    let mut graph = TaskGraph::new();
    graph.build_for_task("deploy", &tasks).unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(graph.contains_task("deploy"));
    assert!(graph.contains_task("external::build"));
    assert!(!graph.has_cycles());
}

#[test]
fn test_build_for_task_with_hook_chain() {
    // Test build_for_task includes the full hook chain
    let mut tasks = Tasks::new();

    tasks.tasks.insert(
        "bun.hooks.beforeInstall[0]".to_string(),
        TaskDefinition::Single(Box::new(create_task("hook", vec![], vec![]))),
    );

    tasks.tasks.insert(
        "bun.install".to_string(),
        TaskDefinition::Single(Box::new(create_task(
            "install",
            vec!["bun.hooks.beforeInstall[0]"],
            vec![],
        ))),
    );

    tasks.tasks.insert(
        "bun.setup".to_string(),
        TaskDefinition::Single(Box::new(create_task("setup", vec!["bun.install"], vec![]))),
    );

    tasks.tasks.insert(
        "dev".to_string(),
        TaskDefinition::Single(Box::new(create_workspace_task(
            "dev",
            vec!["bun.setup"],
            vec!["bun"],
        ))),
    );

    let mut graph = TaskGraph::new();
    graph.build_for_task("dev", &tasks).unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(graph.contains_task("bun.hooks.beforeInstall[0]"));
    assert!(graph.contains_task("bun.install"));
    assert!(graph.contains_task("bun.setup"));
    assert!(graph.contains_task("dev"));
}

// ============================================================================
// Large-Scale Tests
// ============================================================================

#[test]
fn test_large_dag_with_many_hooks() {
    // Test with many hook tasks (performance/scalability)
    let mut graph = TaskGraph::new();

    let num_hooks = 20;
    let mut prev_hook = String::new();

    for i in 0..num_hooks {
        let hook_name = format!("bun.hooks.beforeInstall[{}]", i);
        let deps = if prev_hook.is_empty() {
            vec![]
        } else {
            vec![prev_hook.as_str()]
        };
        let hook = create_task(&hook_name, deps, vec![]);
        graph.add_task(&hook_name, hook).unwrap();
        prev_hook = hook_name;
    }

    // Install depends on last hook
    let install = create_task("bun.install", vec![&prev_hook], vec![]);
    graph.add_task("bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), num_hooks + 1);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // All sequential (chain of 20 hooks + install)
    assert_eq!(groups.len(), num_hooks + 1);
}

#[test]
fn test_dag_with_many_parallel_projects() {
    // Test with many projects that can run in parallel
    let mut graph = TaskGraph::new();

    let num_projects = 15;

    for i in 0..num_projects {
        let project_name = format!("project{}::build", i);
        let task = create_task(&project_name, vec![], vec![]);
        graph.add_task(&project_name, task).unwrap();
    }

    // Integration task depends on all projects
    let integration_deps: Vec<String> = (0..num_projects)
        .map(|i| format!("project{}::build", i))
        .collect();

    let integration = Task {
        command: "echo integration".to_string(),
        depends_on: integration_deps,
        ..Default::default()
    };
    graph.add_task("integration", integration).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), num_projects + 1);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), num_projects); // All projects in parallel
    assert_eq!(groups[1].len(), 1); // integration
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_task_with_very_long_name() {
    // Test handling of very long task names
    let mut graph = TaskGraph::new();

    let long_name = "a".repeat(500);
    let task = create_task(&long_name, vec![], vec![]);
    graph.add_task(&long_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(&long_name));
}

#[test]
fn test_task_with_unicode_name() {
    // Test handling of Unicode characters in task names
    let mut graph = TaskGraph::new();

    let unicode_name = "任务_测试_日本語_émoji🚀";
    let task = create_task(unicode_name, vec![], vec![]);
    graph.add_task(unicode_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(unicode_name));
}

#[test]
fn test_cross_project_with_unicode_names() {
    // Test cross-project dependencies with Unicode project names
    let mut graph = TaskGraph::new();

    let proj_a = "项目A::构建";
    let proj_b = "プロジェクトB::ビルド";

    let task_a = create_task(proj_a, vec![], vec![]);
    let task_b = create_task(proj_b, vec![proj_a], vec![]);

    graph.add_task(proj_a, task_a).unwrap();
    graph.add_task(proj_b, task_b).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted[0].name, proj_a);
    assert_eq!(sorted[1].name, proj_b);
}

#[test]
fn test_task_with_special_characters() {
    // Test handling of special characters in task names
    let mut graph = TaskGraph::new();

    let special_name = "task-with.dots_and-dashes";
    let task = create_task(special_name, vec![], vec![]);
    graph.add_task(special_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(special_name));
}

#[test]
fn test_synthetic_hook_with_special_index() {
    // Test synthetic hook naming with various index formats
    let mut graph = TaskGraph::new();

    // Test multiple hook naming patterns
    let patterns = vec![
        "ws.hooks.beforeInstall[0]",
        "ws.hooks.beforeInstall[99]",
        "ws.hooks.beforeInstall.matcher[0]",
        "ws.hooks.afterInstall[0]",
    ];

    for pattern in &patterns {
        let task = create_task(pattern, vec![], vec![]);
        graph.add_task(pattern, task).unwrap();
    }

    assert_eq!(graph.task_count(), 4);
    for pattern in &patterns {
        assert!(graph.contains_task(pattern));
    }
}

#[test]
fn test_empty_depends_on_list() {
    // Test that tasks with empty dependency lists are handled correctly
    let mut graph = TaskGraph::new();

    let task1 = Task {
        command: "echo task1".to_string(),
        depends_on: vec![], // Empty
        ..Default::default()
    };

    let task2 = Task {
        command: "echo task2".to_string(),
        depends_on: vec![], // Empty
        ..Default::default()
    };

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    // Both should be in the same parallel group (no dependencies between them)
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].len(), 2);
}

#[test]
fn test_project_separator_variations() {
    // Test various project separator formats
    let mut graph = TaskGraph::new();

    // Different project::task formats
    let formats = [
        "project::task",
        "project::nested.task",
        "project::deeply.nested.task",
        "my-project::my-task",
        "my_project::my_task",
    ];

    for (i, name) in formats.iter().enumerate() {
        let deps: Vec<&str> = if i > 0 { vec![formats[i - 1]] } else { vec![] };
        let task = create_task(name, deps, vec![]);
        graph.add_task(name, task).unwrap();
    }

    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), formats.len());
    assert!(!graph.has_cycles());

    // Should be sequential
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), formats.len());
}

#[test]
fn test_multiple_tasks_same_project() {
    // Test multiple tasks from the same project
    let mut graph = TaskGraph::new();

    let tasks = vec![
        ("myproject::build", vec![]),
        ("myproject::test", vec!["myproject::build"]),
        ("myproject::lint", vec!["myproject::build"]),
        (
            "myproject::deploy",
            vec!["myproject::test", "myproject::lint"],
        ),
    ];

    for (name, deps) in &tasks {
        let task = create_task(name, deps.to_vec(), vec![]);
        graph.add_task(name, task).unwrap();
    }

    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // build -> (test, lint) in parallel -> deploy
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 1); // build
    assert_eq!(groups[1].len(), 2); // test, lint
    assert_eq!(groups[2].len(), 1); // deploy
}

#[test]
fn test_deeply_nested_task_groups() {
    // Test deeply nested parallel and sequential groups
    let mut graph = TaskGraph::new();

    // Create a chain: a -> (b1, b2) -> c -> (d1, d2, d3) -> e
    let task_a = create_task("a", vec![], vec![]);
    let task_b1 = create_task("b1", vec!["a"], vec![]);
    let task_b2 = create_task("b2", vec!["a"], vec![]);
    let task_c = create_task("c", vec!["b1", "b2"], vec![]);
    let task_d1 = create_task("d1", vec!["c"], vec![]);
    let task_d2 = create_task("d2", vec!["c"], vec![]);
    let task_d3 = create_task("d3", vec!["c"], vec![]);
    let task_e = create_task("e", vec!["d1", "d2", "d3"], vec![]);

    graph.add_task("a", task_a).unwrap();
    graph.add_task("b1", task_b1).unwrap();
    graph.add_task("b2", task_b2).unwrap();
    graph.add_task("c", task_c).unwrap();
    graph.add_task("d1", task_d1).unwrap();
    graph.add_task("d2", task_d2).unwrap();
    graph.add_task("d3", task_d3).unwrap();
    graph.add_task("e", task_e).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 8);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 5);
    assert_eq!(groups[0].len(), 1); // a
    assert_eq!(groups[1].len(), 2); // b1, b2
    assert_eq!(groups[2].len(), 1); // c
    assert_eq!(groups[3].len(), 3); // d1, d2, d3
    assert_eq!(groups[4].len(), 1); // e
}
