use super::*;
use crate::tasks::{TaskDependency, TaskGroup, TaskNode};
use crate::test_utils::create_test_hook;

#[test]
fn test_vcs_dependency_deserializes_subdir() {
    let dep: VcsDependency = serde_json::from_value(serde_json::json!({
        "url": "https://github.com/cuenv/cuenv.git",
        "reference": "0.27.1",
        "vendor": true,
        "path": ".agents/skills",
        "subdir": ".agents/skills"
    }))
    .expect("subdir field should deserialize");
    assert_eq!(dep.subdir.as_deref(), Some(".agents/skills"));
}

#[test]
fn test_vcs_dependency_subdir_defaults_to_none() {
    let dep: VcsDependency = serde_json::from_value(serde_json::json!({
        "url": "https://example.com/lib.git",
        "reference": "main",
        "vendor": true,
        "path": "vendor/lib"
    }))
    .expect("legacy spec without subdir should deserialize");
    assert_eq!(dep.subdir, None);
}

#[test]
fn test_service_type_defaults_to_service_when_omitted() {
    let service: Service = serde_json::from_value(serde_json::json!({
        "entrypoint": { "command": "echo", "args": ["hello"] }
    }))
    .expect("service should deserialize without explicit type");

    assert_eq!(service.service_type, "service");
}

#[test]
fn test_service_entrypoint_command_variant() {
    let service: Service = serde_json::from_value(serde_json::json!({
        "entrypoint": { "command": "echo", "args": ["hi"] }
    }))
    .expect("should deserialize command entrypoint");

    // Task is tried first and matches (Task accepts any {command,args})
    // so the command variant here is shaped like a Task with just command+args.
    match &service.entrypoint {
        Entrypoint::Task(task) => {
            assert_eq!(task.command, "echo");
        }
        Entrypoint::Command(cmd) => assert_eq!(cmd.command, "echo"),
        Entrypoint::Script(_) => panic!("expected Task or Command, got Script"),
    }
}

#[test]
fn test_service_entrypoint_script_variant() {
    let service: Service = serde_json::from_value(serde_json::json!({
        "entrypoint": { "script": "echo hi" }
    }))
    .expect("should deserialize script entrypoint");

    match &service.entrypoint {
        Entrypoint::Task(task) => {
            assert_eq!(task.script.as_deref(), Some("echo hi"));
        }
        Entrypoint::Script(s) => assert_eq!(s.script, "echo hi"),
        Entrypoint::Command(_) => panic!("expected Task or Script, got Command"),
    }
}

#[test]
fn test_expand_cross_project_references() {
    let task = Task {
        inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
        ..Default::default()
    };

    let mut cuenv = Project::new("test");
    cuenv
        .tasks
        .insert("deploy".into(), TaskNode::Task(Box::new(task)));

    cuenv.expand_cross_project_references();

    let task_def = cuenv.tasks.get("deploy").unwrap();
    let task = task_def.as_task().unwrap();

    // Check inputs expansion
    assert_eq!(task.inputs.len(), 1);
    match &task.inputs[0] {
        Input::Project(proj_ref) => {
            assert_eq!(proj_ref.project, "myproj");
            assert_eq!(proj_ref.task, "build");
            assert_eq!(proj_ref.map.len(), 1);
            assert_eq!(proj_ref.map[0].from, "dist/app.js");
            assert_eq!(proj_ref.map[0].to, "dist/app.js");
        }
        _ => panic!("Expected ProjectReference"),
    }

    // Check implicit dependency
    assert_eq!(task.depends_on.len(), 1);
    assert_eq!(task.depends_on[0].task_name(), "#myproj:build");
}

// ============================================================================
// HookItem and TaskRef Tests
// ============================================================================

#[test]
fn test_task_ref_parse_valid() {
    let task_ref = TaskRef {
        ref_: "#projen-generator:types".to_string(),
    };

    let parsed = task_ref.parse();
    assert!(parsed.is_some());

    let (project, task) = parsed.unwrap();
    assert_eq!(project, "projen-generator");
    assert_eq!(task, "types");
}

#[test]
fn test_task_ref_parse_with_dots() {
    let task_ref = TaskRef {
        ref_: "#my-project:bun.install".to_string(),
    };

    let parsed = task_ref.parse();
    assert!(parsed.is_some());

    let (project, task) = parsed.unwrap();
    assert_eq!(project, "my-project");
    assert_eq!(task, "bun.install");
}

#[test]
fn test_task_ref_parse_no_hash() {
    let task_ref = TaskRef {
        ref_: "project:task".to_string(),
    };

    // Without leading #, parse should fail
    let parsed = task_ref.parse();
    assert!(parsed.is_none());
}

#[test]
fn test_task_ref_parse_no_colon() {
    let task_ref = TaskRef {
        ref_: "#project-only".to_string(),
    };

    // Without colon separator, parse should fail
    let parsed = task_ref.parse();
    assert!(parsed.is_none());
}

#[test]
fn test_task_ref_parse_empty_project() {
    let task_ref = TaskRef {
        ref_: "#:task".to_string(),
    };

    // Empty project name should be rejected
    assert!(task_ref.parse().is_none());
}

#[test]
fn test_task_ref_parse_empty_task() {
    let task_ref = TaskRef {
        ref_: "#project:".to_string(),
    };

    // Empty task name should be rejected
    assert!(task_ref.parse().is_none());
}

#[test]
fn test_task_ref_parse_both_empty() {
    let task_ref = TaskRef {
        ref_: "#:".to_string(),
    };

    // Both empty should be rejected
    assert!(task_ref.parse().is_none());
}

#[test]
fn test_task_ref_parse_multiple_colons() {
    let task_ref = TaskRef {
        ref_: "#project:task:extra".to_string(),
    };

    // Multiple colons - first split wins
    let parsed = task_ref.parse();
    assert!(parsed.is_some());
    let (project, task) = parsed.unwrap();
    assert_eq!(project, "project");
    assert_eq!(task, "task:extra");
}

#[test]
fn test_task_ref_parse_unicode() {
    let task_ref = TaskRef {
        ref_: "#项目名:任务名".to_string(),
    };

    let parsed = task_ref.parse();
    assert!(parsed.is_some());
    let (project, task) = parsed.unwrap();
    assert_eq!(project, "项目名");
    assert_eq!(task, "任务名");
}

#[test]
fn test_task_ref_parse_special_characters() {
    let task_ref = TaskRef {
        ref_: "#my-project_v2:build.ci-test".to_string(),
    };

    let parsed = task_ref.parse();
    assert!(parsed.is_some());
    let (project, task) = parsed.unwrap();
    assert_eq!(project, "my-project_v2");
    assert_eq!(task, "build.ci-test");
}

#[test]
fn test_hook_item_task_ref_deserialization() {
    let json = "{\"ref\": \"#other-project:build\"}";
    let hook_item: HookItem = serde_json::from_str(json).unwrap();

    match hook_item {
        HookItem::TaskRef(task_ref) => {
            assert_eq!(task_ref.ref_, "#other-project:build");
            let (project, task) = task_ref.parse().unwrap();
            assert_eq!(project, "other-project");
            assert_eq!(task, "build");
        }
        _ => panic!("Expected HookItem::TaskRef"),
    }
}

#[test]
fn test_hook_item_match_deserialization() {
    let json = r#"{
        "name": "projen",
        "match": {
            "labels": ["codegen", "projen"]
        }
    }"#;
    let hook_item: HookItem = serde_json::from_str(json).unwrap();

    match hook_item {
        HookItem::Match(match_hook) => {
            assert_eq!(match_hook.name, Some("projen".to_string()));
            assert_eq!(
                match_hook.matcher.labels,
                Some(vec!["codegen".to_string(), "projen".to_string()])
            );
        }
        _ => panic!("Expected HookItem::Match"),
    }
}

#[test]
fn test_hook_item_match_with_parallel_false() {
    let json = r#"{
        "match": {
            "labels": ["build"],
            "parallel": false
        }
    }"#;
    let hook_item: HookItem = serde_json::from_str(json).unwrap();

    match hook_item {
        HookItem::Match(match_hook) => {
            assert!(match_hook.name.is_none());
            assert!(!match_hook.matcher.parallel);
        }
        _ => panic!("Expected HookItem::Match"),
    }
}

#[test]
fn test_hook_item_inline_task_deserialization() {
    let json = r#"{
        "command": "echo",
        "args": ["hello"]
    }"#;
    let hook_item: HookItem = serde_json::from_str(json).unwrap();

    match hook_item {
        HookItem::Task(task) => {
            assert_eq!(task.command, "echo");
            assert_eq!(task.args, vec!["hello"]);
        }
        _ => panic!("Expected HookItem::Task"),
    }
}

#[test]
fn test_task_matcher_deserialization() {
    let json = r#"{
        "labels": ["projen", "codegen"],
        "parallel": true
    }"#;
    let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

    assert_eq!(
        matcher.labels,
        Some(vec!["projen".to_string(), "codegen".to_string()])
    );
    assert!(matcher.parallel);
}

#[test]
fn test_task_matcher_defaults() {
    let json = r#"{}"#;
    let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

    assert!(matcher.labels.is_none());
    assert!(matcher.command.is_none());
    assert!(matcher.args.is_none());
    assert!(matcher.parallel); // default true
}

#[test]
fn test_task_matcher_with_command() {
    let json = r#"{
        "command": "prisma",
        "args": [{"contains": "generate"}]
    }"#;
    let matcher: TaskMatcher = serde_json::from_str(json).unwrap();

    assert_eq!(matcher.command, Some("prisma".to_string()));
    let args = matcher.args.unwrap();
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].contains, Some("generate".to_string()));
}

// ============================================================================
// Cross-Project Reference Expansion Tests
// ============================================================================

#[test]
fn test_expand_multiple_cross_project_references() {
    let task = Task {
        inputs: vec![
            Input::Path("#projA:build:dist/lib.js".to_string()),
            Input::Path("#projB:compile:out/types.d.ts".to_string()),
            Input::Path("src/**/*.ts".to_string()), // Local path
        ],
        ..Default::default()
    };

    let mut cuenv = Project::new("test");
    cuenv
        .tasks
        .insert("bundle".into(), TaskNode::Task(Box::new(task)));

    cuenv.expand_cross_project_references();

    let task_def = cuenv.tasks.get("bundle").unwrap();
    let task = task_def.as_task().unwrap();

    // Should have 3 inputs (2 project refs + 1 local)
    assert_eq!(task.inputs.len(), 3);

    // Should have 2 implicit dependencies
    assert_eq!(task.depends_on.len(), 2);
    assert!(
        task.depends_on
            .iter()
            .any(|d| d.task_name() == "#projA:build")
    );
    assert!(
        task.depends_on
            .iter()
            .any(|d| d.task_name() == "#projB:compile")
    );
}

#[test]
fn test_expand_cross_project_in_task_group() {
    let task1 = Task {
        command: "step1".to_string(),
        inputs: vec![Input::Path("#projA:build:dist/lib.js".to_string())],
        ..Default::default()
    };

    let task2 = Task {
        command: "step2".to_string(),
        inputs: vec![Input::Path("#projB:compile:out/types.d.ts".to_string())],
        ..Default::default()
    };

    let mut cuenv = Project::new("test");
    cuenv.tasks.insert(
        "pipeline".into(),
        TaskNode::Sequence(vec![
            TaskNode::Task(Box::new(task1)),
            TaskNode::Task(Box::new(task2)),
        ]),
    );

    cuenv.expand_cross_project_references();

    // Verify expansion happened in both tasks
    match cuenv.tasks.get("pipeline").unwrap() {
        TaskNode::Sequence(steps) => {
            match &steps[0] {
                TaskNode::Task(task) => {
                    assert!(
                        task.depends_on
                            .iter()
                            .any(|d| d.task_name() == "#projA:build")
                    );
                }
                _ => panic!("Expected single task"),
            }
            match &steps[1] {
                TaskNode::Task(task) => {
                    assert!(
                        task.depends_on
                            .iter()
                            .any(|d| d.task_name() == "#projB:compile")
                    );
                }
                _ => panic!("Expected single task"),
            }
        }
        _ => panic!("Expected task list"),
    }
}

#[test]
fn test_expand_cross_project_in_parallel_group() {
    let task1 = Task {
        command: "taskA".to_string(),
        inputs: vec![Input::Path("#projA:build:lib.js".to_string())],
        ..Default::default()
    };

    let task2 = Task {
        command: "taskB".to_string(),
        inputs: vec![Input::Path("#projB:build:types.d.ts".to_string())],
        ..Default::default()
    };

    let mut parallel_tasks = HashMap::new();
    parallel_tasks.insert("a".to_string(), TaskNode::Task(Box::new(task1)));
    parallel_tasks.insert("b".to_string(), TaskNode::Task(Box::new(task2)));

    let mut cuenv = Project::new("test");
    cuenv.tasks.insert(
        "parallel".into(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    cuenv.expand_cross_project_references();

    // Verify expansion happened in both parallel tasks
    match cuenv.tasks.get("parallel").unwrap() {
        TaskNode::Group(group) => {
            match group.children.get("a").unwrap() {
                TaskNode::Task(task) => {
                    assert!(
                        task.depends_on
                            .iter()
                            .any(|d| d.task_name() == "#projA:build")
                    );
                }
                _ => panic!("Expected single task"),
            }
            match group.children.get("b").unwrap() {
                TaskNode::Task(task) => {
                    assert!(
                        task.depends_on
                            .iter()
                            .any(|d| d.task_name() == "#projB:build")
                    );
                }
                _ => panic!("Expected single task"),
            }
        }
        _ => panic!("Expected parallel group"),
    }
}

#[test]
fn test_no_duplicate_implicit_dependencies() {
    // Task already has the dependency explicitly
    let task = Task {
        depends_on: vec![TaskDependency::from_name("#myproj:build")],
        inputs: vec![Input::Path("#myproj:build:dist/app.js".to_string())],
        ..Default::default()
    };

    let mut cuenv = Project::new("test");
    cuenv
        .tasks
        .insert("deploy".into(), TaskNode::Task(Box::new(task)));

    cuenv.expand_cross_project_references();

    let task_def = cuenv.tasks.get("deploy").unwrap();
    let task = task_def.as_task().unwrap();

    // Should not duplicate the dependency
    assert_eq!(task.depends_on.len(), 1);
    assert_eq!(task.depends_on[0].task_name(), "#myproj:build");
}

// ============================================================================
// Project Hooks (onEnter, onExit) Tests
// ============================================================================

#[test]
fn test_on_enter_hooks_ordering() {
    let mut on_enter = HashMap::new();
    on_enter.insert("hook_c".to_string(), create_test_hook(300, "echo c"));
    on_enter.insert("hook_a".to_string(), create_test_hook(100, "echo a"));
    on_enter.insert("hook_b".to_string(), create_test_hook(200, "echo b"));

    let mut cuenv = Project::new("test");
    cuenv.hooks = Some(Hooks {
        on_enter: Some(on_enter),
        on_exit: None,
        pre_push: None,
    });

    let hooks = cuenv.on_enter_hooks();
    assert_eq!(hooks.len(), 3);

    // Should be sorted by order
    assert_eq!(hooks[0].order, 100);
    assert_eq!(hooks[1].order, 200);
    assert_eq!(hooks[2].order, 300);
}

#[test]
fn test_on_enter_hooks_same_order_sort_by_name() {
    let mut on_enter = HashMap::new();
    on_enter.insert("z_hook".to_string(), create_test_hook(100, "echo z"));
    on_enter.insert("a_hook".to_string(), create_test_hook(100, "echo a"));

    let cuenv = Project {
        name: "test".to_string(),
        hooks: Some(Hooks {
            on_enter: Some(on_enter),
            on_exit: None,
            pre_push: None,
        }),
        ..Default::default()
    };

    let hooks = cuenv.on_enter_hooks();
    assert_eq!(hooks.len(), 2);

    // Same order, should be sorted by name
    assert_eq!(hooks[0].command, "echo a");
    assert_eq!(hooks[1].command, "echo z");
}

#[test]
fn test_empty_hooks() {
    let cuenv = Project::new("test");

    let on_enter = cuenv.on_enter_hooks();
    let on_exit = cuenv.on_exit_hooks();

    assert!(on_enter.is_empty());
    assert!(on_exit.is_empty());
}

#[test]
fn test_project_deserialization_with_script_tasks() {
    // This test uses the new explicit API with type: "group" and flattened children
    let json = r#"{
        "name": "cuenv",
        "hooks": {
            "onEnter": {
                "nix": {
                    "order": 10,
                    "propagate": false,
                    "command": "nix",
                    "args": ["print-dev-env"],
                    "inputs": ["flake.nix", "flake.lock"],
                    "source": true
                }
            }
        },
        "tasks": {
            "pwd": { "command": "pwd" },
            "check": {
                "command": "nix",
                "args": ["flake", "check"],
                "inputs": ["flake.nix"]
            },
            "fmt": {
                "type": "group",
                "fix": {
                    "command": "treefmt",
                    "inputs": [".config"]
                },
                "check": {
                    "command": "treefmt",
                    "args": ["--fail-on-change"],
                    "inputs": [".config"]
                }
            },
            "cross": {
                "type": "group",
                "linux": {
                    "script": "echo building for linux",
                    "inputs": ["Cargo.toml"]
                }
            },
            "docs": {
                "type": "group",
                "build": {
                    "command": "bash",
                    "args": ["-c", "bun install"],
                    "inputs": ["docs"],
                    "outputs": ["docs/dist"]
                },
                "deploy": {
                    "command": "bash",
                    "args": ["-c", "wrangler deploy"],
                    "dependsOn": ["docs.build"],
                    "inputs": [{"task": "docs.build"}]
                }
            }
        }
    }"#;

    let result: Result<Project, _> = serde_json::from_str(json);
    match result {
        Ok(project) => {
            assert_eq!(project.name, "cuenv");
            assert_eq!(project.tasks.len(), 5);
            assert!(project.tasks.contains_key("pwd"));
            assert!(project.tasks.contains_key("cross"));
            // Verify cross is a group with parallel subtasks
            let cross = project.tasks.get("cross").unwrap();
            assert!(cross.is_group());
        }
        Err(e) => {
            panic!("Failed to deserialize Project with script tasks: {}", e);
        }
    }
}

#[test]
fn test_deserialize_actual_cuenv_project() {
    // Read actual CUE output from /tmp/project.json (created by cue eval)
    let json = match std::fs::read_to_string("/tmp/project.json") {
        Ok(content) => content,
        Err(_) => return, // Skip if file doesn't exist
    };
    let result: Result<Project, _> = serde_json::from_str(&json);
    match result {
        Ok(project) => {
            eprintln!("Project name: {}", project.name);
            eprintln!("Tasks: {:?}", project.tasks.keys().collect::<Vec<_>>());
        }
        Err(e) => {
            eprintln!("Failed: {}", e);
            eprintln!("Line: {}, Col: {}", e.line(), e.column());
            // Read the JSON around the error line
            let lines: Vec<&str> = json.lines().collect();
            let line_num = e.line();
            let start = if line_num > 3 { line_num - 3 } else { 1 };
            let end = std::cmp::min(line_num + 3, lines.len());
            for i in start..=end {
                if i <= lines.len() {
                    eprintln!("{}: {}", i, lines[i - 1]);
                }
            }
            panic!("Deserialization failed");
        }
    }
}
