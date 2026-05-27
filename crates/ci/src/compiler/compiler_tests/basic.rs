use super::*;

#[test]
fn test_compile_simple_task() {
    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![cuenv_core::tasks::Input::Path("src/**/*.rs".to_string())],
            outputs: vec!["target/debug/binary".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.version, "1.5");
    assert_eq!(ir.pipeline.name, "test-project");
    assert_eq!(ir.tasks.len(), 1);
    assert_eq!(ir.tasks[0].id, "build");
    assert_eq!(ir.tasks[0].command, vec!["cargo", "build"]);
    assert_eq!(ir.tasks[0].inputs, vec!["src/**/*.rs"]);
}

#[test]
fn test_compile_task_with_dependencies() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 2);

    let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
    assert_eq!(test_task.depends_on, vec!["build"]);
}

#[test]
fn test_compile_deployment_task() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            labels: vec!["deployment".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 1);
    assert!(ir.tasks[0].deployment);
    assert_eq!(ir.tasks[0].cache_policy, CachePolicy::Disabled);
}

#[test]
fn test_compile_script_task() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "script-task".to_string(),
        TaskNode::Task(Box::new(Task {
            script: Some("echo 'Running script'\nls -la".to_string()),
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 1);
    assert!(ir.tasks[0].shell);
    assert_eq!(ir.tasks[0].command[0], "/bin/sh");
    assert_eq!(ir.tasks[0].command[1], "-c");
}
