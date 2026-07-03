use super::*;

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
