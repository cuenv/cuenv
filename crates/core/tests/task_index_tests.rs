//! Tests for task indexing and path resolution

use cuenv_core::tasks::{IndexedTask, Task, TaskDefinition, TaskIndex, TaskPath, Tasks};
use std::collections::HashMap;

#[test]
fn test_task_path_parse_simple() {
    let path = TaskPath::parse("build").unwrap();
    assert_eq!(path.canonical(), "build");
    assert_eq!(path.segments(), &["build"]);
}

#[test]
fn test_task_path_parse_dotted() {
    let path = TaskPath::parse("build.release").unwrap();
    assert_eq!(path.canonical(), "build.release");
    assert_eq!(path.segments(), &["build", "release"]);
}

#[test]
fn test_task_path_parse_colon_separator() {
    let path = TaskPath::parse("build:release").unwrap();
    assert_eq!(path.canonical(), "build.release");
    assert_eq!(path.segments(), &["build", "release"]);
}

#[test]
fn test_task_path_parse_mixed_separators() {
    let path = TaskPath::parse("build:release.fast").unwrap();
    assert_eq!(path.canonical(), "build.release.fast");
    assert_eq!(path.segments(), &["build", "release", "fast"]);
}

#[test]
fn test_task_path_parse_empty() {
    let result = TaskPath::parse("");
    assert!(result.is_err());
}

#[test]
fn test_task_path_parse_whitespace_only() {
    let result = TaskPath::parse("   ");
    assert!(result.is_err());
}

#[test]
fn test_task_path_parse_trims_whitespace() {
    let path = TaskPath::parse("  build  .  release  ").unwrap();
    assert_eq!(path.canonical(), "build.release");
    assert_eq!(path.segments(), &["build", "release"]);
}

#[test]
fn test_task_path_parse_multiple_dots() {
    let path = TaskPath::parse("build..release").unwrap();
    assert_eq!(path.canonical(), "build.release");
    assert_eq!(path.segments(), &["build", "release"]);
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
    let result = path.join("has.dot");
    assert!(result.is_err());
}

#[test]
fn test_task_path_join_empty_segment() {
    let path = TaskPath::parse("build").unwrap();
    let result = path.join("");
    assert!(result.is_err());
}

#[test]
fn test_task_index_build_simple() {
    let mut tasks = HashMap::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["test".to_string()],
        ..Default::default()
    };
    tasks.insert("build".to_string(), TaskDefinition::Single(task));

    let index = TaskIndex::build(&tasks).unwrap();
    let entries = index.list();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "build");
}

#[test]
fn test_task_index_strips_underscore_prefix() {
    let mut tasks = HashMap::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["private".to_string()],
        ..Default::default()
    };
    tasks.insert("_private".to_string(), TaskDefinition::Single(task));

    let index = TaskIndex::build(&tasks).unwrap();
    let entries = index.list();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "private");
    assert_eq!(entries[0].original_name, "_private");
}

#[test]
fn test_task_index_resolve_simple() {
    let mut tasks = HashMap::new();
    let task = Task {
        command: "echo".to_string(),
        ..Default::default()
    };
    tasks.insert("build".to_string(), TaskDefinition::Single(task));

    let index = TaskIndex::build(&tasks).unwrap();
    let resolved = index.resolve("build").unwrap();

    assert_eq!(resolved.name, "build");
}

#[test]
fn test_task_index_resolve_colon_notation() {
    let mut tasks = HashMap::new();
    let task = Task {
        command: "echo".to_string(),
        ..Default::default()
    };
    tasks.insert("build.release".to_string(), TaskDefinition::Single(task));

    let index = TaskIndex::build(&tasks).unwrap();
    let resolved = index.resolve("build:release").unwrap();

    assert_eq!(resolved.name, "build.release");
}

#[test]
fn test_task_index_resolve_not_found() {
    let tasks = HashMap::new();
    let index = TaskIndex::build(&tasks).unwrap();
    let result = index.resolve("nonexistent");

    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("not found"));
}

#[test]
fn test_task_index_resolve_suggests_similar() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "build".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );
    tasks.insert(
        "test".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let result = index.resolve("buld");

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Should suggest "build" as a similar task
    assert!(err_msg.contains("Did you mean") || err_msg.contains("Available"));
}

#[test]
fn test_task_index_list_sorted() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "zulu".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );
    tasks.insert(
        "alpha".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );
    tasks.insert(
        "bravo".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let entries = index.list();

    // Should be sorted alphabetically
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "alpha");
    assert_eq!(entries[1].name, "bravo");
    assert_eq!(entries[2].name, "zulu");
}

#[test]
fn test_task_index_to_tasks() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "build".to_string(),
        TaskDefinition::Single(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            ..Default::default()
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let tasks_out = index.to_tasks();

    assert_eq!(tasks_out.tasks.len(), 1);
    assert!(tasks_out.tasks.contains_key("build"));
}

#[test]
fn test_task_index_handles_empty_tasks() {
    let tasks = HashMap::new();
    let index = TaskIndex::build(&tasks).unwrap();

    assert_eq!(index.list().len(), 0);
}

#[test]
fn test_task_path_equality() {
    let path1 = TaskPath::parse("build.release").unwrap();
    let path2 = TaskPath::parse("build:release").unwrap();

    assert_eq!(path1, path2);
    assert_eq!(path1.canonical(), path2.canonical());
}

#[test]
fn test_task_path_clone() {
    let path = TaskPath::parse("build").unwrap();
    let cloned = path.clone();

    assert_eq!(path, cloned);
}

#[test]
fn test_indexed_task_is_group_false_for_single() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "single".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let entry = index.resolve("single").unwrap();

    assert!(!entry.is_group);
}

#[test]
fn test_task_index_multiple_underscore_tasks() {
    let mut tasks = HashMap::new();
    tasks.insert(
        "_private1".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );
    tasks.insert(
        "_private2".to_string(),
        TaskDefinition::Single(Task {
            command: "echo".to_string(),
            ..Default::default()
        }),
    );

    let index = TaskIndex::build(&tasks).unwrap();
    let entries = index.list();

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e.name == "private1"));
    assert!(entries.iter().any(|e| e.name == "private2"));
}

#[test]
fn test_task_path_deep_nesting() {
    let path = TaskPath::parse("build.release.optimized.final").unwrap();
    assert_eq!(path.segments().len(), 4);
    assert_eq!(path.canonical(), "build.release.optimized.final");
}

#[test]
fn test_task_path_join_multiple() {
    let path = TaskPath::parse("build").unwrap();
    let path2 = path.join("release").unwrap();
    let path3 = path2.join("fast").unwrap();

    assert_eq!(path3.canonical(), "build.release.fast");
}
