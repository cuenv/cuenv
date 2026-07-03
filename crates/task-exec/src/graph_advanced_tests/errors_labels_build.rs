use super::*;

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
    let user_task = create_task("dev", vec!["bun.setup"], vec!["bun"]);

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
        TaskNode::Task(Box::new(create_task("external::build", vec![], vec![]))),
    );

    // Local task depending on external
    tasks.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(create_task(
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
        TaskNode::Task(Box::new(create_task("hook", vec![], vec![]))),
    );

    tasks.tasks.insert(
        "bun.install".to_string(),
        TaskNode::Task(Box::new(create_task(
            "install",
            vec!["bun.hooks.beforeInstall[0]"],
            vec![],
        ))),
    );

    tasks.tasks.insert(
        "bun.setup".to_string(),
        TaskNode::Task(Box::new(create_task("setup", vec!["bun.install"], vec![]))),
    );

    tasks.tasks.insert(
        "dev".to_string(),
        TaskNode::Task(Box::new(create_task("dev", vec!["bun.setup"], vec!["bun"]))),
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
