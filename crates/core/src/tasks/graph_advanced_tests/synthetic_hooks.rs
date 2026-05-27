use super::*;

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
