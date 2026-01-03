//! Output panel widget showing task output with filtering support

use crate::tui::state::{OutputMode, TuiState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

/// Widget for displaying task output
pub struct OutputPanelWidget<'a> {
    state: &'a TuiState,
}

impl<'a> OutputPanelWidget<'a> {
    /// Create a new output panel widget
    #[must_use]
    pub const fn new(state: &'a TuiState) -> Self {
        Self { state }
    }

    /// Render a task header separator
    fn render_task_header(task_name: &str, status_symbol: &str, color: Color) -> Line<'static> {
        // Extract display name from full task name
        let display_name = Self::extract_display_name(task_name);
        let header_text = format!("─── {status_symbol} {display_name} ");
        let padding = "─".repeat(40);
        Line::from(vec![
            Span::styled(
                header_text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(padding, Style::default().fg(Color::DarkGray)),
        ])
    }

    /// Extract display name from full task name (e.g., "task:cuenv:test.bdd" -> "test.bdd")
    fn extract_display_name(task_name: &str) -> &str {
        let parts: Vec<&str> = task_name.split(':').collect();
        if parts.len() >= 3 {
            parts[2]
        } else if parts.len() == 2 {
            parts[1]
        } else {
            task_name
        }
    }

    /// Parse task path for matching against groups
    fn parse_task_path(task_name: &str) -> String {
        let parts: Vec<&str> = task_name.split(':').collect();
        if parts.len() >= 3 {
            parts[2].to_string()
        } else if parts.len() == 2 {
            parts[1].to_string()
        } else {
            task_name.to_string()
        }
    }

    /// Check if a task belongs to a group
    fn task_matches_filter(task_name: &str, filter: &str) -> bool {
        if filter.starts_with("::group::") {
            let group_path = filter.strip_prefix("::group::").unwrap_or("");
            let task_path = Self::parse_task_path(task_name);
            task_path.starts_with(group_path)
        } else {
            // Direct task match
            task_name == filter
        }
    }

    /// Get tasks to display based on current selection
    fn get_visible_tasks(&self) -> Vec<&str> {
        match &self.state.selected_task {
            None => {
                // All mode - show all tasks
                self.state.tasks.keys().map(String::as_str).collect()
            }
            Some(filter) => {
                // Filter tasks
                self.state
                    .tasks
                    .keys()
                    .filter(|name| Self::task_matches_filter(name, filter))
                    .map(String::as_str)
                    .collect()
            }
        }
    }
}

impl Widget for OutputPanelWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Generate title based on selection
        let title = match &self.state.selected_task {
            None => " Output: All Tasks ".to_string(),
            Some(filter) if filter.starts_with("::group::") => {
                let group = filter.strip_prefix("::group::").unwrap_or("?");
                format!(" Output: {group} ")
            }
            Some(task) => {
                let display = Self::extract_display_name(task);
                format!(" Output: {display} ")
            }
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line> = Vec::new();

        // Get filtered tasks
        let visible_tasks = self.get_visible_tasks();

        // Sort tasks for consistent display
        let mut sorted_tasks: Vec<&str> = visible_tasks;
        sorted_tasks.sort_unstable();

        for task_name in sorted_tasks {
            if let Some(output) = self.state.outputs.get(task_name) {
                if output.combined.is_empty() {
                    continue;
                }

                let task_info = self.state.tasks.get(task_name);
                let (symbol, color) = task_info.map_or(("?", Color::DarkGray), |t| {
                    (t.status.symbol(), t.status.color())
                });

                lines.push(Self::render_task_header(task_name, symbol, color));

                for output_line in &output.combined {
                    if output_line.is_stderr {
                        lines.push(Line::from(vec![
                            Span::styled("! ", Style::default().fg(Color::Red)),
                            Span::raw(output_line.content.clone()),
                        ]));
                    } else {
                        lines.push(Line::from(vec![Span::raw(output_line.content.clone())]));
                    }
                }

                lines.push(Line::from("")); // Spacing between tasks
            }
        }

        // Handle empty state
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "No output yet...",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]));
        }

        // Apply scroll offset and render
        let visible_height = inner.height as usize;
        let total_lines = lines.len();

        // Auto-scroll to bottom for "All" mode, use manual scroll for "Selected" mode
        let effective_scroll = match self.state.output_mode {
            OutputMode::All => total_lines.saturating_sub(visible_height),
            OutputMode::Selected => self
                .state
                .output_scroll
                .min(total_lines.saturating_sub(visible_height)),
        };

        let visible_lines: Vec<Line> = lines
            .into_iter()
            .skip(effective_scroll)
            .take(visible_height)
            .collect();

        let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
        paragraph.render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_panel_widget_new() {
        let state = TuiState::new();
        let widget = OutputPanelWidget::new(&state);
        assert!(std::ptr::eq(widget.state, &raw const state));
    }

    #[test]
    fn test_render_task_header() {
        let line = OutputPanelWidget::render_task_header("test_task", "✓", Color::Green);
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn test_extract_display_name_full_format() {
        // task:project:name format
        assert_eq!(
            OutputPanelWidget::extract_display_name("task:cuenv:test.bdd"),
            "test.bdd"
        );
    }

    #[test]
    fn test_extract_display_name_two_parts() {
        // project:name format
        assert_eq!(
            OutputPanelWidget::extract_display_name("cuenv:build"),
            "build"
        );
    }

    #[test]
    fn test_extract_display_name_simple() {
        // Simple name
        assert_eq!(OutputPanelWidget::extract_display_name("build"), "build");
    }

    #[test]
    fn test_parse_task_path_full_format() {
        assert_eq!(
            OutputPanelWidget::parse_task_path("task:cuenv:test.unit"),
            "test.unit"
        );
    }

    #[test]
    fn test_parse_task_path_two_parts() {
        assert_eq!(OutputPanelWidget::parse_task_path("cuenv:build"), "build");
    }

    #[test]
    fn test_parse_task_path_simple() {
        assert_eq!(OutputPanelWidget::parse_task_path("build"), "build");
    }

    #[test]
    fn test_task_matches_filter_exact() {
        assert!(OutputPanelWidget::task_matches_filter(
            "task:cuenv:build",
            "task:cuenv:build"
        ));
        assert!(!OutputPanelWidget::task_matches_filter(
            "task:cuenv:build",
            "task:cuenv:test"
        ));
    }

    #[test]
    fn test_task_matches_filter_group() {
        // Group filter should match tasks that start with the group path
        assert!(OutputPanelWidget::task_matches_filter(
            "task:cuenv:test.unit",
            "::group::test"
        ));
        assert!(OutputPanelWidget::task_matches_filter(
            "task:cuenv:test.integration",
            "::group::test"
        ));
        assert!(!OutputPanelWidget::task_matches_filter(
            "task:cuenv:build",
            "::group::test"
        ));
    }

    #[test]
    fn test_task_matches_filter_empty_group() {
        // Empty group filter
        assert!(OutputPanelWidget::task_matches_filter(
            "task:cuenv:test",
            "::group::"
        ));
    }

    #[test]
    fn test_get_visible_tasks_all() {
        use crate::tui::state::TaskInfo;

        let mut state = TuiState::new();
        state.tasks.insert(
            "task:proj:build".to_string(),
            TaskInfo::new("task:proj:build".to_string(), vec![], 0),
        );
        state.tasks.insert(
            "task:proj:test".to_string(),
            TaskInfo::new("task:proj:test".to_string(), vec![], 0),
        );

        let widget = OutputPanelWidget::new(&state);
        let visible = widget.get_visible_tasks();

        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn test_get_visible_tasks_filtered() {
        use crate::tui::state::TaskInfo;

        let mut state = TuiState::new();
        state.tasks.insert(
            "task:proj:build".to_string(),
            TaskInfo::new("task:proj:build".to_string(), vec![], 0),
        );
        state.tasks.insert(
            "task:proj:test.unit".to_string(),
            TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0),
        );
        state.tasks.insert(
            "task:proj:test.integration".to_string(),
            TaskInfo::new("task:proj:test.integration".to_string(), vec![], 0),
        );
        state.selected_task = Some("::group::test".to_string());

        let widget = OutputPanelWidget::new(&state);
        let visible = widget.get_visible_tasks();

        // Should only show test.unit and test.integration, not build
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|t| t.contains("test")));
    }

    #[test]
    fn test_get_visible_tasks_specific_task() {
        use crate::tui::state::TaskInfo;

        let mut state = TuiState::new();
        state.tasks.insert(
            "task:proj:build".to_string(),
            TaskInfo::new("task:proj:build".to_string(), vec![], 0),
        );
        state.tasks.insert(
            "task:proj:test".to_string(),
            TaskInfo::new("task:proj:test".to_string(), vec![], 0),
        );
        state.selected_task = Some("task:proj:build".to_string());

        let widget = OutputPanelWidget::new(&state);
        let visible = widget.get_visible_tasks();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0], "task:proj:build");
    }

    #[test]
    fn test_widget_render_empty() {
        let state = TuiState::new();
        let widget = OutputPanelWidget::new(&state);

        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
        widget.render(Rect::new(0, 0, 60, 10), &mut buf);

        // Should render without panicking
    }

    #[test]
    fn test_render_task_header_format() {
        let line = OutputPanelWidget::render_task_header("task:cuenv:build", "✓", Color::Green);

        // Should have multiple spans
        assert!(line.spans.len() >= 2);
        // First span should contain the task name
        let first_span_text = line.spans[0].content.to_string();
        assert!(first_span_text.contains("build"));
    }
}
