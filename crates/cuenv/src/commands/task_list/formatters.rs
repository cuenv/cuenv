//! Task-list formatter implementations.

use super::{TaskListData, TaskListFormatter, TaskNode};
use std::collections::BTreeMap;

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
    pub(super) fn cyan(&self, s: &str) -> String {
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
pub fn format_category_name(prefix: &str) -> String {
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
pub fn infer_category_from_name(name: &str, description: Option<&str>) -> String {
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
pub fn get_category_emoji(category: &str) -> &'static str {
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
