use super::*;

#[test]
fn test_workspace_setup_chain() {
    // Test the full workspace setup chain: hooks -> install -> setup -> user_task
    let mut graph = TaskGraph::new();

    let hook1 = create_task("bun.hooks.beforeInstall[0]", vec![], vec![]);
    let install = create_task("bun.install", vec!["bun.hooks.beforeInstall[0]"], vec![]);
    let setup = create_task("bun.setup", vec!["bun.install"], vec![]);
    let user_task = create_task("dev", vec!["bun.setup"], vec!["bun"]);

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
    let build = create_task(
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
