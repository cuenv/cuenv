//! Rich TUI for task execution with DAG visualization and parallel output panes

use super::state::{TaskInfo, TaskStatus, TuiState};
use super::widgets::{DagWidget, TaskPanesWidget};
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use cuenv_events::{CuenvEvent, EventCategory, Stream, TaskEvent};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::io::{self, Stdout};
use std::time::Duration;
use tokio::sync::mpsc;

/// RAII guard that restores terminal state on drop
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Rich TUI manager for task execution
pub struct RichTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: TuiState,
    _guard: TerminalGuard,
    event_rx: mpsc::UnboundedReceiver<CuenvEvent>,
    quit_requested: bool,
    can_quit: bool,
    received_completion_event: bool,
}

impl RichTui {
    /// Create a new rich TUI
    pub fn new(event_rx: mpsc::UnboundedReceiver<CuenvEvent>) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self {
            terminal,
            state: TuiState::new(),
            _guard: TerminalGuard,
            event_rx,
            quit_requested: false,
            can_quit: false,
            received_completion_event: false,
        })
    }

    /// Initialize task graph from task information
    pub fn init_tasks(&mut self, tasks: Vec<TaskInfo>) {
        for task in tasks {
            self.state.add_task(task);
        }
    }

    /// Run the TUI event loop
    pub async fn run(&mut self) -> io::Result<()> {
        loop {
            // Render the UI
            self.render()?;

            // Check for quit conditions
            if self.quit_requested && self.can_quit {
                break;
            }

            // Handle events (non-blocking)
            if !self.handle_events().await? {
                break;
            }
        }

        Ok(())
    }

    /// Handle events (keyboard and cuenv events)
    async fn handle_events(&mut self) -> io::Result<bool> {
        // Non-blocking poll for keyboard events
        if event::poll(Duration::from_millis(50))? {
            if let CrosstermEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if self.state.is_complete {
                            return Ok(false); // Exit immediately if complete
                        }
                        self.quit_requested = true;
                        self.can_quit = self.state.is_complete;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Force quit on Ctrl+C
                        return Ok(false);
                    }
                    _ => {}
                }
            }
        }

        // Non-blocking check for cuenv events
        match self.event_rx.try_recv() {
            Ok(event) => {
                self.handle_cuenv_event(event);
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                // No events available, continue
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                // Event channel closed - only mark success if we received a completion event
                if !self.state.is_complete {
                    if self.received_completion_event {
                        self.state.complete(true, None);
                    } else {
                        // Channel closed unexpectedly without completion event
                        self.state.complete(false, Some("Event channel closed unexpectedly".to_string()));
                    }
                }
                self.can_quit = true;
            }
        }

        Ok(true)
    }

    /// Handle a cuenv event
    fn handle_cuenv_event(&mut self, event: CuenvEvent) {
        match event.category {
            EventCategory::Task(task_event) => self.handle_task_event(task_event),
            EventCategory::Command(cmd_event) => {
                use cuenv_events::CommandEvent;
                match cmd_event {
                    CommandEvent::Completed { success, .. } => {
                        self.received_completion_event = true;
                        self.state.complete(success, None);
                        self.can_quit = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Handle a task event
    fn handle_task_event(&mut self, event: TaskEvent) {
        match event {
            TaskEvent::Started { name, .. } => {
                self.state.update_task_status(&name, TaskStatus::Running);
            }
            TaskEvent::CacheHit { name, .. } => {
                self.state.update_task_status(&name, TaskStatus::Cached);
            }
            TaskEvent::Output {
                name,
                stream,
                content,
            } => {
                let stream_str = match stream {
                    cuenv_events::Stream::Stdout => "stdout",
                    cuenv_events::Stream::Stderr => "stderr",
                };
                self.state.add_task_output(&name, stream_str, content);
            }
            TaskEvent::Completed {
                name,
                success,
                exit_code,
                ..
            } => {
                let status = if success {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };
                self.state.update_task_status(&name, status);

                // Update exit code
                if let Some(task) = self.state.tasks.get_mut(&name) {
                    task.exit_code = exit_code;
                }
            }
            _ => {}
        }
    }

    /// Render the TUI
    fn render(&mut self) -> io::Result<()> {
        self.terminal.draw(|f| {
            let size = f.area();

            // Create 4-panel layout: Header, DAG, Task Panes, Status Bar
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),      // Header (elapsed time)
                    Constraint::Percentage(25), // DAG (25%)
                    Constraint::Percentage(65), // Task Panes (65%)
                    Constraint::Length(3),      // Status Bar
                ])
                .split(size);

            // Render header
            self.render_header(f, chunks[0]);

            // Render DAG widget
            let dag_widget = DagWidget::new(&self.state);
            f.render_widget(dag_widget, chunks[1]);

            // Render task panes widget
            let panes_widget = TaskPanesWidget::new(&self.state);
            f.render_widget(panes_widget, chunks[2]);

            // Render status bar
            self.render_status_bar(f, chunks[3]);
        })?;

        Ok(())
    }

    /// Render header with elapsed time
    fn render_header(&self, f: &mut ratatui::Frame, area: Rect) {
        let elapsed_ms = self.state.elapsed_ms();
        let elapsed_secs = elapsed_ms / 1000;
        let mins = elapsed_secs / 60;
        let secs = elapsed_secs % 60;

        let title = if self.state.is_complete {
            if self.state.success {
                format!(" Task Execution Complete ({mins}:{secs:02}) ")
            } else {
                format!(" Task Execution Failed ({mins}:{secs:02}) ")
            }
        } else {
            format!(" Task Execution ({mins}:{secs:02}) ")
        };

        let color = if self.state.is_complete {
            if self.state.success {
                Color::Green
            } else {
                Color::Red
            }
        } else {
            Color::Cyan
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(color));

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Show task counts
        let total = self.state.tasks.len();
        let completed = self
            .state
            .tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Cached))
            .count();
        let failed = self
            .state
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Failed)
            .count();
        let running = self.state.running_tasks.len();

        let info = format!(
            "Total: {} | Running: {} | Completed: {} | Failed: {}",
            total, running, completed, failed
        );

        let paragraph = Paragraph::new(vec![Line::from(vec![Span::raw(info)])]);
        f.render_widget(paragraph, inner);
    }

    /// Render status bar
    fn render_status_bar(&self, f: &mut ratatui::Frame, area: Rect) {
        let help_text = if self.state.is_complete {
            "Press 'q' or Esc to quit"
        } else if self.quit_requested {
            "Waiting for tasks to complete... (Ctrl+C to force quit)"
        } else {
            "Press 'q' or Esc to quit when done | Ctrl+C to abort"
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let paragraph = Paragraph::new(vec![Line::from(vec![Span::styled(
            help_text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )])]);
        f.render_widget(paragraph, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_guard_drop() {
        // Just verify TerminalGuard can be created and dropped
        let _guard = TerminalGuard;
    }
}
