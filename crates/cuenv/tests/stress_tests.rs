//! Stress tests for task graph operations.
//!
//! These tests verify that the task graph handles large-scale scenarios
//! correctly. They are marked as #[ignore] since they may take longer
//! to run and are typically only needed for validation before releases.

use cuenv_task_graph::{MutableTaskNodeData, TaskGraph, TaskNodeData};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Simple task type for stress testing.
#[derive(Clone, Debug)]
struct StressTask {
    deps: Vec<String>,
}

impl TaskNodeData for StressTask {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.deps.iter().map(String::as_str)
    }
}

impl MutableTaskNodeData for StressTask {
    fn add_dependency(&mut self, dep: String) {
        if !self.deps.contains(&dep) {
            self.deps.push(dep);
        }
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

fn task_position(positions: &HashMap<String, usize>, task_name: &str) -> TestResult<usize> {
    positions
        .get(task_name)
        .copied()
        .ok_or_else(|| io::Error::other(format!("missing task {task_name}")).into())
}

/// Test that 100 tasks can run in parallel when they have no dependencies.
///
/// This verifies that the parallel group computation correctly identifies
/// independent tasks and groups them together.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_100_parallel_tasks() -> TestResult {
    let mut graph = TaskGraph::new();

    for i in 0..100 {
        let task = StressTask::new(&[]);
        graph.add_task(&format!("parallel_task_{i}"), task)?;
    }

    graph.add_dependency_edges()?;

    assert!(!graph.has_cycles(), "Graph should not have cycles");

    let groups = graph.get_parallel_groups()?;

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

    let sorted = graph.topological_sort()?;
    assert_eq!(sorted.len(), 100, "Should have 100 tasks in sorted order");
    Ok(())
}

/// Test a deep dependency chain of 20 levels.
///
/// This verifies that the graph correctly handles deeply nested dependencies
/// where each task depends on the previous one in a chain.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_deep_dependency_chain_20_levels() -> TestResult {
    const DEPTH: usize = 20;
    let mut graph = TaskGraph::new();

    graph.add_task("chain_task_0", StressTask::new(&[]))?;

    for i in 1..DEPTH {
        let dep = format!("chain_task_{}", i - 1);
        let task = StressTask::with_deps(vec![dep]);
        graph.add_task(&format!("chain_task_{i}"), task)?;
    }

    graph.add_dependency_edges()?;

    assert!(!graph.has_cycles(), "Graph should not have cycles");

    let groups = graph.get_parallel_groups()?;

    assert_eq!(
        groups.len(),
        DEPTH,
        "Should have {DEPTH} levels (one task per level)"
    );

    // Each group should have exactly 1 task
    for (level, group) in groups.iter().enumerate() {
        assert_eq!(group.len(), 1, "Level {level} should have exactly 1 task");
    }

    let sorted = graph.topological_sort()?;

    assert_eq!(sorted.len(), DEPTH, "Should have {DEPTH} tasks");

    // Verify correct ordering: each task should appear before its dependents
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();

    for i in 1..DEPTH {
        let current = format!("chain_task_{i}");
        let previous = format!("chain_task_{}", i - 1);
        let previous_position = task_position(&positions, &previous)?;
        let current_position = task_position(&positions, &current)?;
        assert!(
            previous_position < current_position,
            "Task {} should come before {}",
            previous,
            current
        );
    }
    Ok(())
}

/// Test a large task graph with 1000 tasks.
///
/// This creates a graph with multiple parallel branches that fan out
/// and fan back in, creating a complex dependency structure.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_large_task_graph_1000_tasks() -> TestResult {
    const TOTAL_TASKS: usize = 1000;
    const FAN_WIDTH: usize = 10;
    let mut graph = TaskGraph::new();

    // Structure:
    // - Level 0: 1 root task
    // - Level 1-99: 10 tasks each, all depending on level 0
    // - Final task depends on all tasks from the last level
    //
    // This creates a fan-out/fan-in pattern

    graph.add_task("root", StressTask::new(&[]))?;

    let mut last_level_tasks: Vec<String> = vec!["root".to_string()];
    let mut task_count = 1;

    // Add intermediate levels (fan-out pattern)
    let num_levels = (TOTAL_TASKS - 2) / FAN_WIDTH; // Reserve 1 for root, 1 for final

    for level in 0..num_levels {
        let mut current_level_tasks = Vec::new();

        for i in 0..FAN_WIDTH {
            let task_name = format!("level_{level}_task_{i}");
            let task = StressTask::with_deps(last_level_tasks.clone());
            graph.add_task(&task_name, task)?;
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
    graph.add_task("final", final_task)?;

    graph.add_dependency_edges()?;

    assert!(!graph.has_cycles(), "Graph should not have cycles");

    let actual_task_count = graph.task_count();
    assert!(
        actual_task_count >= 100,
        "Should have at least 100 tasks, got {actual_task_count}"
    );

    let groups = graph.get_parallel_groups()?;

    assert!(
        !groups.is_empty(),
        "Should have at least one parallel group"
    );

    // First group should contain only the root
    assert_eq!(groups[0].len(), 1, "First group should have only root");
    assert_eq!(groups[0][0].name, "root", "First task should be root");

    // Last group should contain only the final task
    let last_group = groups
        .last()
        .ok_or_else(|| io::Error::other("expected at least one parallel group"))?;
    assert_eq!(
        last_group.len(),
        1,
        "Last group should have only final task"
    );
    assert_eq!(last_group[0].name, "final", "Last task should be final");

    let sorted = graph.topological_sort()?;

    assert_eq!(
        sorted.len(),
        actual_task_count,
        "Sorted list should contain all tasks"
    );

    // Verify root comes first and final comes last
    assert_eq!(sorted.first().map(|n| n.name.as_str()), Some("root"));
    assert_eq!(sorted.last().map(|n| n.name.as_str()), Some("final"));
    Ok(())
}

/// Test that a wide graph (many tasks depending on one root) is handled correctly.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_wide_graph_500_leaves() -> TestResult {
    const LEAF_COUNT: usize = 500;
    let mut graph = TaskGraph::new();

    graph.add_task("root", StressTask::new(&[]))?;

    for i in 0..LEAF_COUNT {
        let task = StressTask::new(&["root"]);
        graph.add_task(&format!("leaf_{i}"), task)?;
    }

    graph.add_dependency_edges()?;

    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups()?;

    // Should have 2 levels: root, then all leaves
    assert_eq!(groups.len(), 2, "Should have 2 levels");
    assert_eq!(groups[0].len(), 1, "First level should have 1 task (root)");
    assert_eq!(
        groups[1].len(),
        LEAF_COUNT,
        "Second level should have all {LEAF_COUNT} leaves"
    );
    Ok(())
}

/// Test diamond dependency pattern at scale.
///
/// Creates multiple diamond patterns connected in a chain.
#[test]
#[ignore = "stress test - run explicitly with --ignored"]
fn test_diamond_pattern_chain() -> TestResult {
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
            graph.add_task(&name, StressTask::new(&[]))?;
            name
        };

        let left_name = format!("diamond_{d}_left");
        let right_name = format!("diamond_{d}_right");
        let bottom_name = format!("diamond_{d}_bottom");

        graph.add_task(&left_name, StressTask::new(&[&top_name]))?;
        graph.add_task(&right_name, StressTask::new(&[&top_name]))?;
        graph.add_task(&bottom_name, StressTask::new(&[&left_name, &right_name]))?;

        prev_bottom = Some(bottom_name);
    }

    graph.add_dependency_edges()?;

    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups()?;

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

    let sorted = graph.topological_sort()?;

    // Each diamond contributes left, right, and bottom; the first diamond also
    // contributes the initial top node.
    let expected_tasks = 1 + 3 * DIAMONDS;
    assert_eq!(
        sorted.len(),
        expected_tasks,
        "Should have {expected_tasks} tasks"
    );
    Ok(())
}
