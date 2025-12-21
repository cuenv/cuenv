//! Rich TUI for task execution with tree navigation and filtered output display

use super::state::{OutputMode, TaskInfo, TaskStatus, TuiState};
use super::widgets::{OutputPanelWidget, TaskTreeWidget};
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use cuenv_events::{CuenvEvent, EventCategory, EventReceiver, TaskEvent};
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
use tokio::sync::oneshot;

/// RAII guard that restores terminal state on drop.
///
/// This guard ensures the terminal is properly restored even if the TUI
/// exits unexpectedly (e.g., due to a panic). Errors during cleanup are
/// logged but cannot be propagated since Drop cannot return errors.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Attempt to restore terminal state. Log errors since Drop can't propagate them.
        // Users may need to run `reset` if these fail.
        if let Err(e) = disable_raw_mode() {
            tracing::warn!(error = %e, "Failed to disable raw mode");
        }
        if let Err(e) = execute!(io::stdout(), LeaveAlternateScreen) {
            tracing::warn!(error = %e, "Failed to leave alternate screen");
        }
    }
}

/// Rich TUI manager for task execution
pub struct RichTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: TuiState,
    _guard: TerminalGuard,
    event_rx: EventReceiver,
    quit_requested: bool,
    can_quit: bool,
    received_completion_event: bool,
    /// Oneshot channel to signal when the TUI event loop is ready.
    /// This prevents a race condition where task execution starts
    /// before the TUI is ready to receive events.
    ready_tx: Option<oneshot::Sender<()>>,
}

impl RichTui {
    /// Create a new rich TUI
    ///
    /// # Arguments
    /// * `event_rx` - Receiver for cuenv events
    /// * `ready_tx` - Oneshot sender to signal when the TUI event loop is ready.
    ///   Task execution should wait for this signal before starting.
    pub fn new(event_rx: EventReceiver, ready_tx: oneshot::Sender<()>) -> io::Result<Self> {
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
            ready_tx: Some(ready_tx),
        })
    }

    /// Initialize task graph from task information
    pub fn init_tasks(&mut self, tasks: Vec<TaskInfo>) {
        for task in tasks {
            self.state.add_task(task);
        }
        // Initialize the tree view with root tasks expanded
        self.state.init_tree();
    }

    /// Run the TUI event loop
    pub fn run(&mut self) -> io::Result<()> {
        // Signal that the TUI event loop is ready to receive events.
        // This must happen before the first poll to prevent a race condition
        // where task execution starts before we're listening for events.
        if let Some(ready_tx) = self.ready_tx.take() {
            // Ignore send error - receiver may have been dropped if task setup failed
            let _ = ready_tx.send(());
        }

        loop {
            // Render the UI
            self.render()?;

            // Check for quit conditions
            if self.quit_requested && self.can_quit {
                break;
            }

            // Handle events (non-blocking)
            if !self.handle_events()? {
                break;
            }
        }

        // Log diagnostic if we're exiting without having received a completion event.
        // This can happen if: the user force-quit (Ctrl+C), events were dropped,
        // or there's a bug in event delivery.
        if !self.received_completion_event {
            tracing::debug!(
                "TUI exited without receiving completion event (user may have quit early)"
            );
        }

        Ok(())
    }

    /// Handle events (keyboard and cuenv events)
    fn handle_events(&mut self) -> io::Result<bool> {
        // Non-blocking poll for keyboard events
        if event::poll(Duration::from_millis(50))?
            && let CrosstermEvent::Key(key) = event::read()?
        {
            match key.code {
                // Quit handling
                KeyCode::Char('q') => {
                    if self.state.is_complete {
                        return Ok(false); // Exit immediately if complete
                    }
                    self.quit_requested = true;
                    self.can_quit = self.state.is_complete;
                }
                KeyCode::Esc => {
                    // If in selected mode, return to all mode first
                    if self.state.output_mode == OutputMode::Selected {
                        self.state.show_all_output();
                    } else if self.state.is_complete {
                        return Ok(false);
                    } else {
                        self.quit_requested = true;
                        self.can_quit = self.state.is_complete;
                    }
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Force quit on Ctrl+C
                    return Ok(false);
                }

                // Tree navigation
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.cursor_up();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.cursor_down();
                }

                // Expand/collapse tree nodes
                KeyCode::Left | KeyCode::Char('h') => {
                    // Collapse current node
                    if let Some(node) = self.state.highlighted_node() {
                        let node_key = node.node_key();
                        if node.has_children && self.state.expanded_nodes.contains(&node_key) {
                            self.state.toggle_expansion(&node_key);
                        }
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    // Expand current node
                    if let Some(node) = self.state.highlighted_node() {
                        let node_key = node.node_key();
                        if node.has_children && !self.state.expanded_nodes.contains(&node_key) {
                            self.state.toggle_expansion(&node_key);
                        }
                    }
                }

                // Select node for filtered output
                KeyCode::Enter => {
                    self.state.select_current_node();
                }

                // Return to "All" output mode
                KeyCode::Char('a') => {
                    self.state.show_all_output();
                }

                // Output scrolling (when in selected mode)
                KeyCode::PageUp => {
                    if self.state.output_mode == OutputMode::Selected {
                        self.state.output_scroll = self.state.output_scroll.saturating_sub(10);
                    }
                }
                KeyCode::PageDown => {
                    if self.state.output_mode == OutputMode::Selected {
                        self.state.output_scroll += 10;
                    }
                }

                _ => {}
            }
        }

        // Non-blocking drain of ALL available cuenv events
        // Important: Use `while let` to process ALL pending events, not just one.
        // Events can accumulate between render cycles (50ms), especially when
        // multiple tasks emit events in quick succession.
        while let Some(event) = self.event_rx.try_recv() {
            self.handle_cuenv_event(event);
        }
        // Note: We don't need to detect channel closure separately since
        // we emit a completion event before the channel closes (in task.rs)

        Ok(true)
    }

    /// Handle a cuenv event
    fn handle_cuenv_event(&mut self, event: CuenvEvent) {
        match event.category {
            EventCategory::Task(task_event) => self.handle_task_event(task_event),
            EventCategory::Command(cmd_event) => {
                use cuenv_events::CommandEvent;
                match cmd_event {
                    CommandEvent::Completed {
                        success, command, ..
                    } => {
                        self.received_completion_event = true;
                        let error_msg = if success {
                            None
                        } else {
                            // Provide context that task execution failed
                            // Detailed error info is shown in task output panes
                            Some(format!(
                                "Command '{command}' failed - see task output for details"
                            ))
                        };
                        self.state.complete(success, error_msg);
                        self.can_quit = true;
                    }
                    // Other command events are not displayed in TUI
                    CommandEvent::Started { .. } | CommandEvent::Progress { .. } => {}
                }
            }
            // These event categories are not relevant for the TUI display
            EventCategory::Ci(_)
            | EventCategory::Interactive(_)
            | EventCategory::System(_)
            | EventCategory::Output(_) => {}
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
            // These events don't require status updates in the TUI
            TaskEvent::CacheMiss { .. }
            | TaskEvent::GroupStarted { .. }
            | TaskEvent::GroupCompleted { .. } => {}
        }
    }

    /// Render the TUI
    fn render(&mut self) -> io::Result<()> {
        // Extract references to avoid borrow checker issues in the closure
        let state = &self.state;
        let quit_requested = self.quit_requested;

        self.terminal.draw(|f| {
            let size = f.area();

            // Create 3-panel layout: Header, Main Content, Status Bar
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Header (elapsed time)
                    Constraint::Min(10),   // Main content area
                    Constraint::Length(3), // Status Bar
                ])
                .split(size);

            // Render header
            Self::render_header_static(state, f, main_chunks[0]);

            // Create 2-panel horizontal split for main content
            let content_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30), // Task tree (left)
                    Constraint::Percentage(70), // Output panel (right)
                ])
                .split(main_chunks[1]);

            // Render task tree widget (left panel)
            let tree_widget = TaskTreeWidget::new(state);
            f.render_widget(tree_widget, content_chunks[0]);

            // Render output panel widget (right panel)
            let output_widget = OutputPanelWidget::new(state);
            f.render_widget(output_widget, content_chunks[1]);

            // Render status bar
            Self::render_status_bar_static(state, quit_requested, f, main_chunks[2]);
        })?;

        Ok(())
    }

    /// Render header with elapsed time (static version for use in closures)
    fn render_header_static(state: &TuiState, f: &mut ratatui::Frame, area: Rect) {
        let elapsed_ms = state.elapsed_ms();
        let elapsed_secs = elapsed_ms / 1000;
        let mins = elapsed_secs / 60;
        let secs = elapsed_secs % 60;

        // Determine title prefix and color based on completion state
        let (title_prefix, color) = match (state.is_complete, state.success) {
            (true, true) => ("Task Execution Complete", Color::Green),
            (true, false) => ("Task Execution Failed", Color::Red),
            (false, _) => ("Task Execution", Color::Cyan),
        };
        let title = format!(" {title_prefix} ({mins}:{secs:02}) ");

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(color));

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Show task counts
        let total = state.tasks.len();
        let completed = state
            .tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Cached))
            .count();
        let failed = state
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Failed)
            .count();
        let running = state.running_tasks.len();

        let info = format!(
            "Total: {total} | Running: {running} | Completed: {completed} | Failed: {failed}"
        );

        let paragraph = Paragraph::new(vec![Line::from(vec![Span::raw(info)])]);
        f.render_widget(paragraph, inner);
    }

    /// Render status bar (static version for use in closures)
    fn render_status_bar_static(
        state: &TuiState,
        quit_requested: bool,
        f: &mut ratatui::Frame,
        area: Rect,
    ) {
        let help_text = if state.is_complete {
            "Press 'q' to quit"
        } else if quit_requested {
            "Waiting for tasks... (Ctrl+C to force)"
        } else if state.output_mode == OutputMode::Selected {
            "Esc/a: All | ↑↓/jk: Navigate | PgUp/PgDn: Scroll | q: Quit"
        } else {
            "↑↓/jk: Navigate | ←→/hl: Collapse/Expand | Enter: Select | a: All | q: Quit"
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
