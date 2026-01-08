//! Property-based tests for task graph invariants.
//!
//! These tests verify the behavioral contracts of the task graph:
//! - Topological sort respects all dependencies
//! - Parallel groups contain only independent tasks
//! - Cycle detection is accurate

use cuenv_task_graph::{TaskGraph, TaskNodeData};
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

// =============================================================================
// Test Task Type
// =============================================================================

/// Simple task type for property testing.
#[derive(Clone, Debug)]
struct PropTask {
    deps: Vec<String>,
}

impl TaskNodeData for PropTask {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.deps.iter().map(String::as_str)
    }

    fn add_dependency(&mut self, dep: String) {
        if !self.deps.contains(&dep) {
            self.deps.push(dep);
        }
    }
}

impl PropTask {
    fn new(deps: Vec<String>) -> Self {
        Self { deps }
    }
}

// =============================================================================
// Strategies for generating test data
// =============================================================================

/// Generate a valid task name (lowercase alphanumeric with underscores).
fn task_name_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,10}".prop_map(String::from)
}

/// Generate a DAG (directed acyclic graph) with a specified number of tasks.
///
/// The strategy ensures no cycles by only allowing dependencies on tasks
/// with lower indices (tasks added earlier in the sequence).
fn dag_strategy(
    min_tasks: usize,
    max_tasks: usize,
) -> impl Strategy<Value = Vec<(String, Vec<String>)>> {
    (min_tasks..=max_tasks).prop_flat_map(|task_count| {
        // Generate unique task names
        proptest::collection::vec(task_name_strategy(), task_count).prop_flat_map(move |names| {
            // Deduplicate names by appending index
            let unique_names: Vec<String> = names
                .into_iter()
                .enumerate()
                .map(|(i, name)| format!("{name}_{i}"))
                .collect();

            // For each task, generate dependencies from earlier tasks only
            let dep_strategies: Vec<_> = (0..task_count)
                .map(|i| {
                    if i == 0 {
                        // First task has no deps
                        Just(vec![]).boxed()
                    } else {
                        // Can depend on any earlier task (0..i)
                        let earlier_names: Vec<String> = unique_names[..i].to_vec();
                        proptest::collection::vec(
                            proptest::sample::select(earlier_names),
                            0..=i.min(3), // Limit deps to avoid explosion
                        )
                        .prop_map(|deps| {
                            // Deduplicate deps
                            deps.into_iter()
                                .collect::<HashSet<_>>()
                                .into_iter()
                                .collect()
                        })
                        .boxed()
                    }
                })
                .collect();

            let names_clone = unique_names.clone();
            dep_strategies
                .into_iter()
                .collect::<Vec<_>>()
                .prop_map(move |all_deps| {
                    names_clone
                        .iter()
                        .cloned()
                        .zip(all_deps)
                        .collect::<Vec<_>>()
                })
        })
    })
}

/// Generate a graph that definitely contains a cycle.
fn cyclic_graph_strategy() -> impl Strategy<Value = Vec<(String, Vec<String>)>> {
    // Generate a small base graph then add a cycle
    (3..=6_usize).prop_flat_map(|task_count| {
        proptest::collection::vec(task_name_strategy(), task_count).prop_flat_map(move |names| {
            let unique_names: Vec<String> = names
                .into_iter()
                .enumerate()
                .map(|(i, name)| format!("{name}_{i}"))
                .collect();

            // Create a cycle: task_0 depends on task_last, and task_last depends on task_0
            // through intermediate tasks
            let task_count = unique_names.len();
            let names_clone = unique_names.clone();

            Just((0..task_count).collect::<Vec<_>>()).prop_map(move |indices| {
                let mut tasks: Vec<(String, Vec<String>)> = Vec::new();

                for i in indices {
                    let deps = if i == 0 {
                        // First task depends on last (creates cycle)
                        vec![names_clone[task_count - 1].clone()]
                    } else {
                        // Each task depends on the previous
                        vec![names_clone[i - 1].clone()]
                    };
                    tasks.push((names_clone[i].clone(), deps));
                }

                tasks
            })
        })
    })
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Build a TaskGraph from a list of (name, dependencies) pairs.
fn build_graph(
    tasks: &[(String, Vec<String>)],
) -> Result<TaskGraph<PropTask>, cuenv_task_graph::Error> {
    let mut graph = TaskGraph::new();

    for (name, deps) in tasks {
        let task = PropTask::new(deps.clone());
        graph.add_task(name, task)?;
    }

    graph.add_dependency_edges()?;
    Ok(graph)
}

// =============================================================================
// Property Tests: Topological Sort
// =============================================================================

proptest! {
    /// Contract: Topological sort respects all dependencies.
    ///
    /// For every task A that depends on task B, B must appear before A
    /// in the topologically sorted output.
    #[test]
    fn topological_sort_respects_dependencies(
        tasks in dag_strategy(1, 15)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");

        // Graph should be acyclic
        prop_assert!(!graph.has_cycles(), "Generated DAG should not have cycles");

        let sorted = graph.topological_sort().expect("Sort should succeed for DAG");

        // Build position map
        let positions: HashMap<String, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, node)| (node.name.clone(), i))
            .collect();

        // Verify: for each task, all its dependencies come before it
        for (name, deps) in &tasks {
            let task_pos = positions.get(name).expect("Task should be in sorted output");

            for dep in deps {
                let dep_pos = positions.get(dep).expect("Dependency should be in sorted output");
                prop_assert!(
                    dep_pos < task_pos,
                    "Dependency '{}' (pos {}) should come before '{}' (pos {})",
                    dep, dep_pos, name, task_pos
                );
            }
        }
    }

    /// Contract: Topological sort includes all tasks.
    #[test]
    fn topological_sort_includes_all_tasks(
        tasks in dag_strategy(1, 20)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");
        let sorted = graph.topological_sort().expect("Sort should succeed");

        prop_assert_eq!(
            sorted.len(),
            tasks.len(),
            "Sorted output should contain all {} tasks",
            tasks.len()
        );

        let sorted_names: HashSet<String> = sorted.iter().map(|n| n.name.clone()).collect();
        for (name, _) in &tasks {
            prop_assert!(
                sorted_names.contains(name),
                "Task '{}' should be in sorted output",
                name
            );
        }
    }
}

// =============================================================================
// Property Tests: Parallel Groups
// =============================================================================

proptest! {
    /// Contract: Parallel groups are disjoint (no task appears in multiple groups).
    #[test]
    fn parallel_groups_are_disjoint(
        tasks in dag_strategy(1, 15)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");
        let groups = graph.get_parallel_groups().expect("Parallel groups should succeed");

        let mut seen_tasks: HashSet<String> = HashSet::new();

        for (group_idx, group) in groups.iter().enumerate() {
            for node in group {
                prop_assert!(
                    seen_tasks.insert(node.name.clone()),
                    "Task '{}' appears in multiple groups (found in group {})",
                    node.name, group_idx
                );
            }
        }

        // All tasks should be present
        prop_assert_eq!(
            seen_tasks.len(),
            tasks.len(),
            "All tasks should appear exactly once across all groups"
        );
    }

    /// Contract: Tasks in the same parallel group have no dependencies on each other.
    #[test]
    fn parallel_groups_have_no_internal_dependencies(
        tasks in dag_strategy(2, 15)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");
        let groups = graph.get_parallel_groups().expect("Parallel groups should succeed");

        // Build dependency map for quick lookup
        let dep_map: HashMap<String, HashSet<String>> = tasks
            .iter()
            .map(|(name, deps)| (name.clone(), deps.iter().cloned().collect()))
            .collect();

        for (group_idx, group) in groups.iter().enumerate() {
            let group_names: HashSet<String> = group.iter().map(|n| n.name.clone()).collect();

            for node in group {
                if let Some(deps) = dep_map.get(&node.name) {
                    for dep in deps {
                        prop_assert!(
                            !group_names.contains(dep),
                            "Task '{}' in group {} depends on '{}' which is in the same group",
                            node.name, group_idx, dep
                        );
                    }
                }
            }
        }
    }

    /// Contract: Parallel group ordering respects dependencies.
    ///
    /// If task A depends on task B, then B's group index must be less than A's group index.
    #[test]
    fn parallel_groups_respect_dependency_order(
        tasks in dag_strategy(2, 15)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");
        let groups = graph.get_parallel_groups().expect("Parallel groups should succeed");

        // Build task-to-group-index map
        let mut task_group: HashMap<String, usize> = HashMap::new();
        for (group_idx, group) in groups.iter().enumerate() {
            for node in group {
                task_group.insert(node.name.clone(), group_idx);
            }
        }

        // Verify dependencies are in earlier groups
        for (name, deps) in &tasks {
            let task_group_idx = task_group.get(name).expect("Task should have group");

            for dep in deps {
                let dep_group_idx = task_group.get(dep).expect("Dependency should have group");
                prop_assert!(
                    dep_group_idx < task_group_idx,
                    "Dependency '{}' (group {}) should be in earlier group than '{}' (group {})",
                    dep, dep_group_idx, name, task_group_idx
                );
            }
        }
    }
}

// =============================================================================
// Property Tests: Cycle Detection
// =============================================================================

proptest! {
    /// Contract: Acyclic graphs are correctly identified as having no cycles.
    #[test]
    fn cycle_detection_identifies_dags(
        tasks in dag_strategy(1, 15)
    ) {
        let graph = build_graph(&tasks).expect("Graph should build successfully");

        prop_assert!(
            !graph.has_cycles(),
            "DAG should be identified as acyclic"
        );

        // Topological sort should succeed
        let result = graph.topological_sort();
        prop_assert!(
            result.is_ok(),
            "Topological sort should succeed for DAG"
        );
    }

    /// Contract: Cyclic graphs are correctly identified as having cycles.
    #[test]
    fn cycle_detection_identifies_cycles(
        tasks in cyclic_graph_strategy()
    ) {
        let graph_result = build_graph(&tasks);

        // The graph might fail to build if deps reference non-existent tasks,
        // but if it builds, it should have cycles
        if let Ok(graph) = graph_result {
            prop_assert!(
                graph.has_cycles(),
                "Cyclic graph should be identified as having cycles"
            );

            // Topological sort should fail
            let result = graph.topological_sort();
            prop_assert!(
                result.is_err(),
                "Topological sort should fail for cyclic graph"
            );
        }
    }
}

// =============================================================================
// Additional Property Tests
// =============================================================================

proptest! {
    /// Contract: Empty graph operations succeed.
    #[test]
    fn empty_graph_operations_succeed(_seed in 0..100_u32) {
        let graph: TaskGraph<PropTask> = TaskGraph::new();

        prop_assert!(!graph.has_cycles(), "Empty graph has no cycles");

        let sorted = graph.topological_sort().expect("Sort should succeed");
        prop_assert!(sorted.is_empty(), "Empty graph produces empty sort");

        let groups = graph.get_parallel_groups().expect("Parallel groups should succeed");
        prop_assert!(groups.is_empty(), "Empty graph produces no groups");
    }

    /// Contract: Single task graph works correctly.
    #[test]
    fn single_task_graph_works(name in task_name_strategy()) {
        let mut graph = TaskGraph::new();
        graph.add_task(&name, PropTask::new(vec![])).expect("Add should succeed");
        graph.add_dependency_edges().expect("Edges should succeed");

        prop_assert!(!graph.has_cycles());
        prop_assert_eq!(graph.task_count(), 1);

        let sorted = graph.topological_sort().expect("Sort should succeed");
        prop_assert_eq!(sorted.len(), 1);
        prop_assert_eq!(&sorted[0].name, &name);

        let groups = graph.get_parallel_groups().expect("Groups should succeed");
        prop_assert_eq!(groups.len(), 1);
        prop_assert_eq!(groups[0].len(), 1);
    }

    /// Contract: Duplicate task names return the same node.
    #[test]
    fn duplicate_task_names_handled(name in task_name_strategy()) {
        let mut graph = TaskGraph::new();

        let node1 = graph.add_task(&name, PropTask::new(vec![])).expect("First add");
        let node2 = graph.add_task(&name, PropTask::new(vec![])).expect("Second add");

        prop_assert_eq!(node1, node2, "Same name should return same node index");
        prop_assert_eq!(graph.task_count(), 1, "Should only have one task");
    }

    /// Contract: Graph task count matches input.
    #[test]
    fn task_count_matches_input(tasks in dag_strategy(1, 20)) {
        let graph = build_graph(&tasks).expect("Graph should build");
        prop_assert_eq!(
            graph.task_count(),
            tasks.len(),
            "Task count should match number of unique tasks added"
        );
    }
}

// =============================================================================
// Determinism Tests
// =============================================================================

proptest! {
    /// Contract: Topological sort is deterministic for the same graph.
    #[test]
    fn topological_sort_is_deterministic(tasks in dag_strategy(2, 10)) {
        let graph1 = build_graph(&tasks).expect("Graph 1 should build");
        let graph2 = build_graph(&tasks).expect("Graph 2 should build");

        let sorted1 = graph1.topological_sort().expect("Sort 1 should succeed");
        let sorted2 = graph2.topological_sort().expect("Sort 2 should succeed");

        let names1: Vec<String> = sorted1.iter().map(|n| n.name.clone()).collect();
        let names2: Vec<String> = sorted2.iter().map(|n| n.name.clone()).collect();

        prop_assert_eq!(names1, names2, "Topological sort should be deterministic");
    }

    /// Contract: Parallel groups are deterministic for the same graph.
    #[test]
    fn parallel_groups_are_deterministic(tasks in dag_strategy(2, 10)) {
        let graph1 = build_graph(&tasks).expect("Graph 1 should build");
        let graph2 = build_graph(&tasks).expect("Graph 2 should build");

        let groups1 = graph1.get_parallel_groups().expect("Groups 1 should succeed");
        let groups2 = graph2.get_parallel_groups().expect("Groups 2 should succeed");

        prop_assert_eq!(groups1.len(), groups2.len(), "Group count should match");

        for (g1, g2) in groups1.iter().zip(groups2.iter()) {
            let names1: HashSet<String> = g1.iter().map(|n| n.name.clone()).collect();
            let names2: HashSet<String> = g2.iter().map(|n| n.name.clone()).collect();
            prop_assert_eq!(names1, names2, "Groups should contain same tasks");
        }
    }
}
