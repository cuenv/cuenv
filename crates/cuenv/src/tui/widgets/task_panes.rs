//! Split-screen task output panes showing parallel task execution

use crate::tui::state::{TaskOutput, TuiState};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

/// Widget for displaying split-screen task output panes
pub struct TaskPanesWidget<'a> {
    state: &'a TuiState,
}

impl<'a> TaskPanesWidget<'a> {
    /// Create a new task panes widget
    #[must_use]
    pub fn new(state: &'a TuiState) -> Self {
        Self { state }
    }

    /// Render a single task pane
    fn render_task_pane(
        task_name: &str,
        output: &TaskOutput,
        area: Rect,
        buf: &mut Buffer,
        task_status: &crate::tui::state::TaskStatus,
    ) {
        // Color-code border based on task status
        let border_color = task_status.color();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} {} ", task_status.symbol(), task_name))
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        // Use combined output which preserves chronological order
        let mut lines = Vec::new();

        // Show last N lines that fit in the pane
        let max_lines = inner.height.saturating_sub(1) as usize;

        // Skip older lines if we have more than max_lines
        let skip = output.combined.len().saturating_sub(max_lines);

        for output_line in output.combined.iter().skip(skip) {
            if output_line.is_stderr {
                lines.push(Line::from(vec![
                    Span::styled("! ", Style::default().fg(Color::Red)),
                    Span::raw(&output_line.content),
                ]));
            } else {
                lines.push(Line::from(vec![Span::raw(&output_line.content)]));
            }
        }

        // If no output yet, show placeholder
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "Waiting for output...",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]));
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        paragraph.render(inner, buf);
    }
}

impl<'a> Widget for TaskPanesWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let running_tasks = self.state.visible_running_tasks();

        if running_tasks.is_empty() {
            // No running tasks, show placeholder
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Task Output ")
                .border_style(Style::default().fg(Color::Cyan));

            let inner = block.inner(area);
            block.render(area, buf);

            let placeholder = Paragraph::new(vec![Line::from(vec![Span::styled(
                "No tasks running",
                Style::default().fg(Color::DarkGray),
            )])]);
            placeholder.render(inner, buf);
            return;
        }

        // Create equal-sized panes for each running task
        let pane_count = running_tasks.len();
        let constraints: Vec<Constraint> = (0..pane_count)
            .map(|_| Constraint::Percentage(100 / pane_count as u16))
            .collect();

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);

        // Render each task pane
        for (i, task_name) in running_tasks.iter().enumerate() {
            if let (Some(output), Some(task)) = (
                self.state.outputs.get(*task_name),
                self.state.tasks.get(*task_name),
            ) {
                Self::render_task_pane(task_name, output, chunks[i], buf, &task.status);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_panes_widget_new() {
        let state = TuiState::new();
        let widget = TaskPanesWidget::new(&state);
        assert!(std::ptr::eq(widget.state, &state));
    }
}
