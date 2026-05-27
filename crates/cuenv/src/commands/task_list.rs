//! Task list data structures and formatters.
//!
//! This module provides a clean separation between task list data and its presentation.
//! The `TaskListData` structure captures all information about available tasks,
//! while `TaskListFormatter` implementations handle different output formats.

mod formatters;

pub use formatters::{
    DashboardFormatter, EmojiFormatter, RichFormatter, TablesFormatter, TextFormatter,
};

#[cfg(test)]
pub(super) use formatters::{format_category_name, get_category_emoji, infer_category_from_name};

use cuenv_core::tasks::{IndexedTask, TaskNode as CoreTaskNode};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

// ============================================================================
// Data Structures
// ============================================================================

/// Complete task list data extracted from indexed tasks.
#[derive(Debug, Clone)]
pub struct TaskListData {
    /// Groups of tasks organized by source file.
    pub sources: Vec<TaskSourceGroup>,
    /// Aggregate statistics.
    pub stats: TaskListStats,
}

/// A group of tasks from a single source file.
#[derive(Debug, Clone)]
pub struct TaskSourceGroup {
    /// Source file path (empty string = root env.cue).
    pub source: String,
    /// Header to display (e.g., "Tasks:" or "Tasks from projects/foo/env.cue:").
    pub header: String,
    /// Root-level task nodes (tree structure).
    pub nodes: Vec<TaskNode>,
}

/// A node in the task tree (either a task or a namespace group).
#[derive(Debug, Clone)]
pub struct TaskNode {
    /// Display name segment (e.g., "install" for nested, "build" for root).
    pub name: String,
    /// Full executable reference (e.g., "bun.install") - None for groups.
    pub full_name: Option<String>,
    /// Task description.
    pub description: Option<String>,
    /// True if this is a namespace-only node (not executable).
    pub is_group: bool,
    /// Number of dependencies (0 if none).
    pub dep_count: usize,
    /// Whether cached result exists for this task.
    pub is_cached: bool,
    /// Nested child nodes.
    pub children: Vec<Self>,
}

/// Aggregate statistics about the task list.
#[derive(Debug, Clone, Default)]
pub struct TaskListStats {
    /// Total number of executable tasks.
    pub total_tasks: usize,
    /// Total number of namespace groups.
    pub total_groups: usize,
    /// Number of tasks with cached results.
    pub cached_count: usize,
}

// ============================================================================
// Formatter Trait
// ============================================================================

/// Trait for formatting task list data into displayable output.
pub trait TaskListFormatter {
    /// Format the task list data into a displayable string.
    #[must_use]
    fn format(&self, data: &TaskListData) -> String;
}

// ============================================================================
// Build Task List
// ============================================================================

/// Build a `TaskListData` from a list of indexed tasks.
///
/// This function groups tasks by source file, builds a tree structure for
/// hierarchical task names (e.g., "bun.install"), and calculates statistics.
///
/// # Arguments
/// * `tasks` - Slice of indexed tasks from the task index
/// * `cwd_relative` - Optional path relative to cue.mod root for proximity sorting
/// * `project_root` - Project root for cache lookups
#[must_use]
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

/// Collect task names that have cached results.
///
/// The legacy `cuenv_cache` crate has been removed. Although the executor now
/// writes `ActionResult` records to `cuenv_cas`, `task list` still lacks a
/// stable task-name to action-digest mapping, so cache markers are not
/// implemented here yet.
fn collect_cached_tasks(_tasks: &[&IndexedTask], _project_root: &Path) -> HashSet<String> {
    HashSet::new()
}

// Intermediate structure for building the tree
#[derive(Default)]
struct TreeBuilder {
    name: String,
    full_name: Option<String>,
    description: Option<String>,
    is_task: bool,
    is_cached: bool,
    dep_count: usize,
    children: BTreeMap<String, Self>,
}

// Convert TreeBuilder to TaskNode
fn convert(builders: BTreeMap<String, TreeBuilder>, stats: &mut TaskListStats) -> Vec<TaskNode> {
    builders
        .into_values()
        .map(|builder| {
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
                node.dep_count = get_dep_count(&task.node);
                node.is_cached = cached_tasks.contains(&task.name);

                // Extract description from node
                node.description = match &task.node {
                    CoreTaskNode::Task(t) => t.description.clone(),
                    CoreTaskNode::Group(g) => g.description.clone(),
                    CoreTaskNode::Sequence(_) => None,
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

/// Get dependency count from a task node
fn get_dep_count(node: &CoreTaskNode) -> usize {
    match node {
        CoreTaskNode::Task(t) => t.depends_on.len(),
        CoreTaskNode::Group(g) => {
            // Get first task from parallel group
            g.children.values().next().map_or(0, get_dep_count)
        }
        CoreTaskNode::Sequence(steps) => {
            // Get first task from sequence
            steps.first().map_or(0, get_dep_count)
        }
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

#[cfg(test)]
#[path = "task_list_tests.rs"]
mod tests;
