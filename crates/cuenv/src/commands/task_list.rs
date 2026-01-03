//! Task list data structures and formatters.
//!
//! This module provides a clean separation between task list data and its presentation.
//! The `TaskListData` structure captures all information about available tasks,
//! while `TaskListFormatter` implementations handle different output formats.

use cuenv_cache::tasks as task_cache;
use cuenv_core::tasks::{IndexedTask, TaskDefinition, TaskGroup};
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
            TaskGroup::Sequential(tasks) => tasks.first().map_or(0, get_dep_count),
            TaskGroup::Parallel(parallel) => {
                // Get first task from parallel group
                parallel.tasks.values().next().map_or(0, get_dep_count)
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

/// Plain text formatter (no colors or styling, suitable for piping).
#[derive(Debug, Default)]
pub struct TextFormatter;

impl TaskListFormatter for TextFormatter {
    fn format(&self, data: &TaskListData) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        for (i, group) in data.sources.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            // Use source in header if available, otherwise just the header
            if group.source.is_empty() {
                let _ = writeln!(output, "{}:", group.header);
            } else {
                let _ = writeln!(output, "{} ({}):", group.header, group.source);
            }

            let max_width = calculate_max_width(&group.nodes, 0);
            format_text_nodes(&group.nodes, &mut output, max_width, "");
        }

        if output.is_empty() {
            output = "No tasks defined in the configuration".to_string();
        } else {
            // Append stats summary
            let _ = writeln!(
                output,
                "\n({} tasks, {} groups, {} cached)",
                data.stats.total_tasks, data.stats.total_groups, data.stats.cached_count
            );
        }

        output
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

        // For executable tasks, show the full_name (e.g., "bun.install"); for groups just the name
        let display_name = if node.is_group {
            &node.name
        } else {
            node.full_name.as_deref().unwrap_or(&node.name)
        };
        let current_len =
            prefix.chars().count() + marker.chars().count() + display_name.chars().count();

        let _ = write!(output, "{prefix}{marker}{display_name}");

        // Show dependency count if there are dependencies
        if node.dep_count > 0 {
            let _ = write!(output, " [{}]", node.dep_count);
        }

        if let Some(desc) = &node.description {
            let padding = max_width.saturating_sub(current_len);
            let dots = ".".repeat(padding + 4);
            let _ = write!(output, " {dots} {desc}");
        }
        let _ = writeln!(output);

        let child_prefix = if is_last { "   " } else { "│  " };
        let new_prefix = format!("{prefix}{child_prefix}");
        format_text_nodes(&node.children, output, max_width, &new_prefix);
    }
}

// ============================================================================
// RichFormatter - Colored Output (same structure as text, with colors)
// ============================================================================

/// Rich formatter with colors but same tree structure as text (no box frame).
#[derive(Debug)]
pub struct RichFormatter {
    /// Whether to use colors (auto-detected from TTY).
    pub use_colors: bool,
}

impl Default for RichFormatter {
    fn default() -> Self {
        Self { use_colors: true }
    }
}

impl RichFormatter {
    /// Create a new formatter with auto-detected settings.
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
        use std::fmt::Write;
        let mut output = String::new();

        for (i, group) in data.sources.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            // Simple bold header, no box
            let _ = writeln!(output, "{}:", self.bold(&group.header));

            let max_width = calculate_max_width(&group.nodes, 0);
            format_rich_nodes(self, &group.nodes, &mut output, max_width, "");
        }

        if output.is_empty() {
            output = "No tasks defined in the configuration".to_string();
        }

        output
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
        let _ = write!(output, "{prefix}{}", formatter.dim(marker));

        // Colored name: cyan for tasks, dim for groups
        let colored_name = if node.is_group {
            formatter.dim(&node.name)
        } else {
            formatter.cyan(&node.name)
        };
        let _ = write!(output, "{colored_name}");

        if let Some(desc) = &node.description {
            let padding = max_width.saturating_sub(current_len);
            let dots = formatter.dim(&".".repeat(padding + 4));
            let _ = write!(output, " {dots} {desc}");
        }
        let _ = writeln!(output);

        let child_prefix = if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}{}  ", formatter.dim("│"))
        };
        format_rich_nodes(formatter, &node.children, output, max_width, &child_prefix);
    }
}

// ============================================================================
// TablesFormatter - Category-Grouped Tables
// ============================================================================

/// Tables formatter that groups tasks by namespace into bordered tables.
#[derive(Debug, Default)]
pub struct TablesFormatter {
    /// Whether to use colors (auto-detected from TTY).
    pub use_colors: bool,
}

impl TablesFormatter {
    /// Create a new formatter with auto-detected settings.
    #[must_use]
    pub fn new() -> Self {
        use std::io::IsTerminal;
        let use_colors = std::io::stdout().is_terminal();
        Self { use_colors }
    }

    fn cyan(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[36m{s}\x1b[0m")
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

impl TaskListFormatter for TablesFormatter {
    fn format(&self, data: &TaskListData) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        if data.sources.is_empty() || data.stats.total_tasks == 0 {
            return "No tasks defined in the configuration".to_string();
        }

        // Collect all tasks with their categories
        let mut categorized_tasks: CategorizedTasks = BTreeMap::new();

        for group in &data.sources {
            collect_tasks_for_tables(&group.nodes, &mut categorized_tasks, "");
        }

        // Render each category as a table
        for (i, (category, tasks)) in categorized_tasks.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            // Calculate column widths (use plain text lengths, not styled)
            let max_name = tasks
                .iter()
                .map(|(n, _, _)| n.len())
                .max()
                .unwrap_or(10)
                .max(10);
            let max_desc = tasks
                .iter()
                .map(|(_, d, _)| d.len())
                .max()
                .unwrap_or(20)
                .max(20);
            let dep_width = 7; // "N deps" or empty

            let total_width = max_name + max_desc + dep_width + 6; // +6 for separators and padding

            // Header (use plain text for width calculations)
            let _ = writeln!(output, "╭─{}─╮", "─".repeat(total_width));
            let category_upper = category.to_uppercase();
            let category_display = self.bold(&category_upper);
            let padding = total_width.saturating_sub(category_upper.len()); // Use original length
            let _ = writeln!(output, "│ {}{} │", category_display, " ".repeat(padding));

            // Column headers
            let _ = write!(output, "├─");
            let _ = write!(output, "{}─┬─", "─".repeat(max_name));
            let _ = write!(output, "{}─┬─", "─".repeat(max_desc));
            let _ = writeln!(output, "{}─┤", "─".repeat(dep_width));

            // Rows
            for (name, desc, dep_count) in tasks {
                let name_display = self.cyan(name);
                let name_padding = max_name.saturating_sub(name.len()); // Use original length
                let desc_padding = max_desc.saturating_sub(desc.len());
                let dep_display = if *dep_count > 0 {
                    format!("{} dep{}", dep_count, if *dep_count > 1 { "s" } else { "" })
                } else {
                    String::new()
                };
                let dep_padding = dep_width.saturating_sub(dep_display.len());

                let _ = writeln!(
                    output,
                    "│ {}{} │ {}{} │ {}{} │",
                    name_display,
                    " ".repeat(name_padding),
                    desc,
                    " ".repeat(desc_padding),
                    dep_display,
                    " ".repeat(dep_padding)
                );
            }

            // Footer
            let _ = write!(output, "╰─");
            let _ = write!(output, "{}─┴─", "─".repeat(max_name));
            let _ = write!(output, "{}─┴─", "─".repeat(max_desc));
            let _ = writeln!(output, "{}─╯", "─".repeat(dep_width));
        }

        // Summary
        let _ = writeln!(
            output,
            "\n{} tasks in {} categories",
            data.stats.total_tasks,
            categorized_tasks.len()
        );

        output
    }
}

/// Type alias for categorized task entries (name, description, dep_count)
type CategorizedTasks = BTreeMap<String, Vec<(String, String, usize)>>;

/// Collect tasks into categories for tables formatter
fn collect_tasks_for_tables(nodes: &[TaskNode], categories: &mut CategorizedTasks, prefix: &str) {
    for node in nodes {
        let full_name = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{prefix}.{}", node.name)
        };

        if !node.is_group {
            // Determine category from namespace
            let category = if full_name.contains('.') {
                let parts: Vec<&str> = full_name.split('.').collect();
                format_category_name(parts[0])
            } else {
                "General".to_string()
            };

            let task_name = node.full_name.as_deref().unwrap_or(&node.name);
            let description = node.description.as_deref().unwrap_or("").to_string();

            categories.entry(category).or_default().push((
                task_name.to_string(),
                description,
                node.dep_count,
            ));
        }

        // Recurse into children
        collect_tasks_for_tables(&node.children, categories, &full_name);
    }
}

/// Format category name from namespace prefix
fn format_category_name(prefix: &str) -> String {
    match prefix {
        "build" => "Build & Compile".to_string(),
        "test" => "Testing".to_string(),
        "lint" | "fmt" | "check" => "Code Quality".to_string(),
        "cargo" | "bun" | "npm" | "go" => format!("{} Tasks", prefix.to_uppercase()),
        "security" | "audit" => "Security".to_string(),
        "publish" | "release" | "deploy" => "Release".to_string(),
        "docker" | "container" => "Containers".to_string(),
        "ci" | "cd" => "CI/CD".to_string(),
        _ => format!("{} Tasks", prefix),
    }
}

// ============================================================================
// DashboardFormatter - Status Dashboard
// ============================================================================

/// Dashboard formatter with rich status display and cache information.
#[derive(Debug, Default)]
pub struct DashboardFormatter {
    /// Whether to use colors (auto-detected from TTY).
    pub use_colors: bool,
}

impl DashboardFormatter {
    /// Create a new formatter with auto-detected settings.
    #[must_use]
    pub fn new() -> Self {
        use std::io::IsTerminal;
        let use_colors = std::io::stdout().is_terminal();
        Self { use_colors }
    }

    fn green(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn yellow(&self, s: &str) -> String {
        if self.use_colors {
            format!("\x1b[33m{s}\x1b[0m")
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

impl TaskListFormatter for DashboardFormatter {
    fn format(&self, data: &TaskListData) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        if data.sources.is_empty() || data.stats.total_tasks == 0 {
            return "No tasks defined in the configuration".to_string();
        }

        // Collect all tasks
        let mut all_tasks: Vec<DashboardTask> = Vec::new();
        for group in &data.sources {
            collect_tasks_for_dashboard(&group.nodes, &mut all_tasks, "");
        }

        // Calculate column widths
        let max_group = all_tasks
            .iter()
            .map(|t| t.group.len())
            .max()
            .unwrap_or(10)
            .max(10);
        let max_name = all_tasks
            .iter()
            .map(|t| t.name.len())
            .max()
            .unwrap_or(15)
            .max(15);
        let status_width = 9; // "● cached" or "○ stale"
        let time_width = 15; // "2 mins ago" etc.

        let total_width = max_group + max_name + status_width + time_width + 10;

        // Header (use plain "Tasks" length for calculation, not styled)
        let _ = writeln!(
            output,
            "┌─ {} {}─┐",
            self.bold("Tasks"),
            "─".repeat(total_width.saturating_sub(8))
        );
        let _ = writeln!(
            output,
            "│ {:width_g$}  {:width_n$}  {:width_s$}  {:width_t$} │",
            self.bold("GROUP"),
            self.bold("TASK"),
            self.bold("STATUS"),
            self.bold("LAST RUN"),
            width_g = max_group,
            width_n = max_name,
            width_s = status_width,
            width_t = time_width
        );
        let _ = writeln!(output, "│ {}─┤", "─".repeat(total_width));

        // Group tasks by namespace
        let mut last_group = String::new();
        for task in &all_tasks {
            let group_display = if task.group == last_group {
                " ".repeat(max_group)
            } else {
                last_group.clone_from(&task.group);
                format!("{:width$}", task.group, width = max_group)
            };

            let status_icon = if task.is_cached {
                self.green("● cached")
            } else {
                self.yellow("○ stale")
            };

            let time_display = if task.is_cached {
                self.dim("recently")
            } else {
                self.dim("never")
            };

            let _ = writeln!(
                output,
                "│ {}  {:width_n$}  {}  {:width_t$} │",
                group_display,
                task.name,
                status_icon,
                time_display,
                width_n = max_name,
                width_t = time_width
            );
        }

        // Footer
        let _ = writeln!(output, "└{}─┘", "─".repeat(total_width + 1));

        // Summary stats
        let stale_count = data.stats.total_tasks - data.stats.cached_count;
        let _ = writeln!(
            output,
            "  {} {} cached  {} {} stale  {} │ {} total",
            self.green("●"),
            data.stats.cached_count,
            self.yellow("○"),
            stale_count,
            self.dim("◌ 0 running"),
            data.stats.total_tasks
        );

        output
    }
}

#[derive(Debug)]
struct DashboardTask {
    group: String,
    name: String,
    is_cached: bool,
}

/// Collect tasks for dashboard formatter
fn collect_tasks_for_dashboard(nodes: &[TaskNode], tasks: &mut Vec<DashboardTask>, prefix: &str) {
    for node in nodes {
        let full_name = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{prefix}.{}", node.name)
        };

        if !node.is_group {
            let group = if full_name.contains('.') {
                let parts: Vec<&str> = full_name.split('.').collect();
                parts[0].to_string()
            } else {
                "root".to_string()
            };

            let task_name = node.name.clone();

            tasks.push(DashboardTask {
                group,
                name: task_name,
                is_cached: node.is_cached,
            });
        }

        // Recurse into children
        collect_tasks_for_dashboard(&node.children, tasks, &full_name);
    }
}

// ============================================================================
// EmojiFormatter - Emoji Taxonomy
// ============================================================================

/// Emoji formatter with semantic emoji prefixes based on task names.
#[derive(Debug, Default)]
pub struct EmojiFormatter;

impl TaskListFormatter for EmojiFormatter {
    fn format(&self, data: &TaskListData) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        if data.sources.is_empty() || data.stats.total_tasks == 0 {
            return "No tasks defined in the configuration".to_string();
        }

        // Collect and categorize tasks
        let mut categorized_tasks: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

        for group in &data.sources {
            collect_tasks_for_emoji(&group.nodes, &mut categorized_tasks, "");
        }

        // Calculate max name width for alignment
        let max_name_width = categorized_tasks
            .values()
            .flatten()
            .map(|(name, _)| name.len())
            .max()
            .unwrap_or(20)
            .max(20);

        // Render each category with emoji
        for (category, tasks) in &categorized_tasks {
            let emoji = get_category_emoji(category);
            let _ = writeln!(output, "\n{} {}", emoji, category);

            for (name, desc) in tasks {
                if desc.is_empty() {
                    let _ = writeln!(output, "   {}", name);
                } else {
                    let _ = writeln!(
                        output,
                        "   {:width$} {}",
                        name,
                        desc,
                        width = max_name_width
                    );
                }
            }
        }

        // Summary footer
        let cached_emoji = if data.stats.cached_count > 0 {
            "🎯"
        } else {
            "⚪"
        };
        let _ = writeln!(
            output,
            "\n📦 {} tasks │ {} {} cached │ ⚡ Run: cuenv t <name>",
            data.stats.total_tasks, cached_emoji, data.stats.cached_count
        );

        output
    }
}

/// Collect tasks into categories for emoji formatter
fn collect_tasks_for_emoji(
    nodes: &[TaskNode],
    categories: &mut BTreeMap<String, Vec<(String, String)>>,
    prefix: &str,
) {
    for node in nodes {
        let full_name = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{prefix}.{}", node.name)
        };

        if !node.is_group {
            // Determine category from task name
            let category = infer_category_from_name(&full_name, node.description.as_deref());
            let task_full_name = node.full_name.as_deref().unwrap_or(&node.name).to_string();
            let description = node.description.as_deref().unwrap_or("").to_string();

            categories
                .entry(category)
                .or_default()
                .push((task_full_name, description));
        }

        // Recurse into children
        collect_tasks_for_emoji(&node.children, categories, &full_name);
    }
}

/// Infer category from task name and description
fn infer_category_from_name(name: &str, description: Option<&str>) -> String {
    let name_lower = name.to_lowercase();
    let desc_lower = description.map(|d| d.to_lowercase()).unwrap_or_default();
    let combined = format!("{} {}", name_lower, desc_lower);

    // Check for containers first (higher priority than build)
    if combined.contains("docker") || combined.contains("container") || combined.contains("image") {
        return "Containers".to_string();
    }

    if combined.contains("security") || combined.contains("audit") || combined.contains("vuln") {
        return "Security".to_string();
    }

    if combined.contains("publish") || combined.contains("release") || combined.contains("deploy") {
        return "Release".to_string();
    }

    if combined.contains("test") || combined.contains("spec") || combined.contains("bench") {
        return "Testing".to_string();
    }

    if combined.contains("lint")
        || combined.contains("fmt")
        || combined.contains("format")
        || combined.contains("check")
    {
        return "Code Quality".to_string();
    }

    if combined.contains("build") || combined.contains("compile") || combined.contains("install") {
        return "Build & Compile".to_string();
    }

    if combined.contains("doc") || combined.contains("documentation") {
        return "Documentation".to_string();
    }

    if combined.contains("clean") || combined.contains("reset") {
        return "Maintenance".to_string();
    }

    if combined.contains("ci") || combined.contains("cd") {
        return "CI/CD".to_string();
    }

    "Other".to_string()
}

/// Get emoji for category
fn get_category_emoji(category: &str) -> &'static str {
    match category {
        "Build & Compile" => "🔨",
        "Testing" => "🧪",
        "Code Quality" => "✨",
        "Release" => "🚀",
        "Security" => "🔐",
        "Containers" => "🐳",
        "Documentation" => "📚",
        "Maintenance" => "🧹",
        "CI/CD" => "⚙️",
        _ => "📋",
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
        children.insert(
            "child1".to_string(),
            super::TreeBuilder {
                name: "child1".to_string(),
                is_task: true,
                is_cached: true,
                ..Default::default()
            },
        );

        // Child 2: Not cached
        children.insert(
            "child2".to_string(),
            super::TreeBuilder {
                name: "child2".to_string(),
                is_task: true,
                is_cached: false,
                ..Default::default()
            },
        );

        let mut root_builder = BTreeMap::new();
        root_builder.insert(
            "group".to_string(),
            super::TreeBuilder {
                name: "group".to_string(),
                is_task: false,   // It's a group
                is_cached: false, // Initially false
                children,
                ..Default::default()
            },
        );

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
