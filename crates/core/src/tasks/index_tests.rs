use super::*;
use crate::tasks::{Task, TaskDependency};

// ==========================================================================
// TaskPath tests
// ==========================================================================

#[test]
fn test_task_path_parse_simple() {
    let path = TaskPath::parse("build").unwrap();
    assert_eq!(path.canonical(), "build");
    assert_eq!(path.segments(), &["build"]);
}

#[test]
fn test_task_path_parse_dotted() {
    let path = TaskPath::parse("test.unit").unwrap();
    assert_eq!(path.canonical(), "test.unit");
    assert_eq!(path.segments(), &["test", "unit"]);
}

#[test]
fn test_task_path_parse_colon_separated() {
    let path = TaskPath::parse("test:integration").unwrap();
    assert_eq!(path.canonical(), "test.integration");
    assert_eq!(path.segments(), &["test", "integration"]);
}

#[test]
fn test_task_path_parse_mixed_separators() {
    let path = TaskPath::parse("build:release.optimized").unwrap();
    assert_eq!(path.canonical(), "build.release.optimized");
}

#[test]
fn test_task_path_parse_empty_error() {
    assert!(TaskPath::parse("").is_err());
    assert!(TaskPath::parse("   ").is_err());
}

#[test]
fn test_task_path_parse_only_separators_error() {
    assert!(TaskPath::parse("...").is_err());
    assert!(TaskPath::parse(":::").is_err());
}

#[test]
fn test_task_path_join() {
    let path = TaskPath::parse("build").unwrap();
    let joined = path.join("release").unwrap();
    assert_eq!(joined.canonical(), "build.release");
}

#[test]
fn test_task_path_join_invalid_segment() {
    let path = TaskPath::parse("build").unwrap();
    assert!(path.join("").is_err());
    assert!(path.join("foo.bar").is_err());
    assert!(path.join("foo:bar").is_err());
}

#[test]
fn test_task_path_equality() {
    let path1 = TaskPath::parse("test.unit").unwrap();
    let path2 = TaskPath::parse("test:unit").unwrap();
    assert_eq!(path1, path2);
}

// ==========================================================================
// validate_segment tests
// ==========================================================================

#[test]
fn test_validate_segment_valid() {
    assert!(validate_segment("build").is_ok());
    assert!(validate_segment("test-unit").is_ok());
    assert!(validate_segment("my_task").is_ok());
    assert!(validate_segment("task123").is_ok());
}

#[test]
fn test_validate_segment_empty() {
    assert!(validate_segment("").is_err());
}

#[test]
fn test_validate_segment_with_dot() {
    assert!(validate_segment("foo.bar").is_err());
}

#[test]
fn test_validate_segment_with_colon() {
    assert!(validate_segment("foo:bar").is_err());
}

// ==========================================================================
// TaskIndex tests
// ==========================================================================

#[test]
fn test_task_index_build_single_task() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo build".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    assert_eq!(index.list().len(), 1);

    let resolved = index.resolve("build").unwrap();
    assert_eq!(resolved.name, "build");
    assert!(!resolved.is_group);
}

#[test]
fn test_task_index_build_underscore_prefix() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "_private".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo private".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();

    // Should be accessible without underscore
    let resolved = index.resolve("private").unwrap();
    assert_eq!(resolved.name, "private");
    assert_eq!(resolved.original_name, "_private");
}

#[test]
fn test_task_index_build_nested_tasks() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "test.unit".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo test".to_string(),
            ..Default::default()
        })),
    );
    tasks.insert(
        "test.integration".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo test --test integration".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    assert_eq!(index.list().len(), 2);

    // Can resolve with dots
    assert!(index.resolve("test.unit").is_ok());
    // Can resolve with colons
    assert!(index.resolve("test:integration").is_ok());
}

#[test]
fn test_task_index_resolve_not_found() {
    let tasks = HashMap::new();
    let index = TaskIndex::build(&tasks).unwrap();

    let result = index.resolve("nonexistent");
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"));
}

#[test]
fn test_task_index_resolve_with_suggestions() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo build".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();

    // Typo: "buld" instead of "build"
    let result = index.resolve("buld");
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(err.contains("Did you mean"));
    assert!(err.contains("build"));
}

#[test]
fn test_task_index_list_deterministic_order() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "zebra".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo z".to_string(),
            ..Default::default()
        })),
    );
    tasks.insert(
        "apple".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo a".to_string(),
            ..Default::default()
        })),
    );
    tasks.insert(
        "mango".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo m".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let list = index.list();

    // BTreeMap should give alphabetical order
    assert_eq!(list[0].name, "apple");
    assert_eq!(list[1].name, "mango");
    assert_eq!(list[2].name, "zebra");
}

#[test]
fn test_task_index_to_tasks() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo build".to_string(),
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let converted = index.to_tasks();

    assert!(converted.tasks.contains_key("build"));
}

// ==========================================================================
// is_similar and levenshtein tests
// ==========================================================================

#[test]
fn test_is_similar_prefix_match() {
    assert!(is_similar("build", "build-release"));
    assert!(is_similar("test", "testing"));
}

#[test]
fn test_is_similar_common_prefix() {
    assert!(is_similar("build", "builder"));
    assert!(is_similar("testing", "tester"));
}

#[test]
fn test_is_similar_edit_distance() {
    assert!(is_similar("build", "buld")); // 1 deletion
    assert!(is_similar("test", "tset")); // 1 transposition
    assert!(is_similar("task", "taks")); // 1 transposition
}

#[test]
fn test_is_similar_not_similar() {
    assert!(!is_similar("build", "zebra"));
    assert!(!is_similar("a", "xyz"));
}

#[test]
fn test_levenshtein_identical() {
    assert_eq!(levenshtein("hello", "hello"), 0);
}

#[test]
fn test_levenshtein_empty() {
    assert_eq!(levenshtein("", "hello"), 5);
    assert_eq!(levenshtein("hello", ""), 5);
    assert_eq!(levenshtein("", ""), 0);
}

#[test]
fn test_levenshtein_single_edit() {
    assert_eq!(levenshtein("cat", "car"), 1); // substitution
    assert_eq!(levenshtein("cat", "cats"), 1); // insertion
    assert_eq!(levenshtein("cats", "cat"), 1); // deletion
}

#[test]
fn test_levenshtein_multiple_edits() {
    assert_eq!(levenshtein("kitten", "sitting"), 3);
}

// ==========================================================================
// IndexedTask tests
// ==========================================================================

#[test]
fn test_indexed_task_debug() {
    let task = IndexedTask {
        name: "build".to_string(),
        original_name: "build".to_string(),
        node: TaskNode::Task(Box::default()),
        is_group: false,
        source_file: Some("env.cue".to_string()),
    };

    let debug = format!("{:?}", task);
    assert!(debug.contains("build"));
    assert!(debug.contains("env.cue"));
}

#[test]
fn test_indexed_task_clone() {
    let task = IndexedTask {
        name: "build".to_string(),
        original_name: "_build".to_string(),
        node: TaskNode::Task(Box::default()),
        is_group: false,
        source_file: None,
    };

    let cloned = task.clone();
    assert_eq!(cloned.name, task.name);
    assert_eq!(cloned.original_name, task.original_name);
}

// ==========================================================================
// WorkspaceTask tests
// ==========================================================================

#[test]
fn test_workspace_task_debug() {
    let task = WorkspaceTask {
        project: "my-project".to_string(),
        task: "build".to_string(),
        task_ref: "#my-project:build".to_string(),
        description: Some("Build the project".to_string()),
        is_group: false,
    };

    let debug = format!("{:?}", task);
    assert!(debug.contains("my-project"));
    assert!(debug.contains("build"));
}

#[test]
fn test_workspace_task_serialize() {
    let task = WorkspaceTask {
        project: "api".to_string(),
        task: "test.unit".to_string(),
        task_ref: "#api:test.unit".to_string(),
        description: None,
        is_group: false,
    };

    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("api"));
    assert!(json.contains("test.unit"));
}

// ==========================================================================
// TaskPath additional tests
// ==========================================================================

#[test]
fn test_task_path_clone() {
    let path = TaskPath::parse("build.release").unwrap();
    let cloned = path.clone();
    assert_eq!(path, cloned);
}

#[test]
fn test_task_path_serialize() {
    let path = TaskPath::parse("test.unit").unwrap();
    let json = serde_json::to_string(&path).unwrap();
    assert!(json.contains("test"));
    assert!(json.contains("unit"));
}

// ==========================================================================
// Dependency resolution tests (bug fix: group child -> top-level task)
// ==========================================================================

#[test]
fn test_task_index_preserves_dependency_names_as_given() {
    // TaskIndex preserves whatever dependency name it receives.
    // Reference resolution happens BEFORE TaskIndex (in module.rs enrichment).
    // This test validates TaskIndex's raw behavior in isolation.

    let mut tasks = HashMap::new();

    // Top-level build task
    tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo build".to_string(),
            ..Default::default()
        })),
    );

    // Deploy group with preview child - dependency name is pre-resolved
    let mut deploy_children = HashMap::new();
    deploy_children.insert(
        "preview".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "deploy preview".to_string(),
            // In practice, enrichment resolves this before TaskIndex sees it.
            // This tests that TaskIndex preserves the name as given.
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );
    tasks.insert(
        "deploy".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: deploy_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let preview_task = index.resolve("deploy.preview").unwrap();

    match &preview_task.node {
        TaskNode::Task(task) => {
            assert_eq!(task.depends_on.len(), 1);
            // TaskIndex preserves names as given - resolution happens earlier
            assert_eq!(task.depends_on[0].task_name(), "build");
        }
        _ => panic!("Expected Task"),
    }
}

#[test]
fn test_group_child_depends_on_sibling_qualified() {
    // When using a qualified path like "deploy.upload", TaskIndex preserves it as-is.
    // Enrichment (module.rs) resolves short names to qualified paths before TaskIndex.

    let mut tasks = HashMap::new();

    let mut deploy_children = HashMap::new();
    deploy_children.insert(
        "upload".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "upload".to_string(),
            ..Default::default()
        })),
    );
    deploy_children.insert(
        "activate".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "activate".to_string(),
            // Qualified path - either from CUE source or enrichment resolution
            depends_on: vec![TaskDependency::from_name("deploy.upload")],
            ..Default::default()
        })),
    );
    tasks.insert(
        "deploy".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: deploy_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let activate_task = index.resolve("deploy.activate").unwrap();

    match &activate_task.node {
        TaskNode::Task(task) => {
            assert_eq!(task.depends_on.len(), 1);
            assert_eq!(task.depends_on[0].task_name(), "deploy.upload");
        }
        _ => panic!("Expected Task"),
    }
}

#[test]
fn test_task_index_preserves_cross_project_separator() {
    let mut tasks = HashMap::new();

    tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "deploy".to_string(),
            depends_on: vec![TaskDependency::from_name("#external:build:preview")],
            ..Default::default()
        })),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let deploy_task = index.resolve("deploy").unwrap();

    match &deploy_task.node {
        TaskNode::Task(task) => {
            assert_eq!(task.depends_on.len(), 1);
            assert_eq!(task.depends_on[0].task_name(), "#external:build.preview");
        }
        _ => panic!("Expected Task"),
    }
}

#[test]
fn test_dotted_dependency_treated_as_absolute() {
    // deploy.preview depends on "other.task" -> treated as absolute path

    let mut tasks = HashMap::new();

    // other.task (as group child)
    let mut other_children = HashMap::new();
    other_children.insert(
        "task".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "other task".to_string(),
            ..Default::default()
        })),
    );
    tasks.insert(
        "other".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: other_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    // Deploy group
    let mut deploy_children = HashMap::new();
    deploy_children.insert(
        "preview".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "deploy preview".to_string(),
            depends_on: vec![TaskDependency::from_name("other.task")],
            ..Default::default()
        })),
    );
    tasks.insert(
        "deploy".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: deploy_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let preview_task = index.resolve("deploy.preview").unwrap();

    match &preview_task.node {
        TaskNode::Task(task) => {
            assert_eq!(task.depends_on.len(), 1);
            assert_eq!(task.depends_on[0].task_name(), "other.task");
        }
        _ => panic!("Expected Task"),
    }
}

#[test]
fn test_cross_group_dependency() {
    // deploy.run depends on "build.compile" -> absolute path to build.compile

    let mut tasks = HashMap::new();

    // Build group
    let mut build_children = HashMap::new();
    build_children.insert(
        "compile".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "compile".to_string(),
            ..Default::default()
        })),
    );
    tasks.insert(
        "build".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: build_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    // Deploy group
    let mut deploy_children = HashMap::new();
    deploy_children.insert(
        "run".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "deploy run".to_string(),
            depends_on: vec![TaskDependency::from_name("build.compile")],
            ..Default::default()
        })),
    );
    tasks.insert(
        "deploy".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: deploy_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let run_task = index.resolve("deploy.run").unwrap();

    match &run_task.node {
        TaskNode::Task(task) => {
            assert_eq!(task.depends_on.len(), 1);
            assert_eq!(task.depends_on[0].task_name(), "build.compile");
        }
        _ => panic!("Expected Task"),
    }
}

#[test]
fn test_task_index_preserves_invalid_references() {
    // TaskIndex preserves all dependency names as given, even invalid ones.
    // Validation (missing task detection) happens later during graph building.
    // This tests TaskIndex in isolation.

    let mut tasks = HashMap::new();

    let mut deploy_children = HashMap::new();
    deploy_children.insert(
        "preview".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "deploy preview".to_string(),
            // Invalid reference - no such task exists
            depends_on: vec![TaskDependency::from_name("nonexistent")],
            ..Default::default()
        })),
    );
    tasks.insert(
        "deploy".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: deploy_children,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let preview_task = index.resolve("deploy.preview").unwrap();

    match &preview_task.node {
        TaskNode::Task(task) => {
            // TaskIndex preserves names as given - validation happens at graph build time
            assert_eq!(task.depends_on[0].task_name(), "nonexistent");
        }
        _ => panic!("Expected Task"),
    }
}
