//! Interactive task picker using ratatui
//!
//! Provides a fuzzy-filtering picker UI for selecting tasks to execute.

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::io;

/// A selectable task entry
#[derive(Debug, Clone)]
pub struct SelectableTask {
    /// Full task name (e.g., "bun.install")
    pub name: String,
    /// Task description
    pub description: Option<String>,
}

/// Result of the picker interaction
#[derive(Debug)]
pub enum PickerResult {
    /// User selected a task
    Selected(String),
    /// User cancelled (Esc or Ctrl+C)
    Cancelled,
}

/// Interactive task picker state
struct TaskPicker {
    /// All available tasks
    tasks: Vec<SelectableTask>,
    /// Current filter string
    filter: String,
    /// Filtered task indices
    filtered_indices: Vec<usize>,
    /// List selection state
    list_state: ListState,
}

impl TaskPicker {
    fn new(tasks: Vec<SelectableTask>) -> Self {
        let filtered_indices: Vec<usize> = (0..tasks.len()).collect();
        let mut list_state = ListState::default();
        if !filtered_indices.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            tasks,
            filter: String::new(),
            filtered_indices,
            list_state,
        }
    }

    /// Update the filtered list based on current filter
    fn update_filter(&mut self) {
        let filter_lower = self.filter.to_lowercase();

        self.filtered_indices = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
                if filter_lower.is_empty() {
                    return true;
                }
                // Match anywhere in name or description
                let name_match = task.name.to_lowercase().contains(&filter_lower);
                let desc_match = task
                    .description
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&filter_lower));
                name_match || desc_match
            })
            .map(|(i, _)| i)
            .collect();

        // Reset selection to first item
        if self.filtered_indices.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Move selection up
    fn select_previous(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }

        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move selection down
    fn select_next(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }

        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.filtered_indices.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Get the currently selected task name
    fn selected_task(&self) -> Option<String> {
        self.list_state.selected().and_then(|i| {
            self.filtered_indices
                .get(i)
                .map(|&idx| self.tasks[idx].name.clone())
        })
    }
}

/// Run the interactive task picker
///
/// Returns the selected task name or None if cancelled.
pub fn run_picker(tasks: Vec<SelectableTask>) -> io::Result<PickerResult> {
    // Don't show picker if no tasks
    if tasks.is_empty() {
        return Ok(PickerResult::Cancelled);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create picker state
    let mut picker = TaskPicker::new(tasks);

    // Run event loop
    let result = run_event_loop(&mut terminal, &mut picker);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

/// Main event loop
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    picker: &mut TaskPicker,
) -> io::Result<PickerResult> {
    loop {
        // Draw UI
        terminal.draw(|f| draw_ui(f, picker))?;

        // Handle events
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Only handle key press events (not release)
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    // Cancel
                    KeyCode::Esc => return Ok(PickerResult::Cancelled),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(PickerResult::Cancelled);
                    }

                    // Select
                    KeyCode::Enter => {
                        if let Some(task) = picker.selected_task() {
                            return Ok(PickerResult::Selected(task));
                        }
                    }

                    // Navigation
                    KeyCode::Up | KeyCode::Char('k') => picker.select_previous(),
                    KeyCode::Down | KeyCode::Char('j') => picker.select_next(),

                    // Filter input
                    KeyCode::Char(c) => {
                        picker.filter.push(c);
                        picker.update_filter();
                    }
                    KeyCode::Backspace => {
                        picker.filter.pop();
                        picker.update_filter();
                    }

                    _ => {}
                }
            }
        }
    }
}

/// Draw the picker UI
fn draw_ui(f: &mut Frame, picker: &mut TaskPicker) {
    let area = f.area();

    // Layout: header, filter input, task list, help footer
    let chunks = Layout::vertical([
        Constraint::Length(3), // Filter input
        Constraint::Min(5),    // Task list
        Constraint::Length(1), // Help footer
    ])
    .split(area);

    // Filter input
    draw_filter_input(f, picker, chunks[0]);

    // Task list
    draw_task_list(f, picker, chunks[1]);

    // Help footer
    draw_help_footer(f, chunks[2]);
}

/// Draw the filter input box
fn draw_filter_input(f: &mut Frame, picker: &TaskPicker, area: Rect) {
    let filter_text = if picker.filter.is_empty() {
        "Type to filter...".to_string()
    } else {
        picker.filter.clone()
    };

    let style = if picker.filter.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let input = Paragraph::new(filter_text).style(style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Select a task ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );

    f.render_widget(input, area);

    // Show cursor at end of filter
    let cursor_x = area.x + 1 + picker.filter.len() as u16;
    let cursor_y = area.y + 1;
    f.set_cursor_position((cursor_x.min(area.x + area.width - 2), cursor_y));
}

/// Draw the task list
fn draw_task_list(f: &mut Frame, picker: &mut TaskPicker, area: Rect) {
    let items: Vec<ListItem> = picker
        .filtered_indices
        .iter()
        .map(|&idx| {
            let task = &picker.tasks[idx];

            let mut spans = vec![
                Span::styled("● ", Style::default().fg(Color::Cyan)),
                Span::styled(&task.name, Style::default().fg(Color::Cyan)),
            ];

            if let Some(desc) = &task.description {
                // Add padding and description
                let padding = 30_usize.saturating_sub(task.name.len());
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(desc, Style::default().fg(Color::DarkGray)));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut picker.list_state);
}

/// Draw the help footer
fn draw_help_footer(f: &mut Frame, area: Rect) {
    let help = Line::from(vec![
        Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
        Span::raw(" navigate │ "),
        Span::styled("enter", Style::default().fg(Color::Cyan)),
        Span::raw(" select │ "),
        Span::styled("esc", Style::default().fg(Color::Cyan)),
        Span::raw(" cancel │ type to filter"),
    ]);

    let footer = Paragraph::new(help)
        .style(Style::default().fg(Color::DarkGray))
        .centered();

    f.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_picker_filter() {
        let tasks = vec![
            SelectableTask {
                name: "build".to_string(),
                description: Some("Build the project".to_string()),
            },
            SelectableTask {
                name: "test".to_string(),
                description: Some("Run tests".to_string()),
            },
            SelectableTask {
                name: "bun.install".to_string(),
                description: Some("Install bun dependencies".to_string()),
            },
        ];

        let mut picker = TaskPicker::new(tasks);
        assert_eq!(picker.filtered_indices.len(), 3);

        picker.filter = "bun".to_string();
        picker.update_filter();
        assert_eq!(picker.filtered_indices.len(), 1);
        assert_eq!(picker.filtered_indices[0], 2);
    }

    #[test]
    fn test_task_picker_navigation() {
        let tasks = vec![
            SelectableTask {
                name: "a".to_string(),
                description: None,
            },
            SelectableTask {
                name: "b".to_string(),
                description: None,
            },
            SelectableTask {
                name: "c".to_string(),
                description: None,
            },
        ];

        let mut picker = TaskPicker::new(tasks);
        assert_eq!(picker.list_state.selected(), Some(0));

        picker.select_next();
        assert_eq!(picker.list_state.selected(), Some(1));

        picker.select_next();
        assert_eq!(picker.list_state.selected(), Some(2));

        // Wrap around
        picker.select_next();
        assert_eq!(picker.list_state.selected(), Some(0));

        picker.select_previous();
        assert_eq!(picker.list_state.selected(), Some(2));
    }
}
