use super::*;

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
