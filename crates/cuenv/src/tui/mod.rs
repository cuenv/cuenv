pub mod rich;
pub mod state;
pub mod widgets;

use crate::coordinator::client::CoordinatorClient;
use crate::events::Event;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyModifiers};
use crossterm::style::Color as CrosstermColor;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{
    ExecutableCommand,
    cursor::{Hide, MoveTo, Show},
    style::{Print, ResetColor, SetForegroundColor},
};
use cuenv_events::CuenvEvent;
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph},
};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};
use tracing::{Level, event};

/// An inline terminal user interface that renders within the current terminal session.
///
/// This TUI renders progress and output information in a fixed region of the terminal,
/// allowing for interactive display without taking over the entire screen.
#[allow(dead_code)]
pub struct InlineTui {
    /// The ratatui terminal instance for rendering widgets.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The starting line position in the terminal where the TUI begins.
    start_line: u16,
    /// The height in lines reserved for the TUI display area.
    height: u16,
    /// Timestamp of the last render, used for rate limiting.
    last_render: Instant,
}

/// Represents the current state of the TUI display.
///
/// This struct holds all the information needed to render the TUI,
/// including command progress, messages, and output.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TuiState {
    /// The name of the currently executing command, if any.
    pub command: Option<String>,
    /// Progress value between 0.0 and 1.0 indicating completion percentage.
    pub progress: f32,
    /// A status message to display to the user.
    pub message: String,
    /// Accumulated output lines from the command execution.
    pub output: Vec<String>,
    /// Whether the command has finished executing.
    pub is_complete: bool,
    /// The success status of the command, `None` if still running.
    pub success: Option<bool>,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            command: None,
            progress: 0.0,
            message: String::new(),
            output: Vec::new(),
            is_complete: false,
            success: None,
        }
    }
}

#[allow(dead_code)]
impl InlineTui {
    /// Creates a new inline TUI instance.
    ///
    /// Initializes the terminal backend and determines the current cursor position
    /// to use as the starting line for the TUI display area.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal initialization fails or if the cursor
    /// position cannot be determined.
    pub fn new() -> io::Result<Self> {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        let (_, start_line) = crossterm::cursor::position()?;
        let height = 6; // Reserve space for inline TUI

        Ok(Self {
            terminal,
            start_line,
            height,
            last_render: Instant::now(),
        })
    }

    /// Shows the inline TUI by reserving screen space and hiding the cursor.
    ///
    /// Moves the cursor to the starting position, reserves vertical space
    /// for the TUI display, and hides the cursor for cleaner rendering.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal cursor operations fail.
    pub fn show(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();

        // Move cursor to starting position and reserve space
        stdout.execute(MoveTo(0, self.start_line))?;
        for _ in 0..self.height {
            stdout.execute(Print("\n"))?;
        }
        stdout.execute(MoveTo(0, self.start_line))?;
        stdout.execute(Hide)?;

        event!(
            Level::DEBUG,
            "Inline TUI initialized at line {}",
            self.start_line
        );
        Ok(())
    }

    /// Hides the inline TUI and restores the cursor.
    ///
    /// Shows the cursor again and moves it below the TUI display area
    /// so subsequent output appears after the TUI region.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal cursor operations fail.
    pub fn hide(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(Show)?;
        stdout.execute(MoveTo(0, self.start_line + self.height))?;
        event!(Level::DEBUG, "Inline TUI hidden");
        Ok(())
    }

    /// Renders the TUI with the given state.
    ///
    /// Draws the progress gauge and output sections based on the current state.
    /// Renders are rate-limited to 50ms intervals to avoid flickering.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal drawing operations fail.
    pub fn render(&mut self, state: &TuiState) -> io::Result<()> {
        // Rate limit renders to avoid flicker
        if self.last_render.elapsed() < Duration::from_millis(50) {
            return Ok(());
        }

        self.terminal.draw(|f| {
            let area = f.area();

            // Create layout with progress and output sections
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Progress section
                    Constraint::Min(3),    // Output section
                ])
                .split(area);

            // Render progress section
            if let Some(command) = &state.command {
                let progress_block = Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {command} "));

                let progress_gauge = Gauge::default()
                    .block(progress_block)
                    .gauge_style(Style::default().fg(if state.is_complete {
                        if state.success.unwrap_or(false) {
                            ratatui::style::Color::Green
                        } else {
                            ratatui::style::Color::Red
                        }
                    } else {
                        ratatui::style::Color::Blue
                    }))
                    .ratio(f64::from(state.progress))
                    .label(state.message.clone());

                f.render_widget(progress_gauge, chunks[0]);
            }

            // Render output section
            if !state.output.is_empty() {
                let output_lines: Vec<Line> = state
                    .output
                    .iter()
                    .map(|line| Line::from(vec![Span::raw(line.clone())]))
                    .collect();

                let output_paragraph = Paragraph::new(output_lines)
                    .block(Block::default().borders(Borders::ALL).title(" Output "))
                    .alignment(Alignment::Left);

                f.render_widget(output_paragraph, chunks[1]);
            }
        })?;

        self.last_render = Instant::now();
        Ok(())
    }

    /// Prints an inline message with a cyan arrow prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal output operations fail.
    pub fn print_inline(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Cyan))?;
        stdout.execute(Print("▶ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }

    /// Prints a success message with a green checkmark prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal output operations fail.
    pub fn print_success(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Green))?;
        stdout.execute(Print("✓ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }

    /// Prints an error message with a red X prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal output operations fail.
    pub fn print_error(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Red))?;
        stdout.execute(Print("✗ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }
}

/// RAII guard that restores terminal state on drop
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(crossterm::terminal::LeaveAlternateScreen);
        let _ = io::stdout().execute(Show);
    }
}

/// Run the TUI event viewer that displays events from the coordinator.
///
/// Opens an alternate screen terminal that displays a live stream of events
/// received from the coordinator. Press 'q', Esc, or Ctrl+C to exit.
///
/// # Errors
///
/// Returns an error if terminal initialization, rendering, or cleanup fails.
pub async fn run_event_viewer(client: &mut CoordinatorClient) -> io::Result<()> {
    // Set up terminal with guard for cleanup on any exit path
    enable_raw_mode()?;
    let _guard = TerminalGuard; // Restores terminal on drop, even on early return

    let mut stdout = io::stdout();
    stdout.execute(crossterm::terminal::EnterAlternateScreen)?;
    stdout.execute(Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Event history
    let mut events: Vec<String> = Vec::new();
    let max_events = 100;

    // Event stream for keyboard input
    let mut event_stream = EventStream::new();

    // Main loop
    loop {
        // Check for keyboard events (non-blocking)
        tokio::select! {
            // Handle keyboard input
            Some(Ok(crossterm_event)) = event_stream.next() => {
                if let CrosstermEvent::Key(key) = crossterm_event {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        _ => {}
                    }
                }
            }
            // Poll for events from coordinator
            result = client.recv_event() => {
                match result {
                    Ok(Some(event)) => {
                        let event_str = format_cuenv_event(&event);
                        events.push(event_str);
                        if events.len() > max_events {
                            events.remove(0);
                        }
                    }
                    Ok(None) => {
                        // No event (ping/pong)
                    }
                    Err(e) => {
                        events.push(format!("[ERROR] Connection error: {e}"));
                    }
                }
            }
            // Timeout for periodic refresh
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        // Render
        terminal.draw(|f| {
            let area = f.area();

            // Create layout
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Header
                    Constraint::Min(5),    // Events
                    Constraint::Length(3), // Footer
                ])
                .split(area);

            // Header
            let header = Paragraph::new("cuenv Event Viewer")
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(header, chunks[0]);

            // Events list
            let items: Vec<ListItem> = events
                .iter()
                .rev()
                .take(chunks[1].height as usize - 2)
                .rev()
                .map(|e| ListItem::new(e.as_str()))
                .collect();

            let events_list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Events "))
                .style(Style::default().fg(Color::White));
            f.render_widget(events_list, chunks[1]);

            // Footer with help
            let footer = Paragraph::new("Press 'q' or Esc to quit | Ctrl+C to force exit")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);
        })?;
    }

    // Guard handles terminal cleanup on drop
    Ok(())
}

/// Format a `CuenvEvent` for display
fn format_cuenv_event(event: &CuenvEvent) -> String {
    use cuenv_events::EventCategory;

    let timestamp = event.timestamp.format("%H:%M:%S%.3f");
    let source = &event.source.target;

    match &event.category {
        EventCategory::Task(task_event) => {
            use cuenv_events::TaskEvent;
            match task_event {
                TaskEvent::Started {
                    name,
                    command,
                    hermetic,
                } => {
                    format!(
                        "[{timestamp}] {source} TASK {name} started: {command} (hermetic={hermetic})"
                    )
                }
                TaskEvent::CacheHit { name, cache_key } => {
                    format!("[{timestamp}] {source} CACHE HIT {name} key={cache_key}")
                }
                TaskEvent::Output {
                    name,
                    stream,
                    content,
                } => {
                    format!("[{timestamp}] {source} OUTPUT {name}:{stream:?} {content}")
                }
                TaskEvent::Completed {
                    name,
                    success,
                    duration_ms,
                    ..
                } => {
                    let status = if *success { "✓" } else { "✗" };
                    format!(
                        "[{timestamp}] {source} TASK {name} completed {status} ({duration_ms}ms)"
                    )
                }
                _ => format!("[{timestamp}] {source} TASK {task_event:?}"),
            }
        }
        EventCategory::Command(cmd_event) => {
            format!("[{timestamp}] {source} CMD {cmd_event:?}")
        }
        EventCategory::System(sys_event) => {
            format!("[{timestamp}] {source} SYS {sys_event:?}")
        }
        EventCategory::Output(out_event) => {
            format!("[{timestamp}] {source} OUT {out_event:?}")
        }
        _ => format!("[{}] {} {:?}", timestamp, source, event.category),
    }
}

/// High-level manager for the inline TUI that handles events and state updates.
///
/// Wraps `InlineTui` and `TuiState` to provide a unified interface for
/// displaying command progress and handling UI events.
#[allow(dead_code)]
pub struct TuiManager {
    /// The underlying inline TUI renderer.
    tui: InlineTui,
    /// The current display state.
    state: TuiState,
}

#[allow(dead_code)]
impl TuiManager {
    /// Creates a new TUI manager with default state.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `InlineTui` fails to initialize.
    pub fn new() -> io::Result<Self> {
        let tui = InlineTui::new()?;
        let state = TuiState::default();

        Ok(Self { tui, state })
    }

    /// Shows the TUI display.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn show(&mut self) -> io::Result<()> {
        self.tui.show()
    }

    /// Hides the TUI display and restores the terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn hide(&mut self) -> io::Result<()> {
        self.tui.hide()
    }

    /// Handles an incoming event by updating state and re-rendering.
    ///
    /// Updates the internal state based on the event type (command start,
    /// progress, or completion) and triggers a render.
    ///
    /// # Errors
    ///
    /// Returns an error if rendering fails.
    pub fn handle_event(&mut self, event: &Event) -> io::Result<()> {
        apply_event_to_state(&mut self.state, event);

        self.tui.render(&self.state)
    }
}

fn apply_event_to_state(state: &mut TuiState, event: &Event) {
    match event {
        Event::CommandStart { command } => {
            state.command = Some(command.clone());
            state.progress = 0.0;
            state.message = "Starting...".to_string();
            state.is_complete = false;
            state.success = None;
            state.output.clear();
        }
        Event::CommandProgress {
            progress, message, ..
        } => {
            state.progress = *progress;
            state.message.clone_from(message);
        }
        Event::CommandComplete {
            success, output, ..
        } => {
            state.progress = 1.0;
            state.is_complete = true;
            state.success = Some(*success);
            state.message = if *success { "Complete" } else { "Failed" }.to_string();
            if !output.is_empty() {
                state.output.push(output.clone());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_state_default() {
        let state = TuiState::default();

        assert!(state.command.is_none());
        assert_eq!(state.progress, 0.0);
        assert!(state.message.is_empty());
        assert!(state.output.is_empty());
        assert!(!state.is_complete);
        assert!(state.success.is_none());
    }

    #[test]
    fn test_tui_state_clone() {
        let mut state = TuiState::default();
        state.command = Some("test_command".to_string());
        state.progress = 0.5;
        state.message = "Processing...".to_string();
        state.output = vec!["line1".to_string(), "line2".to_string()];
        state.is_complete = true;
        state.success = Some(true);

        let cloned_state = state.clone();

        assert_eq!(state.command, cloned_state.command);
        assert_eq!(state.progress, cloned_state.progress);
        assert_eq!(state.message, cloned_state.message);
        assert_eq!(state.output, cloned_state.output);
        assert_eq!(state.is_complete, cloned_state.is_complete);
        assert_eq!(state.success, cloned_state.success);
    }

    #[test]
    fn test_tui_manager_event_command_start() {
        let mut state = TuiState::default();
        let event = Event::CommandStart {
            command: "build".to_string(),
        };

        apply_event_to_state(&mut state, &event);

        assert_eq!(state.command, Some("build".to_string()));
        assert_eq!(state.progress, 0.0);
        assert_eq!(state.message, "Starting...");
        assert!(!state.is_complete);
        assert!(state.success.is_none());
    }

    #[test]
    fn test_tui_manager_event_command_progress() {
        let mut state = TuiState {
            command: Some("build".to_string()),
            ..Default::default()
        };

        let event = Event::CommandProgress {
            command: "build".to_string(),
            progress: 0.75,
            message: "Compiling...".to_string(),
        };

        apply_event_to_state(&mut state, &event);

        assert!((state.progress - 0.75).abs() < f32::EPSILON);
        assert_eq!(state.message, "Compiling...");
        assert!(!state.is_complete);
    }

    #[test]
    fn test_tui_manager_event_command_complete_success() {
        let mut state = TuiState {
            command: Some("build".to_string()),
            ..Default::default()
        };

        let event = Event::CommandComplete {
            command: "build".to_string(),
            success: true,
            output: "Build successful".to_string(),
        };

        apply_event_to_state(&mut state, &event);

        assert!((state.progress - 1.0).abs() < f32::EPSILON);
        assert_eq!(state.message, "Complete");
        assert_eq!(state.output, vec!["Build successful".to_string()]);
        assert!(state.is_complete);
        assert_eq!(state.success, Some(true));
    }

    #[test]
    fn test_tui_manager_event_command_complete_failure() {
        let mut state = TuiState {
            command: Some("build".to_string()),
            ..Default::default()
        };

        let event = Event::CommandComplete {
            command: "build".to_string(),
            success: false,
            output: "Build failed with errors".to_string(),
        };

        apply_event_to_state(&mut state, &event);

        assert!((state.progress - 1.0).abs() < f32::EPSILON);
        assert_eq!(state.message, "Failed");
        assert_eq!(state.output, vec!["Build failed with errors".to_string()]);
        assert!(state.is_complete);
        assert_eq!(state.success, Some(false));
    }

    #[test]
    fn test_tui_manager_multiple_output_lines() {
        let mut state = TuiState::default();

        // Add multiple output lines
        let outputs = [
            "Starting compilation...",
            "Compiling main.rs",
            "Compiling lib.rs",
            "Finished compilation",
        ];

        for output in &outputs {
            state.output.push((*output).to_string());
        }

        assert_eq!(state.output.len(), 4);
        assert_eq!(state.output[0], "Starting compilation...");
        assert_eq!(state.output[3], "Finished compilation");
    }

    #[test]
    fn test_progress_values() {
        let mut state = TuiState::default();

        // Test various progress values
        let progress_values = vec![0.0, 0.25, 0.5, 0.75, 1.0];

        for progress in progress_values {
            state.progress = progress;
            assert!((0.0..=1.0).contains(&state.progress));
        }
    }

    #[test]
    fn test_tui_state_debug_format() {
        let state = TuiState {
            command: Some("test".to_string()),
            progress: 0.5,
            message: "Testing...".to_string(),
            output: vec!["output1".to_string()],
            is_complete: false,
            success: None,
        };

        let debug_str = format!("{state:?}");
        assert!(debug_str.contains("TuiState"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("0.5"));
        assert!(debug_str.contains("Testing..."));
    }

    #[test]
    fn test_event_handling_sequence() {
        let mut state = TuiState::default();

        // Simulate a complete command sequence
        let events = vec![
            Event::CommandStart {
                command: "deploy".to_string(),
            },
            Event::CommandProgress {
                command: "deploy".to_string(),
                progress: 0.3,
                message: "Preparing...".to_string(),
            },
            Event::CommandProgress {
                command: "deploy".to_string(),
                progress: 0.7,
                message: "Uploading...".to_string(),
            },
            Event::CommandComplete {
                command: "deploy".to_string(),
                success: true,
                output: "Deployment successful".to_string(),
            },
        ];

        for event in events {
            apply_event_to_state(&mut state, &event);
        }

        assert_eq!(state.command, Some("deploy".to_string()));
        assert!((state.progress - 1.0).abs() < f32::EPSILON);
        assert_eq!(state.message, "Complete");
        assert_eq!(state.output, vec!["Deployment successful".to_string()]);
        assert!(state.is_complete);
        assert_eq!(state.success, Some(true));
    }

    #[test]
    fn test_empty_output_not_added() {
        let mut state = TuiState::default();

        let event = Event::CommandComplete {
            command: "test".to_string(),
            success: true,
            output: String::new(), // Empty output
        };

        apply_event_to_state(&mut state, &event);

        // Empty output should not be added to the output vector
        assert!(state.output.is_empty());
        assert_eq!(state.message, "Complete");
        assert!(state.is_complete);
    }
}
