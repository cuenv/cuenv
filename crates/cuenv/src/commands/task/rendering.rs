//! Task rendering and display formatting
//!
//! Handles formatting task lists, tree views, and detailed task information
//! for CLI output.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use cuenv_core::tasks::discovery::TaskDiscovery;
use cuenv_core::tasks::{TaskDefinition, WorkspaceTask};

use super::list_builder::prepare_task_index;

/// Node in a hierarchical task tree for display purposes
#[derive(Default)]
pub struct TaskTreeNode {
    pub description: Option<String>,
    pub children: BTreeMap<String, Self>,
    pub is_task: bool,
}

/// Get CLI help text for the task subcommand
pub fn get_task_cli_help() -> String {
    use clap::CommandFactory;
    let mut cmd = crate::cli::Cli::command();
    // Navigate to the "task" subcommand
    for subcmd in cmd.get_subcommands_mut() {
        if subcmd.get_name() == "task" {
            return subcmd.render_help().to_string();
        }
    }
    // Fallback (shouldn't happen)
    "Execute a task defined in CUE configuration\n\nUsage: cuenv task [OPTIONS] [NAME]".to_string()
}

/// Format detailed information about a single task
pub fn format_task_detail(task: &cuenv_core::tasks::IndexedTask) -> String {
    let mut output = String::new();
    writeln!(output, "Task: {}", task.name).expect("write to string");

    match &task.definition {
        TaskDefinition::Single(t) => {
            if let Some(desc) = &t.description {
                writeln!(output, "Description: {desc}").expect("write to string");
            }
            writeln!(output, "Command: {}", t.command).expect("write to string");
            if !t.args.is_empty() {
                writeln!(output, "Args: {:?}", t.args).expect("write to string");
            }
            if !t.depends_on.is_empty() {
                writeln!(output, "Depends on: {:?}", t.depends_on).expect("write to string");
            }
            if !t.inputs.is_empty() {
                writeln!(output, "Inputs: {:?}", t.inputs).expect("write to string");
            }
            if !t.outputs.is_empty() {
                writeln!(output, "Outputs: {:?}", t.outputs).expect("write to string");
            }
            // Show params if defined
            if let Some(params) = &t.params {
                if !params.positional.is_empty() {
                    writeln!(output, "\nPositional Arguments:").expect("write to string");
                    for (i, param) in params.positional.iter().enumerate() {
                        let required = if param.required { " (required)" } else { "" };
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" [default: {d}]"))
                            .unwrap_or_default();
                        let desc = param
                            .description
                            .as_ref()
                            .map(|d| format!(" - {d}"))
                            .unwrap_or_default();
                        writeln!(output, "  {{{{{i}}}}}{required}{default}{desc}")
                            .expect("write to string");
                    }
                }
                if !params.named.is_empty() {
                    writeln!(output, "\nNamed Arguments:").expect("write to string");
                    let mut names: Vec<_> = params.named.keys().collect();
                    names.sort();
                    for name in names {
                        let param = &params.named[name];
                        let short = param
                            .short
                            .as_ref()
                            .map(|s| format!("-{s}, "))
                            .unwrap_or_default();
                        let required = if param.required { " (required)" } else { "" };
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" [default: {d}]"))
                            .unwrap_or_default();
                        let desc = param
                            .description
                            .as_ref()
                            .map(|d| format!(" - {d}"))
                            .unwrap_or_default();
                        writeln!(output, "  {short}--{name}{required}{default}{desc}")
                            .expect("write to string");
                    }
                }
            }
        }
        TaskDefinition::Group(g) => {
            writeln!(output, "Type: Task Group").expect("write to string");
            match g {
                cuenv_core::tasks::TaskGroup::Sequential(_) => {
                    writeln!(output, "Mode: Sequential").expect("write to string");
                }
                cuenv_core::tasks::TaskGroup::Parallel(_) => {
                    writeln!(output, "Mode: Parallel").expect("write to string");
                }
            }
        }
    }
    output
}

/// Collect all tasks from discovered projects into `WorkspaceTask` format
///
/// The `module_root` parameter is unused but kept for API compatibility.
#[allow(unused_variables)]
pub fn collect_workspace_tasks(
    discovery: &TaskDiscovery,
    module_root: &Path,
) -> Vec<WorkspaceTask> {
    let mut result = Vec::new();

    for project in discovery.projects() {
        let project_name = project.manifest.name.trim();
        if project_name.is_empty() {
            continue;
        }

        // Clone manifest for mutation during prepare_task_index
        let mut manifest = project.manifest.clone();

        // Build task index with auto-detected workspace tasks injected
        // Best-effort: if injection fails, fall back to basic index
        let task_index = prepare_task_index(&mut manifest, &project.project_root).or_else(|_| {
            // Fall back to basic index without workspace injection
            cuenv_core::tasks::TaskIndex::build(&manifest.tasks)
        });

        if let Ok(index) = task_index {
            for entry in index.list() {
                // Get description from task definition (only available for single tasks)
                let description = entry.definition.as_single().and_then(|t| {
                    let desc = t.description();
                    if desc.is_empty() {
                        None
                    } else {
                        Some(desc.to_string())
                    }
                });

                result.push(WorkspaceTask {
                    project: project_name.to_string(),
                    task: entry.name.clone(),
                    task_ref: format!("#{}:{}", project_name, entry.name),
                    description,
                    is_group: entry.is_group,
                });
            }
        }
    }

    result
}

/// Render workspace tasks in human-readable format
pub fn render_workspace_task_list(tasks: &[WorkspaceTask]) -> String {
    if tasks.is_empty() {
        return "No tasks found in workspace".to_string();
    }

    let mut output = String::new();
    let mut by_project: BTreeMap<&str, Vec<&WorkspaceTask>> = BTreeMap::new();

    for task in tasks {
        by_project.entry(&task.project).or_default().push(task);
    }

    for (project, project_tasks) in by_project {
        writeln!(output, "\n{project}").expect("write to string");
        writeln!(output, "{}", "─".repeat(project.len())).expect("write to string");

        for task in project_tasks {
            let desc = task
                .description
                .as_ref()
                .map(|d| format!(" - {d}"))
                .unwrap_or_default();
            writeln!(output, "  {}{}", task.task_ref, desc).expect("write to string");
        }
    }

    output
}

/// Render tasks grouped by source file, ordered by proximity to current directory
///
/// `cwd_relative`: Current working directory relative to cue.mod root (e.g., "projects/foo")
/// Tasks from the current directory are shown first, then progressively further parent dirs
pub fn render_task_tree(
    tasks: Vec<&cuenv_core::tasks::IndexedTask>,
    cwd_relative: Option<&str>,
) -> String {
    // Group tasks by source file
    // Normalize root-level sources: both "" and "env.cue" are treated as root
    let mut by_source: BTreeMap<String, Vec<&cuenv_core::tasks::IndexedTask>> = BTreeMap::new();
    for task in tasks {
        let source = task.source_file.clone().unwrap_or_default();
        // Normalize root sources to empty string so they group together
        let normalized = if source == "env.cue" {
            String::new()
        } else {
            source
        };
        by_source.entry(normalized).or_default().push(task);
    }

    // Sort source files by proximity to current directory
    // - Tasks from cwd come first
    // - Then tasks from parent directories (closest parent first)
    // - Root tasks come last (unless cwd is root)
    let mut sources: Vec<_> = by_source.keys().cloned().collect();
    sources.sort_by(|a, b| {
        let proximity_a = source_proximity(a, cwd_relative);
        let proximity_b = source_proximity(b, cwd_relative);
        // Lower proximity = closer to cwd = should come first
        proximity_a.cmp(&proximity_b).then(a.cmp(b))
    });

    let mut output = String::new();
    for (i, source) in sources.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        // Format header
        let header = if source.is_empty() || source == "env.cue" {
            "Tasks:".to_string()
        } else {
            format!("Tasks from {source}:")
        };
        writeln!(output, "{header}").expect("write to string");

        // Build and render tree for this source's tasks
        let source_tasks = &by_source[source];
        render_source_tasks(source_tasks, &mut output);
    }

    if output.is_empty() {
        output = "No tasks defined in the configuration".to_string();
    }

    output
}

/// Calculate proximity of a source file to the current directory
/// Lower value = closer to cwd = should be shown first
///
/// Returns:
/// - 0 if source is in the same directory as cwd
/// - 1+ for parent directories (1 = immediate parent, 2 = grandparent, etc.)
/// - `usize::MAX` / 2 for unrelated paths (children of cwd)
fn source_proximity(source: &str, cwd_relative: Option<&str>) -> usize {
    // Get the source directory (remove the filename like env.cue)
    let source_dir = if source.is_empty() {
        ""
    } else {
        std::path::Path::new(source)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
    };

    let cwd = cwd_relative.unwrap_or("");

    // If both are root level
    if source_dir.is_empty() && cwd.is_empty() {
        return 0;
    }

    // If source is in the same directory as cwd
    if source_dir == cwd {
        return 0;
    }

    // Check if source is an ancestor of cwd (parent directory)
    if cwd.starts_with(source_dir)
        && (source_dir.is_empty() || cwd[source_dir.len()..].starts_with('/'))
    {
        // Count how many levels up the source is
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
        // Distance = how many levels up from cwd to reach source
        return cwd_depth - source_depth;
    }

    // Source is not an ancestor of cwd (could be a sibling or child)
    // Show these after all ancestors
    usize::MAX / 2
}

/// Render tasks from a single source file as a tree
fn render_source_tasks(tasks: &[&cuenv_core::tasks::IndexedTask], output: &mut String) {
    let mut roots: BTreeMap<String, TaskTreeNode> = BTreeMap::new();

    // Build the tree
    for task in tasks {
        let parts: Vec<&str> = task.name.split('.').collect();
        let mut current_level = &mut roots;

        for (i, part) in parts.iter().enumerate() {
            let is_last = i == parts.len() - 1;
            let node = current_level.entry((*part).to_string()).or_default();

            if is_last {
                node.is_task = true;
                // Extract description from definition
                let desc = match &task.definition {
                    TaskDefinition::Single(t) => t.description.clone(),
                    TaskDefinition::Group(g) => match g {
                        cuenv_core::tasks::TaskGroup::Sequential(sub) => {
                            sub.first().and_then(|t| match t {
                                TaskDefinition::Single(st) => st.description.clone(),
                                TaskDefinition::Group(_) => None,
                            })
                        }
                        cuenv_core::tasks::TaskGroup::Parallel(_) => None,
                    },
                };
                node.description = desc;
            }

            current_level = &mut node.children;
        }
    }

    // Calculate max width for alignment
    let max_width = calculate_tree_width(&roots, 0);

    print_tree_nodes(&roots, output, max_width, "");
}

fn calculate_tree_width(nodes: &BTreeMap<String, TaskTreeNode>, depth: usize) -> usize {
    let mut max = 0;
    for (name, node) in nodes {
        // Length calculation:
        // depth * 3 (indentation) + 3 (marker "├─ ") + name.len()
        // Actually let's be precise with the print logic:
        // Root items: "├─ name" (len = 3 + name)
        // Nested: "│  ├─ name" (len = depth*3 + 3 + name)
        let len = (depth * 3) + 3 + name.len();
        if len > max {
            max = len;
        }
        let child_max = calculate_tree_width(&node.children, depth + 1);
        if child_max > max {
            max = child_max;
        }
    }
    max
}

fn print_tree_nodes(
    nodes: &BTreeMap<String, TaskTreeNode>,
    output: &mut String,
    max_width: usize,
    prefix: &str,
) {
    let count = nodes.len();
    for (i, (name, node)) in nodes.iter().enumerate() {
        let is_last_item = i == count - 1;

        let marker = if is_last_item { "└─ " } else { "├─ " };

        let current_line_len =
            prefix.chars().count() + marker.chars().count() + name.chars().count();

        write!(output, "{prefix}{marker}{name}").expect("write to string");

        if let Some(desc) = &node.description {
            // Pad with dots
            let padding = max_width.saturating_sub(current_line_len);
            // Add a minimum spacing
            let dots = ".".repeat(padding + 4);
            write!(output, " {dots} {desc}").expect("write to string");
        }
        writeln!(output).expect("write to string");

        let child_prefix = if is_last_item { "   " } else { "│  " };
        let new_prefix = format!("{prefix}{child_prefix}");

        print_tree_nodes(&node.children, output, max_width, &new_prefix);
    }
}
