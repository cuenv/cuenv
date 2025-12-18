//! Task list data structures and formatters
//!
//! This module provides a clean separation between task list data and its presentation.
//! The `TaskListData` structure captures all information about available tasks,
//! while `TaskListFormatter` implementations handle different output formats.

use cuenv_core::cache::tasks as task_cache;
use cuenv_core::tasks::{IndexedTask, TaskDefinition, TaskGroup};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

// ============================================================================
// Data Structures
// ============================================================================

/// Complete task list data extracted from indexed tasks
#[derive(Debug, Clone)]
pub struct TaskListData {
    /// Groups of tasks organized by source file
    pub sources: Vec<TaskSourceGroup>,
    /// Aggregate statistics
    pub stats: TaskListStats,
}

/// A group of tasks from a single source file
#[derive(Debug, Clone)]
pub struct TaskSourceGroup {
    /// Source file path (empty string = root env.cue)
    pub source: String,
    /// Header to display (e.g., "Tasks:" or "Tasks from projects/foo/env.cue:")
    pub header: String,
    /// Root-level task nodes (tree structure)
    pub nodes: Vec<TaskNode>,
}

/// A node in the task tree (either a task or a namespace group)
#[derive(Debug, Clone)]
pub struct TaskNode {
    /// Display name segment (e.g., "install" for nested, "build" for root)
    pub name: String,
    /// Full executable reference (e.g., "bun.install") - None for groups
    pub full_name: Option<String>,
    /// Task description
    pub description: Option<String>,
    /// True if this is a namespace-only node (not executable)
    pub is_group: bool,
    /// Number of dependencies (0 if none)
    pub dep_count: usize,
    /// Whether cached result exists for this task
    pub is_cached: bool,
    /// Nested child nodes
    pub children: Vec<TaskNode>,
}

/// Aggregate statistics about the task list
#[derive(Debug, Clone, Default)]
pub struct TaskListStats {
    pub total_tasks: usize,
    pub total_groups: usize,
    pub cached_count: usize,
}

// ============================================================================
// Formatter Trait
// ============================================================================

/// Trait for formatting task list data into displayable output
pub trait TaskListFormatter {
    /// Format the task list data into a displayable string
    fn format(&self, data: &TaskListData) -> String;

    /// Human-readable name of this formatter
    fn name(&self) -> &'static str;
}

// ============================================================================
// Build Task List
// ============================================================================

/// Build a `TaskListData` from a list of indexed tasks
///
/// This function groups tasks by source file, builds a tree structure for
/// hierarchical task names (e.g., "bun.install"), and calculates statistics.
///
/// # Arguments
/// * `tasks` - Slice of indexed tasks from the task index
/// * `cwd_relative` - Optional path relative to cue.mod root for proximity sorting
/// * `project_root` - Project root for cache lookups
pub fn build_task_list(
    tasks: &[&IndexedTask],
    cwd_relative: Option<&str>,
    project_root: &Path,
) -> TaskListData {
    let cached_tasks = collect_cached_tasks(tasks, project_root);
    // Group tasks by source file
    let mut by_source: BTreeMap<String, Vec<&IndexedTask>> = BTreeMap::new();
    for task in tasks {
        let source = task.source_file.clone().unwrap_or_default();
        // Normalize root sources: both "" and "env.cue" are treated as root
        let normalized = if source == "env.cue" {
            String::new()
        } else {
            source
        };
        by_source.entry(normalized).or_default().push(task);
    }

    // Sort source files by proximity to current directory
    let mut sources: Vec<_> = by_source.keys().cloned().collect();
    sources.sort_by(|a, b| {
        let proximity_a = source_proximity(a, cwd_relative);
        let proximity_b = source_proximity(b, cwd_relative);
        proximity_a.cmp(&proximity_b).then(a.cmp(b))
    });

    // Build source groups
    let mut source_groups = Vec::new();
    let mut stats = TaskListStats::default();

    for source in sources {
        let source_tasks = &by_source[&source];
        let header = if source.is_empty() || source == "env.cue" {
            "Tasks".to_string()
        } else {
            format!("Tasks from {source}")
        };

        let (nodes, group_stats) = build_tree_nodes(source_tasks, &cached_tasks);
        stats.total_tasks += group_stats.total_tasks;
        stats.total_groups += group_stats.total_groups;
        stats.cached_count += group_stats.cached_count;

        source_groups.push(TaskSourceGroup {
            source,
            header,
            nodes,
        });
    }

    TaskListData {
        sources: source_groups,
        stats,
    }
}

/// Build tree nodes from a flat list of tasks
/// Collect task names that have cached results
///
/// Queries the cache index to determine which tasks have valid cached
/// results available. This is used to mark tasks in the display output.
fn collect_cached_tasks(tasks: &[&IndexedTask], project_root: &Path) -> HashSet<String> {
    let mut cached = HashSet::new();

    // Batch load the index once for the whole project
    let project_cache_keys = match task_cache::get_project_cache_keys(project_root, None) {
        Ok(Some(keys)) => keys,
        Ok(None) => return cached,
        Err(e) => {
            // Log warning but proceed with empty cache (graceful degradation)
            // Note: tracing might not be initialized in all contexts, but is the standard way here
            tracing::warn!("Failed to load task cache index: {}", e);
            return cached;
        }
    };

    for task in tasks {
        if let Some(cache_key) = project_cache_keys.get(&task.name) {
            // Check if the specific cache entry actually exists on disk
            if task_cache::lookup(cache_key, None).is_some() {
                cached.insert(task.name.clone());
            }
        }
    }
    cached
}

// Intermediate structure for building the tree
struct TreeBuilder {
    name: String,
    full_name: Option<String>,
    description: Option<String>,
    is_task: bool,
    is_cached: bool,
    dep_count: usize,
    children: BTreeMap<String, TreeBuilder>,
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self {
            name: String::new(),
            full_name: None,
            description: None,
            is_task: false,
            is_cached: false,
            dep_count: 0,
            children: BTreeMap::new(),
        }
    }
}

// Convert TreeBuilder to TaskNode
fn convert(
    builders: BTreeMap<String, TreeBuilder>,
    stats: &mut TaskListStats,
) -> Vec<TaskNode> {
    builders
        .into_iter()
        .map(|(_, builder)| {
            let is_group = !builder.is_task;
            if is_group {
                stats.total_groups += 1;
            }

            let child_nodes = convert(builder.children, stats);
            // Propagate cached status: a group is cached if any child is cached
            let group_cached = builder.is_cached || child_nodes.iter().any(|c| c.is_cached);

            TaskNode {
                name: builder.name,
                full_name: builder.full_name,
                description: builder.description,
                is_group,
                dep_count: builder.dep_count,
                is_cached: group_cached,
                children: child_nodes,
            }
        })
        .collect()
}

fn build_tree_nodes(
    tasks: &[&IndexedTask],
    cached_tasks: &HashSet<String>,
) -> (Vec<TaskNode>, TaskListStats) {
    let mut roots: BTreeMap<String, TreeBuilder> = BTreeMap::new();
    let mut stats = TaskListStats::default();

    // Build the tree
    for task in tasks {
        let parts: Vec<&str> = task.name.split('.').collect();
        let mut current_level = &mut roots;

        for (i, part) in parts.iter().enumerate() {
            let is_last = i == parts.len() - 1;
            let node = current_level.entry((*part).to_string()).or_default();
            node.name = (*part).to_string();

            if is_last {
                node.is_task = true;
                node.full_name = Some(task.name.clone());
                node.dep_count = get_dep_count(&task.definition);
                node.is_cached = cached_tasks.contains(&task.name);

                // Extract description from definition
                node.description = match &task.definition {
                    TaskDefinition::Single(t) => t.description.clone(),
                    TaskDefinition::Group(g) => match g {
                        TaskGroup::Sequential(sub) => sub.first().and_then(|t| match t {
                            TaskDefinition::Single(st) => st.description.clone(),
                            TaskDefinition::Group(_) => None,
                        }),
                        TaskGroup::Parallel(_) => None,
                    },
                };

                stats.total_tasks += 1;
                if node.is_cached {
                    stats.cached_count += 1;
                }
            }

            current_level = &mut node.children;
        }
    }

    let nodes = convert(roots, &mut stats);
    (nodes, stats)
}

/// Get dependency count from a task definition
fn get_dep_count(def: &TaskDefinition) -> usize {
    match def {
        TaskDefinition::Single(t) => t.depends_on.len(),
        TaskDefinition::Group(g) => match g {
            TaskGroup::Sequential(tasks) => tasks.first().map(get_dep_count).unwrap_or(0),
            TaskGroup::Parallel(parallel) => {
                // Get first task from parallel group
                parallel
                    .tasks
                    .values()
                    .next()
                    .map(get_dep_count)
                    .unwrap_or(0)
            }
        },
    }
}

/// Calculate proximity of a source file to the current directory
/// Lower value = closer to cwd = should be shown first
fn source_proximity(source: &str, cwd_relative: Option<&str>) -> usize {
    let source_dir = if source.is_empty() {
        ""
    } else {
        std::path::Path::new(source)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
    };

    let cwd = cwd_relative.unwrap_or("");

    if source_dir.is_empty() && cwd.is_empty() {
        return 0;
    }

    if source_dir == cwd {
        return 0;
    }

    if cwd.starts_with(source_dir)
        && (source_dir.is_empty() || cwd[source_dir.len()..].starts_with('/'))
    {
        let source_depth = if source_dir.is_empty() {
            0
        } else {
            source_dir.matches('/').count() + 1
        };
        let cwd_depth = if cwd.is_empty() {
            0
        } else {
            cwd.matches('/').count() + 1
        };
        return cwd_depth - source_depth;
    }

    usize::MAX / 2
}

// ============================================================================
// TextFormatter - Plain Text Output
// ============================================================================

/// Plain text formatter (no colors or styling, suitable for piping)
#[derive(Debug, Default)]
pub struct TextFormatter;

impl TaskListFormatter for TextFormatter {
    fn format(&self, data: &TaskListData) -> String {
        let mut output = String::new();

        for (i, group) in data.sources.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            use std::fmt::Write;
            writeln!(output, "{}:", group.header).expect("write to string");

            let max_width = calculate_max_width(&group.nodes, 0);
            format_text_nodes(&group.nodes, &mut output, max_width, "");
        }

        if output.is_empty() {
            output = "No tasks defined in the configuration".to_string();
        }

        output
    }

    fn name(&self) -> &'static str {
        "text"
    }
}

/// Calculate maximum width needed for alignment in simple format
fn calculate_max_width(nodes: &[TaskNode], depth: usize) -> usize {
    let mut max = 0;
    for node in nodes {
        let len = (depth * 3) + 3 + node.name.len();
        if len > max {
            max = len;
        }
        let child_max = calculate_max_width(&node.children, depth + 1);
        if child_max > max {
            max = child_max;
        }
    }
    max
}

/// Format nodes in text tree format
fn format_text_nodes(nodes: &[TaskNode], output: &mut String, max_width: usize, prefix: &str) {
    use std::fmt::Write;

    let count = nodes.len();
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i == count - 1;
        let marker = if is_last { "└─ " } else { "├─ " };

        let current_len =
            prefix.chars().count() + marker.chars().count() + node.name.chars().count();

        write!(output, "{prefix}{marker}{}", node.name).expect("write to string");

        if let Some(desc) = &node.description {
            let padding = max_width.saturating_sub(current_len);
            let dots = ".".repeat(padding + 4);
            write!(output, " {dots} {desc}").expect("write to string");
        }
        writeln!(output).expect("write to string");

        let child_prefix = if is_last { "   " } else { "│  " };
        let new_prefix = format!("{prefix}{child_prefix}");
        format_text_nodes(&node.children, output, max_width, &new_prefix);
    }
}

// ============================================================================
// RichFormatter - Colored Output (same structure as text, with colors)
// ============================================================================

/// Rich formatter with colors but same tree structure as text (no box frame)
#[derive(Debug)]
pub struct RichFormatter {
    /// Whether to use colors (auto-detected from TTY)
    pub use_colors: bool,
}

impl Default for RichFormatter {
    fn default() -> Self {
        Self { use_colors: true }
    }
}

impl RichFormatter {
    /// Create a new formatter with auto-detected settings
    #[must_use]
    pub fn new() -> Self {
        use std::io::IsTerminal;
        let use_colors = std::io::stdout().is_terminal();
        Self { use_colors }
    }

    // ANSI color helpers
    fn cyan(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[36m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn dim(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

impl TaskListFormatter for RichFormatter {
    fn format(&self, data: &TaskListData) -> String {
        let mut output = String::new();

        for (i, group) in data.sources.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            use std::fmt::Write;
            // Simple bold header, no box
            writeln!(output, "{}:", self.bold(&group.header)).expect("write to string");

            let max_width = calculate_max_width(&group.nodes, 0);
            format_rich_nodes(self, &group.nodes, &mut output, max_width, "");
        }

        if output.is_empty() {
            output = "No tasks defined in the configuration".to_string();
        }

        output
    }

    fn name(&self) -> &'static str {
        "rich"
    }
}

/// Format nodes in rich colored format (same tree structure as text)
fn format_rich_nodes(
    formatter: &RichFormatter,
    nodes: &[TaskNode],
    output: &mut String,
    max_width: usize,
    prefix: &str,
) {
    use std::fmt::Write;

    let count = nodes.len();
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i == count - 1;
        let marker = if is_last { "└─ " } else { "├─ " };

        let current_len =
            prefix.chars().count() + marker.chars().count() + node.name.chars().count();

        // Dim tree connectors
        write!(output, "{prefix}{}", formatter.dim(marker)).expect("write to string");

        // Colored name: cyan for tasks, dim for groups
        let colored_name = if node.is_group {
            formatter.dim(&node.name)
        } else {
            formatter.cyan(&node.name)
        };
        write!(output, "{colored_name}").expect("write to string");

        if let Some(desc) = &node.description {
            let padding = max_width.saturating_sub(current_len);
            let dots = formatter.dim(&".".repeat(padding + 4));
            write!(output, " {dots} {desc}").expect("write to string");
        }
        writeln!(output).expect("write to string");

        let child_prefix = if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}{}  ", formatter.dim("│"))
        };
        format_rich_nodes(formatter, &node.children, output, max_width, &child_prefix);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_text_formatter_name() {
        let formatter = TextFormatter;
        assert_eq!(formatter.name(), "text");
    }

    #[test]
    fn test_rich_formatter_name() {
        let formatter = RichFormatter::default();
        assert_eq!(formatter.name(), "rich");
    }

    #[test]
    fn test_rich_formatter_no_colors() {
        let formatter = RichFormatter { use_colors: false };
        assert!(!formatter.use_colors);
        // Without colors, cyan should return plain text
        assert_eq!(formatter.cyan("test"), "test");
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use tempfile::TempDir;

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
        children.insert("child1".to_string(), super::TreeBuilder {
            name: "child1".to_string(),
            is_task: true,
            is_cached: true,
            ..Default::default()
        });
        
        // Child 2: Not cached
        children.insert("child2".to_string(), super::TreeBuilder {
            name: "child2".to_string(),
            is_task: true,
            is_cached: false,
            ..Default::default()
        });

        let mut root_builder = BTreeMap::new();
        root_builder.insert("group".to_string(), super::TreeBuilder {
            name: "group".to_string(),
            is_task: false, // It's a group
            is_cached: false, // Initially false
            children,
            ..Default::default()
        });

        let nodes = super::convert(root_builder, &mut stats);
        
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
}
