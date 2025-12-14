//! DAG (Directed Acyclic Graph) visualization widget for task dependencies

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::collections::HashMap;

/// Status of a task in the DAG
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task has not started yet
    Pending,
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was skipped due to dependency failure
    Skipped,
    /// Task was cached (hit)
    Cached,
}

impl TaskStatus {
    /// Get the display symbol for this status
    pub fn symbol(&self) -> &str {
        match self {
            Self::Pending => "⏸",
            Self::Running => "▶",
            Self::Completed => "✓",
            Self::Failed => "✗",
            Self::Skipped => "⊘",
            Self::Cached => "⚡",
        }
    }

    /// Get the color for this status
    pub fn color(&self) -> Color {
        match self {
            Self::Pending => Color::DarkGray,
            Self::Running => Color::Blue,
            Self::Completed => Color::Green,
            Self::Failed => Color::Red,
            Self::Skipped => Color::Yellow,
            Self::Cached => Color::Cyan,
        }
    }

    /// Get the style for this status
    pub fn style(&self) -> Style {
        let base_style = Style::default().fg(self.color());
        match self {
            Self::Running => base_style.add_modifier(Modifier::BOLD),
            Self::Failed => base_style.add_modifier(Modifier::BOLD),
            _ => base_style,
        }
    }
}

/// A task node in the DAG visualization
#[derive(Debug, Clone)]
pub struct DagNode {
    /// Task name
    pub name: String,
    /// Current status
    pub status: TaskStatus,
    /// Dependency level (for layout)
    pub level: usize,
    /// Position within the level
    pub position: usize,
}

/// State for the DAG visualization
#[derive(Debug, Clone, Default)]
pub struct DagState {
    /// All nodes in the DAG
    pub nodes: Vec<DagNode>,
    /// Task name to node index mapping
    pub name_to_index: HashMap<String, usize>,
    /// Maximum level in the DAG (for layout calculation)
    pub max_level: usize,
}

impl DagState {
    /// Create a new DAG state
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update a node in the DAG
    pub fn add_node(&mut self, name: String, status: TaskStatus, level: usize, position: usize) {
        if let Some(&index) = self.name_to_index.get(&name) {
            // Update existing node
            if let Some(node) = self.nodes.get_mut(index) {
                node.status = status;
                node.level = level;
                node.position = position;
            }
        } else {
            // Add new node
            let index = self.nodes.len();
            self.nodes.push(DagNode {
                name: name.clone(),
                status,
                level,
                position,
            });
            self.name_to_index.insert(name, index);
        }
        self.max_level = self.max_level.max(level);
    }

    /// Update the status of a task
    pub fn update_status(&mut self, name: &str, status: TaskStatus) {
        if let Some(&index) = self.name_to_index.get(name) {
            if let Some(node) = self.nodes.get_mut(index) {
                node.status = status;
            }
        }
    }

    /// Get nodes grouped by level (for rendering)
    pub fn nodes_by_level(&self) -> Vec<Vec<&DagNode>> {
        let mut levels: Vec<Vec<&DagNode>> = vec![vec![]; self.max_level + 1];
        for node in &self.nodes {
            levels[node.level].push(node);
        }
        // Sort nodes within each level by position
        for level in &mut levels {
            level.sort_by_key(|n| n.position);
        }
        levels
    }
}

/// DAG visualization widget
pub struct DagWidget<'a> {
    /// The DAG state to render
    state: &'a DagState,
    /// Optional block to wrap the widget
    block: Option<Block<'a>>,
    /// Show level labels
    show_levels: bool,
}

impl<'a> DagWidget<'a> {
    /// Create a new DAG widget
    pub fn new(state: &'a DagState) -> Self {
        Self {
            state,
            block: None,
            show_levels: true,
        }
    }

    /// Set the block for this widget
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set whether to show level labels
    #[must_use]
    pub fn show_levels(mut self, show: bool) -> Self {
        self.show_levels = show;
        self
    }

    /// Render a single task node
    fn render_node(node: &DagNode, max_name_len: usize) -> Line<'static> {
        let status_symbol = node.status.symbol();
        let style = node.status.style();

        // Pad the name to align status symbols
        let padded_name = format!("{:width$}", node.name, width = max_name_len);

        Line::from(vec![
            Span::raw("  "),
            Span::styled(status_symbol, style),
            Span::raw(" "),
            Span::styled(padded_name, style),
        ])
    }
}

impl Widget for DagWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Apply block if present
        let inner = if let Some(block) = self.block {
            let inner = block.inner(area);
            block.render(area, buf);
            inner
        } else {
            area
        };

        // Group nodes by level
        let levels = self.state.nodes_by_level();

        if levels.is_empty() {
            return;
        }

        // Calculate maximum name length for alignment
        let max_name_len = self
            .state
            .nodes
            .iter()
            .map(|n| n.name.len())
            .max()
            .unwrap_or(10)
            .min(40); // Cap at 40 chars

        // Render each level
        let mut y = inner.top();
        for (level_idx, level_nodes) in levels.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }

            // Render level header if enabled
            if self.show_levels && !level_nodes.is_empty() {
                let level_label = format!("Level {level_idx}:");
                let level_line = Line::from(vec![Span::styled(
                    level_label,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]);
                buf.set_line(inner.left(), y, &level_line, inner.width);
                y += 1;
            }

            if y >= inner.bottom() {
                break;
            }

            // Render nodes in this level
            for node in level_nodes {
                if y >= inner.bottom() {
                    break;
                }
                let line = Self::render_node(node, max_name_len);
                buf.set_line(inner.left(), y, &line, inner.width);
                y += 1;
            }

            // Add blank line between levels
            y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_symbols() {
        assert_eq!(TaskStatus::Pending.symbol(), "⏸");
        assert_eq!(TaskStatus::Running.symbol(), "▶");
        assert_eq!(TaskStatus::Completed.symbol(), "✓");
        assert_eq!(TaskStatus::Failed.symbol(), "✗");
        assert_eq!(TaskStatus::Skipped.symbol(), "⊘");
        assert_eq!(TaskStatus::Cached.symbol(), "⚡");
    }

    #[test]
    fn test_task_status_colors() {
        assert_eq!(TaskStatus::Pending.color(), Color::DarkGray);
        assert_eq!(TaskStatus::Running.color(), Color::Blue);
        assert_eq!(TaskStatus::Completed.color(), Color::Green);
        assert_eq!(TaskStatus::Failed.color(), Color::Red);
        assert_eq!(TaskStatus::Skipped.color(), Color::Yellow);
        assert_eq!(TaskStatus::Cached.color(), Color::Cyan);
    }

    #[test]
    fn test_dag_state_add_node() {
        let mut state = DagState::new();
        state.add_node("task1".to_string(), TaskStatus::Pending, 0, 0);
        state.add_node("task2".to_string(), TaskStatus::Running, 1, 0);

        assert_eq!(state.nodes.len(), 2);
        assert_eq!(state.max_level, 1);
        assert!(state.name_to_index.contains_key("task1"));
        assert!(state.name_to_index.contains_key("task2"));
    }

    #[test]
    fn test_dag_state_update_status() {
        let mut state = DagState::new();
        state.add_node("task1".to_string(), TaskStatus::Pending, 0, 0);

        state.update_status("task1", TaskStatus::Running);

        let node = &state.nodes[0];
        assert_eq!(node.status, TaskStatus::Running);
    }

    #[test]
    fn test_dag_state_nodes_by_level() {
        let mut state = DagState::new();
        state.add_node("task1".to_string(), TaskStatus::Pending, 0, 0);
        state.add_node("task2".to_string(), TaskStatus::Pending, 0, 1);
        state.add_node("task3".to_string(), TaskStatus::Pending, 1, 0);

        let levels = state.nodes_by_level();

        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 2);
        assert_eq!(levels[1].len(), 1);
        assert_eq!(levels[0][0].name, "task1");
        assert_eq!(levels[0][1].name, "task2");
    }
}
