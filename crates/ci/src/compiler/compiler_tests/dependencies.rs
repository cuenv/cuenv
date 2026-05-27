use super::*;

#[test]
fn test_expand_dependency_to_task_group() {
    // Test that dependencies on task groups are expanded to their leaf tasks
    let mut project = Project::new("test-project");

    // Create a task group with children
    let mut test_children = HashMap::new();
    test_children.insert(
        "unit".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string(), "--lib".to_string()],
            ..Default::default()
        })),
    );
    test_children.insert(
        "doc".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string(), "--doc".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "tests".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: test_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    // Create a task that depends on the group
    project.tasks.insert(
        "check".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["done".to_string()],
            depends_on: vec![TaskDependency::from_name("tests")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    // Find the check task
    let check_task = ir.tasks.iter().find(|t| t.id == "check").unwrap();

    // Dependencies should be expanded to the leaf tasks (sorted alphabetically)
    assert_eq!(
        check_task.depends_on,
        vec!["tests.doc", "tests.unit"],
        "Group dependency should expand to leaf tasks"
    );
}

#[test]
fn test_expand_dependency_leaf_task_unchanged() {
    // Test that dependencies on leaf tasks remain unchanged
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
    assert_eq!(
        test_task.depends_on,
        vec!["build"],
        "Leaf task dependency should remain unchanged"
    );
}

#[test]
fn test_expand_dependency_nested_groups() {
    // Test that nested groups are recursively expanded
    let mut project = Project::new("test-project");

    // Create inner group
    let mut inner_children = HashMap::new();
    inner_children.insert(
        "a".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["a".to_string()],
            ..Default::default()
        })),
    );
    inner_children.insert(
        "b".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["b".to_string()],
            ..Default::default()
        })),
    );

    // Create outer group containing inner group
    let mut outer_children = HashMap::new();
    outer_children.insert(
        "inner".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: inner_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );
    outer_children.insert(
        "leaf".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["leaf".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "outer".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: outer_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    project.tasks.insert(
        "final".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["final".to_string()],
            depends_on: vec![TaskDependency::from_name("outer")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    let final_task = ir.tasks.iter().find(|t| t.id == "final").unwrap();
    assert_eq!(
        final_task.depends_on,
        vec!["outer.inner.a", "outer.inner.b", "outer.leaf"],
        "Nested group should be recursively expanded"
    );
}

#[test]
fn test_expand_dependency_sibling_resolution() {
    // Test that sibling task references are resolved correctly
    // This tests the case where docs.deploy depends on "build" (a sibling)
    let mut project = Project::new("test-project");

    // Create a group with two tasks: build and deploy
    // deploy depends on "build" (sibling reference, not "docs.build")
    let mut docs_children = HashMap::new();
    docs_children.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "build".to_string()],
            ..Default::default()
        })),
    );
    docs_children.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "deploy".to_string()],
            // This simulates `dependsOn: [build]` which gets extracted as just "build"
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "docs".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: docs_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    // Find the docs.deploy task
    let deploy_task = ir.tasks.iter().find(|t| t.id == "docs.deploy").unwrap();

    // The "build" dependency should be resolved to "docs.build" (sibling)
    assert_eq!(
        deploy_task.depends_on,
        vec!["docs.build"],
        "Sibling reference 'build' should resolve to 'docs.build'"
    );
}
