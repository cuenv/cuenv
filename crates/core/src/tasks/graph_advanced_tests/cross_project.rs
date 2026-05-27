use super::*;

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
