//! Rich TUI for task execution with tree navigation and filtered output display

use super::state::{OutputMode, TaskInfo, TaskStatus, TuiState};
use super::widgets::{OutputPanelWidget, TaskTreeWidget};
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyModifiers},
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use cuenv_events::{CuenvEvent, EventCategory, EventReceiver};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Target render cadence — coalesces bursts of events into a single redraw
/// so a chatty task can't melt the TUI into a redraw storm. ~30 FPS.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// RAII guard that restores terminal state on drop.
///
/// Constructed *before* `enable_raw_mode` runs so its `Drop` is on the
/// stack the moment raw mode is enabled — any panic between enabling raw
/// mode and finishing `RichTui::new` still restores the terminal cleanly.
/// Errors during cleanup are logged but cannot be propagated.
struct TerminalGuard {
    raw_enabled: bool,
}

impl TerminalGuard {
    const fn new() -> Self {
        Self { raw_enabled: false }
    }

    fn enable_raw(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        self.raw_enabled = true;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.raw_enabled
            && let Err(e) = disable_raw_mode()
        {
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
    /// Number of rows the inline viewport reserves at the bottom of the
    /// terminal. Sized so the user keeps a few rows of scrollback visible
    /// during execution, then the TUI buffer scrolls into permanent
    /// scrollback when the run finishes.
    const INLINE_RESERVED_ROWS: u16 = 4;
    /// Minimum inline viewport height — guards against tiny terminals.
    const INLINE_MIN_ROWS: u16 = 18;

    /// Create a new rich TUI.
    ///
    /// Uses ratatui's inline viewport so the run's output stays in the
    /// terminal's scrollback when the TUI exits. The `f` keybinding hides
    /// the task tree and devotes the full viewport to a single task's
    /// output panel — it does not enter the alternate screen.
    ///
    /// # Arguments
    /// * `event_rx` - Receiver for cuenv events
    /// * `ready_tx` - Oneshot sender to signal when the TUI event loop is ready.
    ///   Task execution should wait for this signal before starting.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal initialization fails.
    pub fn new(event_rx: EventReceiver, ready_tx: oneshot::Sender<()>) -> io::Result<Self> {
        let mut guard = TerminalGuard::new();
        guard.enable_raw()?;
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);

        let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 30));
        let inline_height = term_rows
            .saturating_sub(Self::INLINE_RESERVED_ROWS)
            .max(Self::INLINE_MIN_ROWS);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(inline_height),
            },
        )?;

        Ok(Self {
            terminal,
            state: TuiState::new(),
            _guard: guard,
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

    /// Run the TUI event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn run(&mut self) -> io::Result<()> {
        // Signal that the TUI event loop is ready to receive events.
        // This must happen before the first poll to prevent a race condition
        // where task execution starts before we're listening for events.
        if let Some(ready_tx) = self.ready_tx.take() {
            // Ignore send error - receiver may have been dropped if task setup failed
            let _ = ready_tx.send(());
        }

        loop {
            // Coalesce events from this frame's budget into a single render.
            let frame_start = Instant::now();
            self.render()?;

            if self.quit_requested && self.can_quit {
                break;
            }

            // Drain pending cuenv events and key events up to the frame deadline.
            if !self.drain_until_deadline(frame_start + FRAME_INTERVAL)? {
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

    /// Drain key + cuenv events until `deadline`, then return.
    ///
    /// Coalesces bursts of cuenv events without redrawing in between, giving
    /// a ~30 FPS render cadence regardless of how chatty tasks are. Returns
    /// `Ok(false)` when the user requested a force quit and the caller
    /// should exit the run loop.
    fn drain_until_deadline(&mut self, deadline: Instant) -> io::Result<bool> {
        loop {
            // Drain every pending cuenv event before consulting key events,
            // so a flood of cuenv events can't starve user input.
            while let Some(event) = self.event_rx.try_recv() {
                self.handle_cuenv_event(&event);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(true);
            }

            // Cap the per-call timeout so we can re-check cuenv events every
            // ~5ms even when the user isn't pressing keys.
            let poll_timeout = remaining.min(Duration::from_millis(5));
            if event::poll(poll_timeout)?
                && let CrosstermEvent::Key(key) = event::read()?
                && !self.handle_key(key)
            {
                return Ok(false);
            }
        }
    }

    /// Handle a single key event. Returns `false` when the caller should
    /// exit the run loop (force-quit).
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            // Quit handling
            KeyCode::Char('q') => {
                if self.state.is_complete {
                    return false; // Exit immediately if complete
                }
                self.quit_requested = true;
                self.can_quit = self.state.is_complete;
            }
            KeyCode::Esc => {
                // Exit focus mode first, then selected mode, before quitting.
                if self.state.focused_task.is_some() {
                    self.state.clear_focus();
                } else if self.state.output_mode == OutputMode::Selected {
                    self.state.show_all_output();
                } else if self.state.is_complete {
                    return false;
                } else {
                    self.quit_requested = true;
                    self.can_quit = self.state.is_complete;
                }
            }
            KeyCode::Char('f') => {
                self.state.toggle_focus();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Force quit on Ctrl+C
                return false;
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
                if let Some(node) = self.state.highlighted_node() {
                    let node_key = node.node_key();
                    if node.has_children && self.state.expanded_nodes.contains(&node_key) {
                        self.state.toggle_expansion(&node_key);
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
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
            KeyCode::PageUp if self.state.output_mode == OutputMode::Selected => {
                self.state.output_scroll = self.state.output_scroll.saturating_sub(10);
            }
            KeyCode::PageDown if self.state.output_mode == OutputMode::Selected => {
                self.state.output_scroll += 10;
            }

            _ => {}
        }

        true
    }

    /// Handle a cuenv event.
    ///
    /// Task events are funnelled through [`TuiState::apply_event`] — the
    /// canonical deterministic apply path — so live runs and replays
    /// arrive at identical activity state from the same event stream.
    /// Command/completion handling stays local because it owns RichTui's
    /// `received_completion_event` / `can_quit` flags.
    fn handle_cuenv_event(&mut self, event: &CuenvEvent) {
        match &event.category {
            EventCategory::Task(_) => {
                self.state.apply_event(event);
            }
            EventCategory::Command(cmd_event) => {
                use cuenv_events::CommandEvent;
                match cmd_event {
                    CommandEvent::Completed {
                        success, command, ..
                    } => {
                        self.received_completion_event = true;
                        let error_msg = if *success {
                            None
                        } else {
                            Some(format!(
                                "Command '{command}' failed - see task output for details"
                            ))
                        };
                        self.state.complete(*success, error_msg);
                        self.can_quit = true;
                    }
                    CommandEvent::Started { .. } | CommandEvent::Progress { .. } => {}
                }
            }
            EventCategory::Service(_)
            | EventCategory::Ci(_)
            | EventCategory::Interactive(_)
            | EventCategory::System(_)
            | EventCategory::Output(_) => {}
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

            // When a task is focused, the tree is hidden so the output
            // panel gets every available column. Otherwise we keep the
            // usual 30/70 tree-output split.
            if state.focused_task.is_some() {
                let output_widget = OutputPanelWidget::new(state);
                f.render_widget(output_widget, main_chunks[1]);
            } else {
                let content_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(30), // Task tree (left)
                        Constraint::Percentage(70), // Output panel (right)
                    ])
                    .split(main_chunks[1]);

                let tree_widget = TaskTreeWidget::new(state);
                f.render_widget(tree_widget, content_chunks[0]);

                let output_widget = OutputPanelWidget::new(state);
                f.render_widget(output_widget, content_chunks[1]);
            }

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
        } else if state.focused_task.is_some() {
            "f/Esc: Exit focus | PgUp/PgDn: Scroll | q: Quit"
        } else if state.output_mode == OutputMode::Selected {
            "Esc/a: All | f: Focus task | ↑↓/jk: Navigate | PgUp/PgDn: Scroll | q: Quit"
        } else {
            "↑↓/jk: Navigate | ←→/hl: Collapse/Expand | Enter: Select | f: Focus | a: All | q: Quit"
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
        // Just verify TerminalGuard can be created and dropped without
        // leaving the terminal in a stuck state.
        let _guard = TerminalGuard::new();
    }
}
