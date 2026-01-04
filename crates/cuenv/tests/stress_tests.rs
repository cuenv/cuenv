//! Stress tests for task graph operations.
//!
//! These tests verify that the task graph handles large-scale scenarios
//! correctly. They are marked as #[ignore] since they may take longer
//! to run and are typically only needed for validation before releases.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use cuenv_task_graph::{TaskGraph, TaskNodeData};
use std::collections::HashSet;

/// Simple task type for stress testing.
#[derive(Clone, Debug)]
struct StressTask {
    deps: Vec<String>,
}

impl TaskNodeData for StressTask {
    fn depends_on(&self) -> &[String] {
        &self.deps
    }

    fn add_dependency(&mut self, dep: String) {
        self.deps.push(dep);
    }
}

impl StressTask {
    fn new(deps: &[&str]) -> Self {
        Self {
            deps: deps.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn with_deps(deps: Vec<String>) -> Self {
        Self { deps }
    }
}

// =============================================================================
// Stress Tests
// =============================================================================

/// Test that 100 tasks can run in parallel when they have no dependencies.
///
/// This verifies that the parallel group computation correctly identifies
/// independent tasks and groups them together.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_100_parallel_tasks() {
    let mut graph = TaskGraph::new();

    // Add 100 tasks with no dependencies
    for i in 0..100 {
        let task = StressTask::new(&[]);
        graph
            .add_task(&format!("parallel_task_{i}"), task)
            .expect("Failed to add task");
    }

    // Wire dependencies (none in this case, but required for graph consistency)
    graph
        .add_dependency_edges()
        .expect("Failed to add dependency edges");

    // Verify no cycles
    assert!(!graph.has_cycles(), "Graph should not have cycles");

    // Get parallel groups
    let groups = graph
        .get_parallel_groups()
        .expect("Failed to get parallel groups");

    // All 100 tasks should be in a single parallel group
    assert_eq!(groups.len(), 1, "Should have exactly 1 parallel group");
    assert_eq!(
        groups[0].len(),
        100,
        "All 100 tasks should be in the first group"
    );

    // Verify all tasks are present
    let task_names: HashSet<String> = groups[0].iter().map(|n| n.name.clone()).collect();
    for i in 0..100 {
        assert!(
            task_names.contains(&format!("parallel_task_{i}")),
            "Missing task parallel_task_{i}"
        );
    }

    // Verify topological sort works
    let sorted = graph
        .topological_sort()
        .expect("Topological sort should succeed");
    assert_eq!(sorted.len(), 100, "Should have 100 tasks in sorted order");
}

/// Test a deep dependency chain of 20 levels.
///
/// This verifies that the graph correctly handles deeply nested dependencies
/// where each task depends on the previous one in a chain.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_deep_dependency_chain_20_levels() {
    const DEPTH: usize = 20;
    let mut graph = TaskGraph::new();

    // Add first task with no dependencies
    graph
        .add_task("chain_task_0", StressTask::new(&[]))
        .expect("Failed to add first task");

    // Add remaining tasks, each depending on the previous
    for i in 1..DEPTH {
        let dep = format!("chain_task_{}", i - 1);
        let task = StressTask::with_deps(vec![dep]);
        graph
            .add_task(&format!("chain_task_{i}"), task)
            .expect("Failed to add task");
    }

    // Wire dependencies
    graph
        .add_dependency_edges()
        .expect("Failed to add dependency edges");

    // Verify no cycles
    assert!(!graph.has_cycles(), "Graph should not have cycles");

    // Get parallel groups - should have 20 groups, one per level
    let groups = graph
        .get_parallel_groups()
        .expect("Failed to get parallel groups");

    assert_eq!(
        groups.len(),
        DEPTH,
        "Should have {DEPTH} levels (one task per level)"
    );

    // Each group should have exactly 1 task
    for (level, group) in groups.iter().enumerate() {
        assert_eq!(group.len(), 1, "Level {level} should have exactly 1 task");
    }

    // Verify topological ordering
    let sorted = graph
        .topological_sort()
        .expect("Topological sort should succeed");

    assert_eq!(sorted.len(), DEPTH, "Should have {DEPTH} tasks");

    // Verify correct ordering: each task should appear before its dependents
    let positions: std::collections::HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();

    for i in 1..DEPTH {
        let current = format!("chain_task_{i}");
        let previous = format!("chain_task_{}", i - 1);
        assert!(
            positions[&previous] < positions[&current],
            "Task {} should come before {}",
            previous,
            current
        );
    }
}

/// Test a large task graph with 1000 tasks.
///
/// This creates a graph with multiple parallel branches that fan out
/// and fan back in, creating a complex dependency structure.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_large_task_graph_1000_tasks() {
    const TOTAL_TASKS: usize = 1000;
    const FAN_WIDTH: usize = 10;
    let mut graph = TaskGraph::new();

    // Structure:
    // - Level 0: 1 root task
    // - Level 1-99: 10 tasks each, all depending on level 0
    // - Final task depends on all tasks from the last level
    //
    // This creates a fan-out/fan-in pattern

    // Add root task
    graph
        .add_task("root", StressTask::new(&[]))
        .expect("Failed to add root task");

    let mut last_level_tasks: Vec<String> = vec!["root".to_string()];
    let mut task_count = 1;

    // Add intermediate levels (fan-out pattern)
    let num_levels = (TOTAL_TASKS - 2) / FAN_WIDTH; // Reserve 1 for root, 1 for final

    for level in 0..num_levels {
        let mut current_level_tasks = Vec::new();

        for i in 0..FAN_WIDTH {
            let task_name = format!("level_{level}_task_{i}");
            let task = StressTask::with_deps(last_level_tasks.clone());
            graph
                .add_task(&task_name, task)
                .expect("Failed to add task");
            current_level_tasks.push(task_name);
            task_count += 1;

            if task_count >= TOTAL_TASKS - 1 {
                break;
            }
        }

        if task_count >= TOTAL_TASKS - 1 {
            last_level_tasks = current_level_tasks;
            break;
        }

        last_level_tasks = current_level_tasks;
    }

    // Add final task depending on all last-level tasks
    let final_task = StressTask::with_deps(last_level_tasks);
    graph
        .add_task("final", final_task)
        .expect("Failed to add final task");

    // Wire dependencies
    graph
        .add_dependency_edges()
        .expect("Failed to add dependency edges");

    // Verify basic properties
    assert!(!graph.has_cycles(), "Graph should not have cycles");

    let actual_task_count = graph.task_count();
    assert!(
        actual_task_count >= 100,
        "Should have at least 100 tasks, got {actual_task_count}"
    );

    // Verify parallel groups can be computed
    let groups = graph
        .get_parallel_groups()
        .expect("Failed to get parallel groups");

    assert!(
        !groups.is_empty(),
        "Should have at least one parallel group"
    );

    // First group should contain only the root
    assert_eq!(groups[0].len(), 1, "First group should have only root");
    assert_eq!(groups[0][0].name, "root", "First task should be root");

    // Last group should contain only the final task
    let last_group = groups.last().expect("Should have groups");
    assert_eq!(
        last_group.len(),
        1,
        "Last group should have only final task"
    );
    assert_eq!(last_group[0].name, "final", "Last task should be final");

    // Verify topological sort succeeds
    let sorted = graph
        .topological_sort()
        .expect("Topological sort should succeed");

    assert_eq!(
        sorted.len(),
        actual_task_count,
        "Sorted list should contain all tasks"
    );

    // Verify root comes first and final comes last
    assert_eq!(sorted.first().map(|n| &n.name), Some(&"root".to_string()));
    assert_eq!(sorted.last().map(|n| &n.name), Some(&"final".to_string()));
}

/// Test that a wide graph (many tasks depending on one root) is handled correctly.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_wide_graph_500_leaves() {
    const LEAF_COUNT: usize = 500;
    let mut graph = TaskGraph::new();

    // Add root task
    graph
        .add_task("root", StressTask::new(&[]))
        .expect("Failed to add root task");

    // Add 500 leaf tasks all depending on root
    for i in 0..LEAF_COUNT {
        let task = StressTask::new(&["root"]);
        graph
            .add_task(&format!("leaf_{i}"), task)
            .expect("Failed to add leaf task");
    }

    graph
        .add_dependency_edges()
        .expect("Failed to add dependency edges");

    assert!(!graph.has_cycles());

    let groups = graph
        .get_parallel_groups()
        .expect("Failed to get parallel groups");

    // Should have 2 levels: root, then all leaves
    assert_eq!(groups.len(), 2, "Should have 2 levels");
    assert_eq!(groups[0].len(), 1, "First level should have 1 task (root)");
    assert_eq!(
        groups[1].len(),
        LEAF_COUNT,
        "Second level should have all {LEAF_COUNT} leaves"
    );
}

/// Test diamond dependency pattern at scale.
///
/// Creates multiple diamond patterns connected in a chain.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_diamond_pattern_chain() {
    const DIAMONDS: usize = 50;
    let mut graph = TaskGraph::new();

    // Each diamond:
    //     top
    //    /   \
    //  left  right
    //    \   /
    //    bottom
    //
    // Diamonds are chained: bottom of diamond N is top of diamond N+1

    let mut prev_bottom: Option<String> = None;

    for d in 0..DIAMONDS {
        let top_name = if let Some(ref prev) = prev_bottom {
            prev.clone()
        } else {
            let name = format!("diamond_{d}_top");
            graph
                .add_task(&name, StressTask::new(&[]))
                .expect("Failed to add top");
            name
        };

        let left_name = format!("diamond_{d}_left");
        let right_name = format!("diamond_{d}_right");
        let bottom_name = format!("diamond_{d}_bottom");

        graph
            .add_task(&left_name, StressTask::new(&[&top_name]))
            .expect("Failed to add left");
        graph
            .add_task(&right_name, StressTask::new(&[&top_name]))
            .expect("Failed to add right");
        graph
            .add_task(&bottom_name, StressTask::new(&[&left_name, &right_name]))
            .expect("Failed to add bottom");

        prev_bottom = Some(bottom_name);
    }

    graph
        .add_dependency_edges()
        .expect("Failed to add dependency edges");

    assert!(!graph.has_cycles());

    let groups = graph
        .get_parallel_groups()
        .expect("Failed to get parallel groups");

    // Each diamond adds 3 levels (top is shared with previous bottom):
    // Level 0: diamond_0_top
    // Level 1: diamond_0_left, diamond_0_right
    // Level 2: diamond_0_bottom (= diamond_1_top)
    // Level 3: diamond_1_left, diamond_1_right
    // etc.
    // So total levels = 1 + 2*DIAMONDS

    let expected_levels = 1 + 2 * DIAMONDS;
    assert_eq!(
        groups.len(),
        expected_levels,
        "Should have {expected_levels} levels"
    );

    let sorted = graph
        .topological_sort()
        .expect("Topological sort should succeed");

    // Total tasks: first top + (left + right + bottom) * DIAMONDS
    // But bottom of each diamond (except last) is shared with next top
    // So: 1 + 3 * DIAMONDS - (DIAMONDS - 1) = 1 + 3*DIAMONDS - DIAMONDS + 1 = 2 + 2*DIAMONDS
    // Wait, let's count differently:
    // - Each diamond contributes: left, right, bottom (3 tasks)
    // - First diamond also contributes its top (1 task)
    // Total: 1 + 3 * DIAMONDS
    let expected_tasks = 1 + 3 * DIAMONDS;
    assert_eq!(
        sorted.len(),
        expected_tasks,
        "Should have {expected_tasks} tasks"
    );
}
