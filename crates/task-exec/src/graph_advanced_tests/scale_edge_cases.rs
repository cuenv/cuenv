use super::*;

#[test]
fn test_large_dag_with_many_hooks() {
    // Test with many hook tasks (performance/scalability)
    let mut graph = TaskGraph::new();

    let num_hooks = 20;
    let mut prev_hook = String::new();

    for i in 0..num_hooks {
        let hook_name = format!("bun.hooks.beforeInstall[{}]", i);
        let deps = if prev_hook.is_empty() {
            vec![]
        } else {
            vec![prev_hook.as_str()]
        };
        let hook = create_task(&hook_name, deps, vec![]);
        graph.add_task(&hook_name, hook).unwrap();
        prev_hook = hook_name;
    }

    // Install depends on last hook
    let install = create_task("bun.install", vec![&prev_hook], vec![]);
    graph.add_task("bun.install", install).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), num_hooks + 1);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // All sequential (chain of 20 hooks + install)
    assert_eq!(groups.len(), num_hooks + 1);
}

#[test]
fn test_dag_with_many_parallel_projects() {
    // Test with many projects that can run in parallel
    let mut graph = TaskGraph::new();

    let num_projects = 15;

    for i in 0..num_projects {
        let project_name = format!("project{}::build", i);
        let task = create_task(&project_name, vec![], vec![]);
        graph.add_task(&project_name, task).unwrap();
    }

    // Integration task depends on all projects
    let integration_deps: Vec<TaskDependency> = (0..num_projects)
        .map(|i| TaskDependency::from_name(format!("project{}::build", i)))
        .collect();

    let integration = Task {
        command: "echo integration".to_string(),
        depends_on: integration_deps,
        ..Default::default()
    };
    graph.add_task("integration", integration).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), num_projects + 1);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), num_projects); // All projects in parallel
    assert_eq!(groups[1].len(), 1); // integration
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_task_with_very_long_name() {
    // Test handling of very long task names
    let mut graph = TaskGraph::new();

    let long_name = "a".repeat(500);
    let task = create_task(&long_name, vec![], vec![]);
    graph.add_task(&long_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(&long_name));
}

#[test]
fn test_task_with_unicode_name() {
    // Test handling of Unicode characters in task names
    let mut graph = TaskGraph::new();

    let unicode_name = "任务_测试_日本語_émoji🚀";
    let task = create_task(unicode_name, vec![], vec![]);
    graph.add_task(unicode_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(unicode_name));
}

#[test]
fn test_cross_project_with_unicode_names() {
    // Test cross-project dependencies with Unicode project names
    let mut graph = TaskGraph::new();

    let proj_a = "项目A::构建";
    let proj_b = "プロジェクトB::ビルド";

    let task_a = create_task(proj_a, vec![], vec![]);
    let task_b = create_task(proj_b, vec![proj_a], vec![]);

    graph.add_task(proj_a, task_a).unwrap();
    graph.add_task(proj_b, task_b).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted[0].name, proj_a);
    assert_eq!(sorted[1].name, proj_b);
}

#[test]
fn test_task_with_special_characters() {
    // Test handling of special characters in task names
    let mut graph = TaskGraph::new();

    let special_name = "task-with.dots_and-dashes";
    let task = create_task(special_name, vec![], vec![]);
    graph.add_task(special_name, task).unwrap();

    assert_eq!(graph.task_count(), 1);
    assert!(graph.contains_task(special_name));
}

#[test]
fn test_synthetic_hook_with_special_index() {
    // Test synthetic hook naming with various index formats
    let mut graph = TaskGraph::new();

    // Test multiple hook naming patterns
    let patterns = vec![
        "ws.hooks.beforeInstall[0]",
        "ws.hooks.beforeInstall[99]",
        "ws.hooks.beforeInstall.matcher[0]",
        "ws.hooks.afterInstall[0]",
    ];

    for pattern in &patterns {
        let task = create_task(pattern, vec![], vec![]);
        graph.add_task(pattern, task).unwrap();
    }

    assert_eq!(graph.task_count(), 4);
    for pattern in &patterns {
        assert!(graph.contains_task(pattern));
    }
}

#[test]
fn test_empty_depends_on_list() {
    // Test that tasks with empty dependency lists are handled correctly
    let mut graph = TaskGraph::new();

    let task1 = Task {
        command: "echo task1".to_string(),
        depends_on: vec![], // Empty
        ..Default::default()
    };

    let task2 = Task {
        command: "echo task2".to_string(),
        depends_on: vec![], // Empty
        ..Default::default()
    };

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    // Both should be in the same parallel group (no dependencies between them)
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].len(), 2);
}

#[test]
fn test_project_separator_variations() {
    // Test various project separator formats
    let mut graph = TaskGraph::new();

    // Different project::task formats
    let formats = [
        "project::task",
        "project::nested.task",
        "project::deeply.nested.task",
        "my-project::my-task",
        "my_project::my_task",
    ];

    for (i, name) in formats.iter().enumerate() {
        let deps: Vec<&str> = if i > 0 { vec![formats[i - 1]] } else { vec![] };
        let task = create_task(name, deps, vec![]);
        graph.add_task(name, task).unwrap();
    }

    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), formats.len());
    assert!(!graph.has_cycles());

    // Should be sequential
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), formats.len());
}

#[test]
fn test_multiple_tasks_same_project() {
    // Test multiple tasks from the same project
    let mut graph = TaskGraph::new();

    let tasks = vec![
        ("myproject::build", vec![]),
        ("myproject::test", vec!["myproject::build"]),
        ("myproject::lint", vec!["myproject::build"]),
        (
            "myproject::deploy",
            vec!["myproject::test", "myproject::lint"],
        ),
    ];

    for (name, deps) in &tasks {
        let task = create_task(name, deps.to_vec(), vec![]);
        graph.add_task(name, task).unwrap();
    }

    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 4);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    // build -> (test, lint) in parallel -> deploy
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 1); // build
    assert_eq!(groups[1].len(), 2); // test, lint
    assert_eq!(groups[2].len(), 1); // deploy
}

#[test]
fn test_deeply_nested_task_groups() {
    // Test deeply nested parallel and sequential groups
    let mut graph = TaskGraph::new();

    // Create a chain: a -> (b1, b2) -> c -> (d1, d2, d3) -> e
    let task_a = create_task("a", vec![], vec![]);
    let task_b1 = create_task("b1", vec!["a"], vec![]);
    let task_b2 = create_task("b2", vec!["a"], vec![]);
    let task_c = create_task("c", vec!["b1", "b2"], vec![]);
    let task_d1 = create_task("d1", vec!["c"], vec![]);
    let task_d2 = create_task("d2", vec!["c"], vec![]);
    let task_d3 = create_task("d3", vec!["c"], vec![]);
    let task_e = create_task("e", vec!["d1", "d2", "d3"], vec![]);

    graph.add_task("a", task_a).unwrap();
    graph.add_task("b1", task_b1).unwrap();
    graph.add_task("b2", task_b2).unwrap();
    graph.add_task("c", task_c).unwrap();
    graph.add_task("d1", task_d1).unwrap();
    graph.add_task("d2", task_d2).unwrap();
    graph.add_task("d3", task_d3).unwrap();
    graph.add_task("e", task_e).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 8);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 5);
    assert_eq!(groups[0].len(), 1); // a
    assert_eq!(groups[1].len(), 2); // b1, b2
    assert_eq!(groups[2].len(), 1); // c
    assert_eq!(groups[3].len(), 3); // d1, d2, d3
    assert_eq!(groups[4].len(), 1); // e
}
