use super::*;

use tempfile::TempDir;

#[test]
fn test_source_proximity_same_dir() {
    assert_eq!(source_proximity("", None), 0);
    assert_eq!(source_proximity("env.cue", Some("")), 0);
}

#[test]
fn test_task_list_stats_default() {
    let stats = TaskListStats::default();
    assert_eq!(stats.total_tasks, 0);
    assert_eq!(stats.total_groups, 0);
    assert_eq!(stats.cached_count, 0);
}

#[test]
fn test_rich_formatter_no_colors() {
    let formatter = RichFormatter { use_colors: false };
    assert!(!formatter.use_colors);
    // Without colors, cyan should return plain text
    assert_eq!(formatter.cyan("test"), "test");
}

#[test]
fn test_tables_formatter_initialization() {
    let formatter = TablesFormatter::default();
    assert!(!formatter.use_colors);
}

#[test]
fn test_dashboard_formatter_initialization() {
    let formatter = DashboardFormatter::default();
    assert!(!formatter.use_colors);
}

#[test]
fn test_emoji_formatter_can_format_empty() {
    let formatter = EmojiFormatter;
    let data = TaskListData {
        sources: vec![],
        stats: TaskListStats::default(),
    };
    let output = formatter.format(&data);
    assert!(output.contains("No tasks"));
}

#[test]
fn test_format_category_name() {
    assert_eq!(format_category_name("build"), "Build & Compile");
    assert_eq!(format_category_name("test"), "Testing");
    assert_eq!(format_category_name("lint"), "Code Quality");
    assert_eq!(format_category_name("security"), "Security");
    assert_eq!(format_category_name("cargo"), "CARGO Tasks");
}

#[test]
fn test_infer_category_from_name() {
    assert_eq!(infer_category_from_name("build", None), "Build & Compile");
    assert_eq!(infer_category_from_name("test.unit", None), "Testing");
    assert_eq!(infer_category_from_name("lint", None), "Code Quality");
    assert_eq!(infer_category_from_name("publish", None), "Release");
    assert_eq!(infer_category_from_name("security.audit", None), "Security");
    // docker.build should prioritize containers over build
    assert_eq!(infer_category_from_name("docker.build", None), "Containers");
    assert_eq!(infer_category_from_name("container", None), "Containers");
    assert_eq!(infer_category_from_name("unknown", None), "Other");
}

#[test]
fn test_get_category_emoji() {
    assert_eq!(get_category_emoji("Build & Compile"), "🔨");
    assert_eq!(get_category_emoji("Testing"), "🧪");
    assert_eq!(get_category_emoji("Code Quality"), "✨");
    assert_eq!(get_category_emoji("Release"), "🚀");
    assert_eq!(get_category_emoji("Security"), "🔐");
    assert_eq!(get_category_emoji("Other"), "📋");
}

#[test]
fn test_collect_cached_tasks_empty() {
    let temp = TempDir::new().unwrap();
    let tasks = vec![];
    let cached = collect_cached_tasks(&tasks, temp.path());
    assert!(cached.is_empty());
}

#[test]
fn test_group_cache_propagation() {
    let mut stats = TaskListStats::default();
    let mut children = BTreeMap::new();

    // Child 1: Cached
    children.insert(
        "child1".to_string(),
        TreeBuilder {
            name: "child1".to_string(),
            is_task: true,
            is_cached: true,
            ..Default::default()
        },
    );

    // Child 2: Not cached
    children.insert(
        "child2".to_string(),
        TreeBuilder {
            name: "child2".to_string(),
            is_task: true,
            is_cached: false,
            ..Default::default()
        },
    );

    let mut root_builder = BTreeMap::new();
    root_builder.insert(
        "group".to_string(),
        TreeBuilder {
            name: "group".to_string(),
            is_task: false,   // It's a group
            is_cached: false, // Initially false
            children,
            ..Default::default()
        },
    );

    let nodes = convert(root_builder, &mut stats);

    assert_eq!(nodes.len(), 1);
    let group = &nodes[0];
    assert_eq!(group.name, "group");
    assert!(group.is_group);
    // Should be true because child1 is cached
    assert!(group.is_cached);

    assert_eq!(group.children.len(), 2);
    let c1 = group.children.iter().find(|c| c.name == "child1").unwrap();
    assert!(c1.is_cached);
    let c2 = group.children.iter().find(|c| c.name == "child2").unwrap();
    assert!(!c2.is_cached);
}
