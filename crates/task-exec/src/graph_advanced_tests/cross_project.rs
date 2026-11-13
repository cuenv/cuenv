use crate::graph::TaskGraph;
use crate::test_utils;

#[test]
fn test_dag_with_multiple_project_references() {
    // Test DAG with multiple cross-project dependencies
    let mut graph = TaskGraph::new();

    // Project A tasks
    let task_a1 = test_utils::create_task("projectA.build", vec![], vec![]);
    let task_a2 = test_utils::create_task("projectA.test", vec!["projectA.build"], vec![]);

    // Project B tasks
    let task_b1 = test_utils::create_task("projectB.build", vec![], vec![]);
    let task_b2 = test_utils::create_task("projectB.test", vec!["projectB.build"], vec![]);

    // Cross-project integration task
    let integration = test_utils::create_task(
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

    let groups = graph.parallel_groups().unwrap();
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
