//! Centralized state management for the rich TUI

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

/// Status of a task in the execution graph
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TaskStatus {
    /// Task is waiting for dependencies
    Pending,
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was skipped due to dependency failure
    Skipped,
    /// Task result was retrieved from cache
    Cached,
}

impl TaskStatus {
    /// Get the display symbol for this status
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            TaskStatus::Pending => "⏸",
            TaskStatus::Running => "▶",
            TaskStatus::Completed => "✓",
            TaskStatus::Failed => "✗",
            TaskStatus::Skipped => "⊘",
            TaskStatus::Cached => "⚡",
        }
    }

    /// Get the color for this status (as ratatui Color)
    #[must_use]
    pub fn color(self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            TaskStatus::Running => Color::Yellow,
            TaskStatus::Completed => Color::Green,
            TaskStatus::Failed => Color::Red,
            TaskStatus::Pending | TaskStatus::Skipped => Color::DarkGray,
            TaskStatus::Cached => Color::Cyan,
        }
    }
}

/// Output display mode for the output panel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Show all task outputs grouped by task
    #[default]
    All,
    /// Show only the selected task's output
    Selected,
}

/// Type of node in the tree view
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNodeType {
    /// Special "All" node that shows combined output
    All,
    /// Intermediate group node (e.g., "test" in "test.bdd")
    Group(String),
    /// Actual task node (full task name)
    Task(String),
}

/// Represents a single item in the flattened tree view
#[derive(Debug, Clone)]
pub struct TreeViewItem {
    /// Type of this node
    pub node_type: TreeNodeType,
    /// Display name (e.g., "bdd" instead of full "task:cuenv:test.bdd")
    pub display_name: String,
    /// Depth in the tree (for indentation)
    pub depth: usize,
    /// Whether this node is expanded
    pub is_expanded: bool,
    /// Whether this node has children
    pub has_children: bool,
}

impl TreeViewItem {
    /// Get the unique key for this node (for expansion tracking)
    #[must_use]
    pub fn node_key(&self) -> String {
        match &self.node_type {
            TreeNodeType::All => "::all::".to_string(),
            TreeNodeType::Group(path) => format!("::group::{path}"),
            TreeNodeType::Task(name) => name.clone(),
        }
    }
}

/// Information about a task in the execution graph
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TaskInfo {
    /// Task name
    pub name: String,
    /// Current status
    pub status: TaskStatus,
    /// Task dependencies (names of tasks this depends on)
    pub dependencies: Vec<String>,
    /// Level in the DAG (for visualization)
    pub level: usize,
    /// Start time (if started)
    pub start_time: Option<Instant>,
    /// Duration in milliseconds (if completed)
    pub duration_ms: Option<u64>,
    /// Exit code (if completed)
    pub exit_code: Option<i32>,
}

impl TaskInfo {
    /// Create a new task info
    #[must_use]
    pub fn new(name: String, dependencies: Vec<String>, level: usize) -> Self {
        Self {
            name,
            status: TaskStatus::Pending,
            dependencies,
            level,
            start_time: None,
            duration_ms: None,
            exit_code: None,
        }
    }

    /// Get elapsed time in milliseconds for running tasks
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn elapsed_ms(&self) -> Option<u64> {
        self.start_time
            .map(|start| start.elapsed().as_millis() as u64)
    }
}

/// Maximum number of output lines to keep per stream (stdout/stderr)
const MAX_OUTPUT_LINES: usize = 1000;

/// Output line in the combined output buffer.
///
/// Lines are stored in insertion order (via `VecDeque::push_back`), which
/// naturally preserves chronological order since events arrive sequentially.
#[derive(Debug, Clone)]
pub struct OutputLine {
    /// The line content
    pub content: String,
    /// Whether this is stderr (false = stdout)
    pub is_stderr: bool,
}

/// Output buffer for a running task
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TaskOutput {
    /// Task name
    pub name: String,
    /// Stdout lines (bounded ring buffer)
    pub stdout: VecDeque<String>,
    /// Stderr lines (bounded ring buffer)
    pub stderr: VecDeque<String>,
    /// Combined output with ordering preserved (insertion order = chronological)
    pub combined: VecDeque<OutputLine>,
    /// Whether stdout has new content since last render
    pub stdout_dirty: bool,
    /// Whether stderr has new content since last render
    pub stderr_dirty: bool,
}

impl TaskOutput {
    /// Create a new task output buffer
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            name,
            stdout: VecDeque::new(),
            stderr: VecDeque::new(),
            combined: VecDeque::new(),
            stdout_dirty: false,
            stderr_dirty: false,
        }
    }

    /// Add a line to the appropriate buffer (with bounded buffer management)
    fn add_line(&mut self, line: String, is_stderr: bool) {
        // Select the appropriate buffer and dirty flag
        let (buffer, dirty_flag) = if is_stderr {
            (&mut self.stderr, &mut self.stderr_dirty)
        } else {
            (&mut self.stdout, &mut self.stdout_dirty)
        };

        // Add to stream-specific buffer with size limit
        if buffer.len() >= MAX_OUTPUT_LINES {
            buffer.pop_front();
        }
        buffer.push_back(line.clone());

        // Add to combined output (insertion order preserves chronological order)
        if self.combined.len() >= MAX_OUTPUT_LINES {
            self.combined.pop_front();
        }
        self.combined.push_back(OutputLine {
            content: line,
            is_stderr,
        });

        *dirty_flag = true;
    }

    /// Add a stdout line (with bounded buffer)
    pub fn add_stdout(&mut self, line: String) {
        self.add_line(line, false);
    }

    /// Add a stderr line (with bounded buffer)
    pub fn add_stderr(&mut self, line: String) {
        self.add_line(line, true);
    }

    /// Clear dirty flags after rendering
    #[allow(dead_code)]
    pub fn clear_dirty(&mut self) {
        self.stdout_dirty = false;
        self.stderr_dirty = false;
    }
}

/// Global TUI state for task execution
#[derive(Debug)]
pub struct TuiState {
    /// Start time of the overall execution
    pub start_time: Instant,
    /// Map of task name to task info
    pub tasks: HashMap<String, TaskInfo>,
    /// Map of task name to output buffer
    pub outputs: HashMap<String, TaskOutput>,
    /// Currently running tasks (for split-screen display)
    pub running_tasks: Vec<String>,
    /// Whether execution is complete
    pub is_complete: bool,
    /// Overall success status
    pub success: bool,
    /// Error message (if failed)
    pub error_message: Option<String>,
    /// Currently selected task for output filtering (None = show all)
    pub selected_task: Option<String>,
    /// Set of expanded tree nodes (task names that are expanded)
    pub expanded_nodes: HashSet<String>,
    /// Current cursor position in the flattened tree view
    pub cursor_position: usize,
    /// Cached flattened tree view for navigation
    pub flattened_tree: Vec<TreeViewItem>,
    /// View mode for output panel
    pub output_mode: OutputMode,
    /// Scroll offset for output panel
    pub output_scroll: usize,
}

impl TuiState {
    /// Create a new TUI state
    #[must_use]
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            tasks: HashMap::new(),
            outputs: HashMap::new(),
            running_tasks: Vec::new(),
            is_complete: false,
            success: false,
            error_message: None,
            selected_task: None,
            expanded_nodes: HashSet::new(),
            cursor_position: 0,
            flattened_tree: Vec::new(),
            output_mode: OutputMode::All,
            output_scroll: 0,
        }
    }

    /// Add a task to the state
    pub fn add_task(&mut self, task: TaskInfo) {
        let name = task.name.clone();
        self.tasks.insert(name.clone(), task);
        self.outputs.insert(name.clone(), TaskOutput::new(name));
    }

    /// Update task status
    pub fn update_task_status(&mut self, name: &str, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(name) {
            task.status = status;

            match status {
                TaskStatus::Running => {
                    task.start_time = Some(Instant::now());
                    if !self.running_tasks.contains(&name.to_string()) {
                        self.running_tasks.push(name.to_string());
                    }
                }
                TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cached
                | TaskStatus::Skipped => {
                    if let Some(start) = task.start_time {
                        #[allow(clippy::cast_possible_truncation)]
                        let duration = start.elapsed().as_millis() as u64;
                        task.duration_ms = Some(duration);
                    }
                    self.running_tasks.retain(|t| t != name);
                }
                TaskStatus::Pending => {
                    // Explicitly handle Pending - no action needed
                }
            }
        } else {
            // Task not found in state - this could indicate a synchronization issue
            // between the executor and TUI, or events arriving before task registration
            tracing::warn!(
                "Attempted to update status for unknown task '{}' to {:?}",
                name,
                status
            );
        }
    }

    /// Add output for a task
    pub fn add_task_output(&mut self, name: &str, stream: &str, content: String) {
        if let Some(output) = self.outputs.get_mut(name) {
            match stream {
                "stdout" => output.add_stdout(content),
                "stderr" => output.add_stderr(content),
                unknown => {
                    // Unknown stream type - log and treat as stdout
                    tracing::debug!(
                        "Unknown stream type '{}' for task '{}', treating as stdout",
                        unknown,
                        name
                    );
                    output.add_stdout(content);
                }
            }
        } else {
            // Output buffer not found - task may not have been registered yet
            // Use chars().take() for safe UTF-8 truncation (byte slicing can panic on multi-byte chars)
            let preview: String = content.chars().take(50).collect();
            tracing::warn!(
                "Received output for unknown task '{}': {}...",
                name,
                preview
            );
        }
    }

    /// Get elapsed time since start
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Mark execution as complete
    pub fn complete(&mut self, success: bool, error_message: Option<String>) {
        self.is_complete = true;
        self.success = success;
        self.error_message = error_message;
    }

    /// Extract the task path from a full task name.
    /// Task names follow the format: `task:project:path.parts`
    /// Returns the `path.parts` portion split by dots.
    #[must_use]
    fn parse_task_path(task_name: &str) -> Vec<&str> {
        // Format: task:project:path.parts
        // We want to extract "path.parts" and split by "."
        let parts: Vec<&str> = task_name.split(':').collect();
        if parts.len() >= 3 {
            // Get everything after the second colon and split by dots
            parts[2].split('.').collect()
        } else if parts.len() == 2 {
            // Fallback: just use the second part split by dots
            parts[1].split('.').collect()
        } else {
            // Fallback: use the whole name as a single path element
            vec![task_name]
        }
    }

    /// Build a hierarchical tree structure from task names.
    /// Returns a nested structure: `path -> (child_groups, tasks_at_this_level)`
    fn build_name_hierarchy(&self) -> HashMap<String, (Vec<String>, Vec<String>)> {
        let mut tree: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
        tree.insert(String::new(), (Vec::new(), Vec::new())); // Root

        for task_name in self.tasks.keys() {
            let path_parts = Self::parse_task_path(task_name);

            // Build intermediate groups (all parts EXCEPT the last one)
            // The last part is the task itself, not a group
            let mut current_path = String::new();
            let group_parts = if path_parts.len() > 1 {
                &path_parts[..path_parts.len() - 1]
            } else {
                &[] // No groups for single-part paths
            };

            for part in group_parts {
                let parent_path = current_path.clone();
                if current_path.is_empty() {
                    current_path = (*part).to_string();
                } else {
                    current_path = format!("{current_path}.{part}");
                }

                // Ensure this group path exists in the tree
                tree.entry(current_path.clone())
                    .or_insert_with(|| (Vec::new(), Vec::new()));

                // Add this as a child group of parent (if not already)
                let (groups, _) = tree
                    .entry(parent_path)
                    .or_insert_with(|| (Vec::new(), Vec::new()));
                if !groups.contains(&current_path) {
                    groups.push(current_path.clone());
                }
            }

            // Add the task to its parent group
            let parent_path = if path_parts.len() > 1 {
                path_parts[..path_parts.len() - 1].join(".")
            } else {
                String::new() // Root level
            };

            let (_, tasks) = tree
                .entry(parent_path)
                .or_insert_with(|| (Vec::new(), Vec::new()));
            if !tasks.contains(task_name) {
                tasks.push(task_name.clone());
            }
        }

        // Sort all children for consistent ordering
        for (groups, tasks) in tree.values_mut() {
            groups.sort();
            tasks.sort();
        }

        tree
    }

    /// Rebuild the flattened tree view based on current expansion state
    pub fn rebuild_flattened_tree(&mut self) {
        let tree = self.build_name_hierarchy();
        let mut flattened = Vec::new();

        // Add "All" node at the top
        let all_key = "::all::".to_string();
        let all_expanded = self.expanded_nodes.contains(&all_key);
        let has_tasks = !self.tasks.is_empty();

        flattened.push(TreeViewItem {
            node_type: TreeNodeType::All,
            display_name: "All".to_string(),
            depth: 0,
            is_expanded: all_expanded,
            has_children: has_tasks,
        });

        // If "All" is expanded, show the tree
        if all_expanded {
            // Get root level groups and tasks
            if let Some((root_groups, root_tasks)) = tree.get("") {
                // Use a stack for depth-first traversal
                // Stack items: (group_path, display_name, depth, is_group)
                let mut stack: Vec<(String, String, usize, bool)> = Vec::new();

                // Add root tasks (in reverse for correct order after pop)
                for task_name in root_tasks.iter().rev() {
                    let path_parts = Self::parse_task_path(task_name);
                    let display = path_parts
                        .last()
                        .map_or(task_name.clone(), |s| (*s).to_string());
                    stack.push((task_name.clone(), display, 1, false));
                }

                // Add root groups (in reverse for correct order after pop)
                for group_path in root_groups.iter().rev() {
                    let display = group_path
                        .split('.')
                        .next()
                        .unwrap_or(group_path)
                        .to_string();
                    stack.push((group_path.clone(), display, 1, true));
                }

                while let Some((path, display_name, depth, is_group)) = stack.pop() {
                    if is_group {
                        let group_key = format!("::group::{path}");
                        let is_expanded = self.expanded_nodes.contains(&group_key);
                        let empty_vec: Vec<String> = Vec::new();
                        let (child_groups, child_tasks) = tree
                            .get(&path)
                            .map_or((&empty_vec, &empty_vec), |(g, t)| (g, t));
                        let has_children = !child_groups.is_empty() || !child_tasks.is_empty();

                        flattened.push(TreeViewItem {
                            node_type: TreeNodeType::Group(path.clone()),
                            display_name,
                            depth,
                            is_expanded,
                            has_children,
                        });

                        if is_expanded {
                            // Add children (tasks first, then groups - in reverse)
                            for task_name in child_tasks.iter().rev() {
                                let path_parts = Self::parse_task_path(task_name);
                                let task_display = path_parts
                                    .last()
                                    .map_or(task_name.clone(), |s| (*s).to_string());
                                stack.push((task_name.clone(), task_display, depth + 1, false));
                            }
                            for child_path in child_groups.iter().rev() {
                                // Display name is just the last segment
                                let child_display = child_path
                                    .split('.')
                                    .next_back()
                                    .unwrap_or(child_path)
                                    .to_string();
                                stack.push((child_path.clone(), child_display, depth + 1, true));
                            }
                        }
                    } else {
                        // Task node
                        flattened.push(TreeViewItem {
                            node_type: TreeNodeType::Task(path.clone()),
                            display_name,
                            depth,
                            is_expanded: false,
                            has_children: false,
                        });
                    }
                }
            }
        }

        self.flattened_tree = flattened;

        // Ensure cursor position is valid
        if self.cursor_position >= self.flattened_tree.len() {
            self.cursor_position = self.flattened_tree.len().saturating_sub(1);
        }
    }

    /// Toggle expansion state of a tree node
    pub fn toggle_expansion(&mut self, node_key: &str) {
        if self.expanded_nodes.contains(node_key) {
            self.expanded_nodes.remove(node_key);
        } else {
            self.expanded_nodes.insert(node_key.to_string());
        }
        self.rebuild_flattened_tree();
    }

    /// Move cursor up in tree
    pub fn cursor_up(&mut self) {
        if self.cursor_position > 0 {
            self.cursor_position -= 1;
        }
    }

    /// Move cursor down in tree
    pub fn cursor_down(&mut self) {
        if self.cursor_position < self.flattened_tree.len().saturating_sub(1) {
            self.cursor_position += 1;
        }
    }

    /// Get currently highlighted node
    #[must_use]
    pub fn highlighted_node(&self) -> Option<&TreeViewItem> {
        self.flattened_tree.get(self.cursor_position)
    }

    /// Select current node for output filtering
    pub fn select_current_node(&mut self) {
        if let Some(node) = self.highlighted_node() {
            match &node.node_type {
                TreeNodeType::All => {
                    self.selected_task = None;
                    self.output_mode = OutputMode::All;
                }
                TreeNodeType::Task(name) => {
                    self.selected_task = Some(name.clone());
                    self.output_mode = OutputMode::Selected;
                }
                TreeNodeType::Group(path) => {
                    // For groups, we'll store the group path and filter in output
                    self.selected_task = Some(format!("::group::{path}"));
                    self.output_mode = OutputMode::Selected;
                }
            }
            self.output_scroll = 0;
        }
    }

    /// Return to "All" output mode
    pub fn show_all_output(&mut self) {
        self.selected_task = None;
        self.output_mode = OutputMode::All;
        self.output_scroll = 0;
    }

    /// Initialize tree with "All" expanded by default
    pub fn init_tree(&mut self) {
        // Expand "All" node by default for visibility
        self.expanded_nodes.insert("::all::".to_string());
        self.rebuild_flattened_tree();
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_symbol() {
        assert_eq!(TaskStatus::Pending.symbol(), "⏸");
        assert_eq!(TaskStatus::Running.symbol(), "▶");
        assert_eq!(TaskStatus::Completed.symbol(), "✓");
        assert_eq!(TaskStatus::Failed.symbol(), "✗");
        assert_eq!(TaskStatus::Skipped.symbol(), "⊘");
        assert_eq!(TaskStatus::Cached.symbol(), "⚡");
    }

    #[test]
    fn test_task_info_new() {
        let task = TaskInfo::new("test".to_string(), vec!["dep1".to_string()], 1);
        assert_eq!(task.name, "test");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.level, 1);
        assert!(task.start_time.is_none());
        assert!(task.duration_ms.is_none());
    }

    #[test]
    fn test_task_output_new() {
        let output = TaskOutput::new("test".to_string());
        assert_eq!(output.name, "test");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout_dirty);
        assert!(!output.stderr_dirty);
    }

    #[test]
    fn test_task_output_add() {
        let mut output = TaskOutput::new("test".to_string());
        output.add_stdout("line1".to_string());
        output.add_stderr("error1".to_string());

        assert_eq!(output.stdout.len(), 1);
        assert_eq!(output.stderr.len(), 1);
        assert!(output.stdout_dirty);
        assert!(output.stderr_dirty);

        output.clear_dirty();
        assert!(!output.stdout_dirty);
        assert!(!output.stderr_dirty);
    }

    #[test]
    fn test_tui_state_new() {
        let state = TuiState::new();
        assert!(state.tasks.is_empty());
        assert!(state.outputs.is_empty());
        assert!(state.running_tasks.is_empty());
        assert!(!state.is_complete);
        assert!(!state.success);
    }

    #[test]
    fn test_tui_state_add_task() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.outputs.len(), 1);
        assert!(state.tasks.contains_key("test"));
        assert!(state.outputs.contains_key("test"));
    }

    #[test]
    fn test_tui_state_update_status() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        state.update_task_status("test", TaskStatus::Running);
        assert_eq!(state.tasks.get("test").unwrap().status, TaskStatus::Running);
        assert_eq!(state.running_tasks.len(), 1);

        state.update_task_status("test", TaskStatus::Completed);
        assert_eq!(
            state.tasks.get("test").unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(state.running_tasks.len(), 0);
    }

    #[test]
    fn test_tui_state_parse_task_path() {
        // Full format: task:project:path.parts
        let path = TuiState::parse_task_path("task:cuenv:test.bdd");
        assert_eq!(path, vec!["test", "bdd"]);

        let path = TuiState::parse_task_path("task:cuenv:build");
        assert_eq!(path, vec!["build"]);

        // Two-part fallback
        let path = TuiState::parse_task_path("cuenv:test.unit");
        assert_eq!(path, vec!["test", "unit"]);
    }

    #[test]
    fn test_tui_state_name_hierarchy() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:test.bdd".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:build".to_string(), vec![], 0));

        let tree = state.build_name_hierarchy();

        // Root should have "test" group and "build" as a task (single-part names are tasks)
        let (root_groups, root_tasks) = tree.get("").unwrap();
        assert!(root_groups.contains(&"test".to_string()));
        // "build" is a single-part task, so it appears in tasks, not groups
        assert!(root_tasks.contains(&"task:proj:build".to_string()));

        // "test" group should have "test.bdd" and "test.unit" as children
        let (_, test_tasks) = tree.get("test").unwrap();
        assert_eq!(test_tasks.len(), 2);
    }

    #[test]
    fn test_tui_state_tree_navigation() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:test.bdd".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0));
        state.init_tree();

        // Should start at position 0 (the "All" node)
        assert_eq!(state.cursor_position, 0);

        // Verify first item is "All"
        let node = state.highlighted_node().unwrap();
        assert!(matches!(node.node_type, TreeNodeType::All));

        // Move down to first child
        state.cursor_down();
        assert_eq!(state.cursor_position, 1);

        // Move up
        state.cursor_up();
        assert_eq!(state.cursor_position, 0);

        // Should not go below 0
        state.cursor_up();
        assert_eq!(state.cursor_position, 0);
    }

    #[test]
    fn test_tui_state_output_mode() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:build".to_string(), vec![], 0));
        state.init_tree();

        // Start in All mode
        assert_eq!(state.output_mode, OutputMode::All);

        // Navigate to a task and select it
        state.cursor_down(); // Move to "build" group/task
        state.select_current_node();

        // Should be in selected mode now
        assert_eq!(state.output_mode, OutputMode::Selected);

        // Return to All mode
        state.show_all_output();
        assert_eq!(state.output_mode, OutputMode::All);
        assert!(state.selected_task.is_none());
    }

    #[test]
    fn test_tui_state_complete() {
        let mut state = TuiState::new();
        state.complete(true, None);

        assert!(state.is_complete);
        assert!(state.success);
        assert!(state.error_message.is_none());

        let mut state2 = TuiState::new();
        state2.complete(false, Some("error".to_string()));

        assert!(state2.is_complete);
        assert!(!state2.success);
        assert_eq!(state2.error_message, Some("error".to_string()));
    }

    #[test]
    fn test_task_output_bounded_buffer() {
        let mut output = TaskOutput::new("test".to_string());

        // Add more lines than MAX_OUTPUT_LINES
        for i in 0..MAX_OUTPUT_LINES + 100 {
            output.add_stdout(format!("stdout line {i}"));
            output.add_stderr(format!("stderr line {i}"));
        }

        // Buffers should be capped at MAX_OUTPUT_LINES
        assert_eq!(output.stdout.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.stderr.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.combined.len(), MAX_OUTPUT_LINES);

        // Oldest lines should be dropped, newest should remain
        assert_eq!(
            output.stdout.back().unwrap(),
            &format!("stdout line {}", MAX_OUTPUT_LINES + 99)
        );
        assert_eq!(
            output.stderr.back().unwrap(),
            &format!("stderr line {}", MAX_OUTPUT_LINES + 99)
        );
    }

    #[test]
    fn test_task_output_chronological_order() {
        let mut output = TaskOutput::new("test".to_string());

        // Add lines in a specific order
        output.add_stdout("first stdout".to_string());
        output.add_stderr("first stderr".to_string());
        output.add_stdout("second stdout".to_string());
        output.add_stderr("second stderr".to_string());

        // Combined output should preserve insertion order
        assert_eq!(output.combined.len(), 4);
        assert_eq!(output.combined[0].content, "first stdout");
        assert!(!output.combined[0].is_stderr);
        assert_eq!(output.combined[1].content, "first stderr");
        assert!(output.combined[1].is_stderr);
        assert_eq!(output.combined[2].content, "second stdout");
        assert!(!output.combined[2].is_stderr);
        assert_eq!(output.combined[3].content, "second stderr");
        assert!(output.combined[3].is_stderr);
    }
}
