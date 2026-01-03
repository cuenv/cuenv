//! Task tree widget showing hierarchical task names as an expandable tree

use crate::tui::state::{TaskStatus, TreeNodeType, TreeViewItem, TuiState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

/// Widget for displaying the task tree with expand/collapse functionality
pub struct TaskTreeWidget<'a> {
    state: &'a TuiState,
}

impl<'a> TaskTreeWidget<'a> {
    /// Create a new task tree widget
    #[must_use]
    pub const fn new(state: &'a TuiState) -> Self {
        Self { state }
    }

    /// Render tree indentation and expand/collapse indicator
    fn render_tree_prefix(item: &TreeViewItem) -> String {
        let indent = "  ".repeat(item.depth);
        let icon = if item.has_children {
            if item.is_expanded { "▼" } else { "▶" }
        } else {
            "─"
        };
        format!("{indent}{icon} ")
    }

    /// Get aggregated status for a group of tasks
    fn get_group_status(&self, group_path: &str) -> (TaskStatus, usize, usize) {
        let mut running = 0;
        let mut completed = 0;
        let mut failed = 0;
        let mut total = 0;

        for (name, info) in &self.state.tasks {
            let task_path = Self::parse_task_path(name).join(".");
            if task_path.starts_with(group_path) || group_path.is_empty() {
                total += 1;
                match info.status {
                    TaskStatus::Running => running += 1,
                    TaskStatus::Completed | TaskStatus::Cached => completed += 1,
                    TaskStatus::Failed => failed += 1,
                    _ => {}
                }
            }
        }

        let status = if failed > 0 {
            TaskStatus::Failed
        } else if running > 0 {
            TaskStatus::Running
        } else if completed == total && total > 0 {
            TaskStatus::Completed
        } else {
            TaskStatus::Pending
        };

        (status, completed, total)
    }

    /// Parse task path (duplicated from state for widget use)
    fn parse_task_path(task_name: &str) -> Vec<&str> {
        let parts: Vec<&str> = task_name.split(':').collect();
        if parts.len() >= 3 {
            parts[2].split('.').collect()
        } else if parts.len() == 2 {
            parts[1].split('.').collect()
        } else {
            vec![task_name]
        }
    }

    /// Render a single tree item
    fn render_tree_item(
        &self,
        item: &TreeViewItem,
        is_cursor: bool,
        is_selected: bool,
    ) -> Line<'static> {
        let prefix = Self::render_tree_prefix(item);

        let (status_symbol, status_color, suffix) = match &item.node_type {
            TreeNodeType::All => {
                let (status, completed, total) = self.get_group_status("");
                let suffix = format!(" ({completed}/{total})");
                (status.symbol(), status.color(), suffix)
            }
            TreeNodeType::Group(path) => {
                let (status, completed, total) = self.get_group_status(path);
                let suffix = format!(" ({completed}/{total})");
                (status.symbol(), status.color(), suffix)
            }
            TreeNodeType::Task(name) => {
                let task_info = self.state.tasks.get(name);
                let (symbol, color) = task_info.map_or(("?", Color::DarkGray), |t| {
                    (t.status.symbol(), t.status.color())
                });

                // Build duration string for tasks
                let duration = task_info.and_then(|t| {
                    t.duration_ms.or_else(|| {
                        if t.status == TaskStatus::Running {
                            t.elapsed_ms()
                        } else {
                            None
                        }
                    })
                });
                let suffix = duration.map(|ms| format!(" ({ms}ms)")).unwrap_or_default();

                (symbol, color, suffix)
            }
        };

        // Determine base style
        let mut name_style = Style::default().fg(status_color);

        // Bold for running items
        if matches!(&item.node_type, TreeNodeType::Task(name) if self.state.tasks.get(name).map(|t| t.status) == Some(TaskStatus::Running))
        {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }

        // Cursor highlighting (background)
        if is_cursor {
            name_style = name_style.bg(Color::DarkGray);
        }

        // Selection indicator (reversed)
        if is_selected {
            name_style = name_style.add_modifier(Modifier::REVERSED);
        }

        Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(status_symbol.to_string(), Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(
                format!("{}{}", item.display_name.clone(), suffix),
                name_style,
            ),
        ])
    }
}

impl Widget for TaskTreeWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Tasks ")
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        block.render(area, buf);

        // Calculate visible range for scrolling
        let visible_height = inner.height as usize;
        let cursor = self.state.cursor_position;

        // Calculate scroll offset to keep cursor visible
        let scroll_offset = if cursor >= visible_height {
            cursor - visible_height + 1
        } else {
            0
        };

        let mut lines = Vec::new();
        for (idx, item) in self.state.flattened_tree.iter().enumerate() {
            if idx < scroll_offset {
                continue;
            }
            if lines.len() >= visible_height {
                break;
            }

            let is_cursor = idx == cursor;
            // Check if this node is selected based on node type
            let is_selected = match &item.node_type {
                TreeNodeType::All => {
                    self.state.selected_task.is_none()
                        && self.state.output_mode == crate::tui::state::OutputMode::All
                }
                TreeNodeType::Task(name) => self.state.selected_task.as_deref() == Some(name),
                TreeNodeType::Group(path) => {
                    self.state.selected_task.as_deref() == Some(&format!("::group::{path}"))
                }
            };
            lines.push(self.render_tree_item(item, is_cursor, is_selected));
        }

        // Handle empty state
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "No tasks",
                Style::default().fg(Color::DarkGray),
            )]));
        }

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TaskInfo;

    #[test]
    fn test_task_tree_widget_new() {
        let state = TuiState::new();
        let widget = TaskTreeWidget::new(&state);
        assert!(std::ptr::eq(widget.state, &raw const state));
    }

    #[test]
    fn test_render_tree_prefix_collapsed() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Group("test".to_string()),
            display_name: "test".to_string(),
            depth: 0,
            is_expanded: false,
            has_children: true,
        };
        let prefix = TaskTreeWidget::render_tree_prefix(&item);
        assert_eq!(prefix, "▶ ");
    }

    #[test]
    fn test_render_tree_prefix_expanded() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Group("test".to_string()),
            display_name: "test".to_string(),
            depth: 0,
            is_expanded: true,
            has_children: true,
        };
        let prefix = TaskTreeWidget::render_tree_prefix(&item);
        assert_eq!(prefix, "▼ ");
    }

    #[test]
    fn test_render_tree_prefix_leaf() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Task("test".to_string()),
            display_name: "test".to_string(),
            depth: 1,
            is_expanded: false,
            has_children: false,
        };
        let prefix = TaskTreeWidget::render_tree_prefix(&item);
        assert_eq!(prefix, "  ─ ");
    }

    #[test]
    fn test_render_tree_prefix_nested() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Group("test.nested".to_string()),
            display_name: "nested".to_string(),
            depth: 2,
            is_expanded: true,
            has_children: true,
        };
        let prefix = TaskTreeWidget::render_tree_prefix(&item);
        assert_eq!(prefix, "    ▼ ");
    }

    #[test]
    fn test_parse_task_path() {
        // Full format: task:project:path.parts
        assert_eq!(
            TaskTreeWidget::parse_task_path("task:cuenv:test.bdd"),
            vec!["test", "bdd"]
        );
        assert_eq!(
            TaskTreeWidget::parse_task_path("task:cuenv:build"),
            vec!["build"]
        );
        // Two-part format
        assert_eq!(
            TaskTreeWidget::parse_task_path("cuenv:test.unit"),
            vec!["test", "unit"]
        );
        // Single name fallback
        assert_eq!(TaskTreeWidget::parse_task_path("simple"), vec!["simple"]);
    }

    #[test]
    fn test_get_group_status_empty() {
        let state = TuiState::new();
        let widget = TaskTreeWidget::new(&state);
        let (status, completed, total) = widget.get_group_status("test");
        assert_eq!(status, TaskStatus::Pending);
        assert_eq!(completed, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_get_group_status_with_tasks() {
        let mut state = TuiState::new();
        let mut info = TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0);
        info.status = TaskStatus::Completed;
        state.tasks.insert("task:proj:test.unit".to_string(), info);

        let mut info2 = TaskInfo::new("task:proj:test.integration".to_string(), vec![], 0);
        info2.status = TaskStatus::Running;
        state
            .tasks
            .insert("task:proj:test.integration".to_string(), info2);

        let widget = TaskTreeWidget::new(&state);
        let (status, completed, total) = widget.get_group_status("test");

        assert_eq!(status, TaskStatus::Running);
        assert_eq!(completed, 1);
        assert_eq!(total, 2);
    }

    #[test]
    fn test_get_group_status_all_completed() {
        let mut state = TuiState::new();

        let mut info = TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0);
        info.status = TaskStatus::Completed;
        state.tasks.insert("task:proj:test.unit".to_string(), info);

        let mut info2 = TaskInfo::new("task:proj:test.integration".to_string(), vec![], 0);
        info2.status = TaskStatus::Cached;
        state
            .tasks
            .insert("task:proj:test.integration".to_string(), info2);

        let widget = TaskTreeWidget::new(&state);
        let (status, completed, total) = widget.get_group_status("test");

        assert_eq!(status, TaskStatus::Completed);
        assert_eq!(completed, 2);
        assert_eq!(total, 2);
    }

    #[test]
    fn test_get_group_status_with_failure() {
        let mut state = TuiState::new();

        let mut info = TaskInfo::new("task:proj:build".to_string(), vec![], 0);
        info.status = TaskStatus::Completed;
        state.tasks.insert("task:proj:build".to_string(), info);

        let mut info2 = TaskInfo::new("task:proj:build.release".to_string(), vec![], 0);
        info2.status = TaskStatus::Failed;
        state
            .tasks
            .insert("task:proj:build.release".to_string(), info2);

        let widget = TaskTreeWidget::new(&state);
        let (status, _completed, _total) = widget.get_group_status("build");

        assert_eq!(status, TaskStatus::Failed);
    }

    #[test]
    fn test_get_group_status_empty_path() {
        let mut state = TuiState::new();

        let mut info = TaskInfo::new("task:proj:test".to_string(), vec![], 0);
        info.status = TaskStatus::Completed;
        state.tasks.insert("task:proj:test".to_string(), info);

        let widget = TaskTreeWidget::new(&state);
        let (_status, completed, total) = widget.get_group_status("");

        assert_eq!(completed, 1);
        assert_eq!(total, 1);
    }

    #[test]
    fn test_tree_view_item_clone() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Task("test".to_string()),
            display_name: "test".to_string(),
            depth: 0,
            is_expanded: false,
            has_children: false,
        };
        let cloned = item.clone();
        assert_eq!(cloned.display_name, item.display_name);
        assert_eq!(cloned.depth, item.depth);
    }

    #[test]
    fn test_tree_node_type_debug() {
        let all = TreeNodeType::All;
        let group = TreeNodeType::Group("test".to_string());
        let task = TreeNodeType::Task("test".to_string());

        assert!(format!("{all:?}").contains("All"));
        assert!(format!("{group:?}").contains("Group"));
        assert!(format!("{task:?}").contains("Task"));
    }

    #[test]
    fn test_render_tree_prefix_deep_nesting() {
        let item = TreeViewItem {
            node_type: TreeNodeType::Task("test".to_string()),
            display_name: "deep".to_string(),
            depth: 5,
            is_expanded: false,
            has_children: false,
        };
        let prefix = TaskTreeWidget::render_tree_prefix(&item);
        // 5 * 2 spaces = 10 spaces, then "─ "
        assert_eq!(prefix, "          ─ ");
    }

    #[test]
    fn test_parse_task_path_with_many_dots() {
        let path = TaskTreeWidget::parse_task_path("task:proj:a.b.c.d");
        assert_eq!(path, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_widget_render_empty_state() {
        let state = TuiState::new();
        let widget = TaskTreeWidget::new(&state);

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        widget.render(Rect::new(0, 0, 40, 10), &mut buf);

        // The widget should render without panicking
        // and should show "No tasks" when empty
    }

    #[test]
    fn test_render_tree_item_all_type() {
        let mut state = TuiState::new();
        state.flattened_tree.push(TreeViewItem {
            node_type: TreeNodeType::All,
            display_name: "All Tasks".to_string(),
            depth: 0,
            is_expanded: true,
            has_children: true,
        });

        let widget = TaskTreeWidget::new(&state);
        let item = &state.flattened_tree[0];
        let line = widget.render_tree_item(item, false, false);

        // Should not panic and should produce a Line
        assert!(!line.spans.is_empty());
    }
}
