//! Split-screen task output panes widget

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::collections::{HashMap, VecDeque};

use super::dag::TaskStatus;

/// Output buffer for a single task
#[derive(Debug, Clone)]
pub struct TaskOutputBuffer {
    /// Task name
    pub name: String,
    /// Current status
    pub status: TaskStatus,
    /// Output lines (limited by max_lines)
    pub output: VecDeque<String>,
    /// Maximum number of lines to keep
    pub max_lines: usize,
    /// Scroll offset (for future scrolling support)
    pub scroll_offset: usize,
}

impl TaskOutputBuffer {
    /// Create a new output buffer
    pub fn new(name: String, max_lines: usize) -> Self {
        Self {
            name,
            status: TaskStatus::Pending,
            output: VecDeque::new(),
            max_lines,
            scroll_offset: 0,
        }
    }

    /// Add a line to the output buffer
    pub fn push_line(&mut self, line: String) {
        self.output.push_back(line);
        // Keep only the last max_lines
        while self.output.len() > self.max_lines {
            self.output.pop_front();
        }
    }

    /// Clear the output buffer
    pub fn clear(&mut self) {
        self.output.clear();
    }

    /// Get the last N lines for display
    pub fn last_lines(&self, n: usize) -> Vec<&str> {
        self.output
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(String::as_str)
            .collect()
    }
}

/// State for task panes widget
#[derive(Debug, Clone, Default)]
pub struct TaskPanesState {
    /// All task output buffers
    pub buffers: HashMap<String, TaskOutputBuffer>,
    /// Maximum lines per buffer
    pub max_lines_per_buffer: usize,
    /// Active tasks to display (in order)
    pub active_tasks: Vec<String>,
    /// Maximum number of panes to show simultaneously
    pub max_panes: usize,
}

impl TaskPanesState {
    /// Create a new task panes state
    pub fn new(max_lines_per_buffer: usize, max_panes: usize) -> Self {
        Self {
            buffers: HashMap::new(),
            max_lines_per_buffer,
            active_tasks: Vec::new(),
            max_panes,
        }
    }

    /// Add or get a task buffer
    pub fn get_or_create_buffer(&mut self, name: &str) -> &mut TaskOutputBuffer {
        self.buffers.entry(name.to_string()).or_insert_with(|| {
            TaskOutputBuffer::new(name.to_string(), self.max_lines_per_buffer)
        })
    }

    /// Update task status
    pub fn update_status(&mut self, name: &str, status: TaskStatus) {
        if let Some(buffer) = self.buffers.get_mut(name) {
            buffer.status = status;
        }
    }

    /// Add output line to a task
    pub fn push_output(&mut self, name: &str, line: String) {
        let buffer = self.get_or_create_buffer(name);
        buffer.push_line(line);
    }

    /// Mark a task as active (should be displayed)
    pub fn mark_active(&mut self, name: &str) {
        if !self.active_tasks.contains(&name.to_string()) {
            self.active_tasks.push(name.to_string());
        }
    }

    /// Mark a task as inactive (remove from display)
    pub fn mark_inactive(&mut self, name: &str) {
        self.active_tasks.retain(|t| t != name);
    }

    /// Get tasks to display (limited by max_panes)
    pub fn tasks_to_display(&self) -> Vec<&str> {
        self.active_tasks
            .iter()
            .rev()
            .take(self.max_panes)
            .rev()
            .map(String::as_str)
            .collect()
    }
}

/// Task panes widget for split-screen output
pub struct TaskPanesWidget<'a> {
    /// The state to render
    state: &'a TaskPanesState,
    /// Layout direction
    direction: Direction,
}

impl<'a> TaskPanesWidget<'a> {
    /// Create a new task panes widget
    pub fn new(state: &'a TaskPanesState) -> Self {
        Self {
            state,
            direction: Direction::Horizontal,
        }
    }

    /// Set the layout direction
    #[must_use]
    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Render a single task pane
    fn render_pane(buffer: &TaskOutputBuffer, area: Rect, buf: &mut Buffer) {
        let status = buffer.status;
        let border_color = status.color();
        let border_style = Style::default().fg(border_color);

        // Create block with colored border
        let title = format!(" {} {} ", status.symbol(), buffer.name);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(
                title,
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        block.render(area, buf);

        // Render output lines
        let available_height = inner.height as usize;
        let lines_to_show = buffer.last_lines(available_height);

        let text_lines: Vec<Line> = lines_to_show
            .iter()
            .map(|line| Line::from(Span::raw(*line)))
            .collect();

        let paragraph = Paragraph::new(text_lines)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false });

        paragraph.render(inner, buf);
    }
}

impl Widget for TaskPanesWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let tasks_to_display = self.state.tasks_to_display();

        if tasks_to_display.is_empty() {
            return;
        }

        // Calculate constraints for each pane
        let pane_count = tasks_to_display.len();
        let constraints: Vec<Constraint> = vec![Constraint::Ratio(1, pane_count as u32); pane_count];

        // Create layout
        let chunks = Layout::default()
            .direction(self.direction)
            .constraints(constraints)
            .split(area);

        // Render each task pane
        for (i, task_name) in tasks_to_display.iter().enumerate() {
            if let Some(buffer) = self.state.buffers.get(*task_name) {
                if let Some(chunk) = chunks.get(i) {
                    Self::render_pane(buffer, *chunk, buf);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_output_buffer_new() {
        let buffer = TaskOutputBuffer::new("test".to_string(), 100);
        assert_eq!(buffer.name, "test");
        assert_eq!(buffer.status, TaskStatus::Pending);
        assert_eq!(buffer.max_lines, 100);
        assert!(buffer.output.is_empty());
    }

    #[test]
    fn test_task_output_buffer_push_line() {
        let mut buffer = TaskOutputBuffer::new("test".to_string(), 3);
        buffer.push_line("line1".to_string());
        buffer.push_line("line2".to_string());
        buffer.push_line("line3".to_string());
        buffer.push_line("line4".to_string());

        // Should only keep last 3 lines
        assert_eq!(buffer.output.len(), 3);
        assert_eq!(buffer.output[0], "line2");
        assert_eq!(buffer.output[1], "line3");
        assert_eq!(buffer.output[2], "line4");
    }

    #[test]
    fn test_task_output_buffer_last_lines() {
        let mut buffer = TaskOutputBuffer::new("test".to_string(), 100);
        buffer.push_line("line1".to_string());
        buffer.push_line("line2".to_string());
        buffer.push_line("line3".to_string());

        let last = buffer.last_lines(2);
        assert_eq!(last.len(), 2);
        assert_eq!(last[0], "line2");
        assert_eq!(last[1], "line3");
    }

    #[test]
    fn test_task_panes_state_new() {
        let state = TaskPanesState::new(100, 8);
        assert_eq!(state.max_lines_per_buffer, 100);
        assert_eq!(state.max_panes, 8);
        assert!(state.buffers.is_empty());
        assert!(state.active_tasks.is_empty());
    }

    #[test]
    fn test_task_panes_state_get_or_create_buffer() {
        let mut state = TaskPanesState::new(100, 8);
        let buffer = state.get_or_create_buffer("task1");
        buffer.push_line("test".to_string());

        assert_eq!(state.buffers.len(), 1);
        assert!(state.buffers.contains_key("task1"));
    }

    #[test]
    fn test_task_panes_state_mark_active() {
        let mut state = TaskPanesState::new(100, 8);
        state.mark_active("task1");
        state.mark_active("task2");
        state.mark_active("task1"); // Duplicate should not be added

        assert_eq!(state.active_tasks.len(), 2);
    }

    #[test]
    fn test_task_panes_state_mark_inactive() {
        let mut state = TaskPanesState::new(100, 8);
        state.mark_active("task1");
        state.mark_active("task2");
        state.mark_inactive("task1");

        assert_eq!(state.active_tasks.len(), 1);
        assert_eq!(state.active_tasks[0], "task2");
    }

    #[test]
    fn test_task_panes_state_tasks_to_display() {
        let mut state = TaskPanesState::new(100, 3);
        state.mark_active("task1");
        state.mark_active("task2");
        state.mark_active("task3");
        state.mark_active("task4");

        let tasks = state.tasks_to_display();
        // Should only show last 3 tasks due to max_panes = 3
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0], "task2");
        assert_eq!(tasks[1], "task3");
        assert_eq!(tasks[2], "task4");
    }
}
