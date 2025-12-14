//! DAG visualization widget showing task dependency graph with status indicators

use crate::tui::state::{TaskInfo, TaskStatus, TuiState};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

/// Widget for displaying the task dependency DAG
pub struct DagWidget<'a> {
    state: &'a TuiState,
}

impl<'a> DagWidget<'a> {
    /// Create a new DAG widget
    #[must_use]
    pub fn new(state: &'a TuiState) -> Self {
        Self { state }
    }

    /// Render a task node with status indicator
    fn render_task_node(task: &TaskInfo) -> Span {
        let symbol = task.status.symbol();
        let color = task.status.color();
        let name = &task.name;

        // Show duration for completed/failed tasks
        let label = if let Some(duration_ms) = task.duration_ms {
            format!("{symbol} {name} ({duration_ms}ms)")
        } else if task.status == TaskStatus::Running {
            if let Some(elapsed_ms) = task.elapsed_ms() {
                format!("{symbol} {name} ({elapsed_ms}ms)")
            } else {
                format!("{symbol} {name}")
            }
        } else {
            format!("{symbol} {name}")
        };

        Span::styled(
            label,
            Style::default()
                .fg(color)
                .add_modifier(if task.status == TaskStatus::Running {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
    }

    /// Render a level of the DAG
    fn render_level(tasks: &[&TaskInfo]) -> Line {
        let mut spans = Vec::new();

        for (i, task) in tasks.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Self::render_task_node(task));
        }

        Line::from(spans)
    }
}

impl<'a> Widget for DagWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Task Graph ")
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        block.render(area, buf);

        // Get tasks grouped by level
        let levels = self.state.tasks_by_level();

        // Create lines for each level
        let mut lines = Vec::new();
        for (level_idx, level_tasks) in levels.iter().enumerate() {
            if level_tasks.is_empty() {
                continue;
            }

            // Add level header
            lines.push(Line::from(vec![Span::styled(
                format!("Level {level_idx}:"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]));

            // Add tasks in this level
            lines.push(Self::render_level(level_tasks));

            // Add spacing between levels (except after last)
            if level_idx < levels.len() - 1 {
                lines.push(Line::from(vec![Span::raw("↓")]));
            }
        }

        // If no tasks, show placeholder
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

    #[test]
    fn test_dag_widget_new() {
        let state = TuiState::new();
        let widget = DagWidget::new(&state);
        assert!(std::ptr::eq(widget.state, &state));
    }

    #[test]
    fn test_render_task_node() {
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        let span = DagWidget::render_task_node(&task);

        // Just verify it doesn't panic and returns a span
        assert!(!span.content.is_empty());
    }

    #[test]
    fn test_render_level() {
        let task1 = TaskInfo::new("task1".to_string(), vec![], 0);
        let task2 = TaskInfo::new("task2".to_string(), vec![], 0);
        let tasks = vec![&task1, &task2];

        let line = DagWidget::render_level(&tasks);
        assert!(!line.spans.is_empty());
    }
}
