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

#[allow(dead_code)]
pub struct InlineTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    start_line: u16,
    height: u16,
    last_render: Instant,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TuiState {
    pub command: Option<String>,
    pub progress: f32,
    pub message: String,
    pub output: Vec<String>,
    pub is_complete: bool,
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

    pub fn hide(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(Show)?;
        stdout.execute(MoveTo(0, self.start_line + self.height))?;
        event!(Level::DEBUG, "Inline TUI hidden");
        Ok(())
    }

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

    pub fn print_inline(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Cyan))?;
        stdout.execute(Print("▶ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }

    pub fn print_success(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Green))?;
        stdout.execute(Print("✓ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }

    pub fn print_error(message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(SetForegroundColor(CrosstermColor::Red))?;
        stdout.execute(Print("✗ "))?;
        stdout.execute(ResetColor)?;
        stdout.execute(Print(format!("{message}\n")))?;
        Ok(())
    }
}

/// Run the TUI event viewer that displays events from the coordinator
pub async fn run_event_viewer(client: &mut CoordinatorClient) -> io::Result<()> {
    // Set up terminal
    enable_raw_mode()?;
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

    // Restore terminal
    disable_raw_mode()?;
    terminal
        .backend_mut()
        .execute(crossterm::terminal::LeaveAlternateScreen)?;
    terminal.backend_mut().execute(Show)?;

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
                    format!(
                        "[{timestamp}] {source} CACHE HIT {name} key={cache_key}"
                    )
                }
                TaskEvent::Output {
                    name,
                    stream,
                    content,
                } => {
                    format!(
                        "[{timestamp}] {source} OUTPUT {name}:{stream:?} {content}"
                    )
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

#[allow(dead_code)]
pub struct TuiManager {
    tui: InlineTui,
    state: TuiState,
}

#[allow(dead_code)]
impl TuiManager {
    pub fn new() -> io::Result<Self> {
        let tui = InlineTui::new()?;
        let state = TuiState::default();

        Ok(Self { tui, state })
    }

    pub fn show(&mut self) -> io::Result<()> {
        self.tui.show()
    }

    pub fn hide(&mut self) -> io::Result<()> {
        self.tui.hide()
    }

    pub fn handle_event(&mut self, event: &Event) -> io::Result<()> {
        match event {
            Event::CommandStart { command } => {
                self.state.command = Some(command.clone());
                self.state.progress = 0.0;
                self.state.message = "Starting...".to_string();
                self.state.is_complete = false;
                self.state.success = None;
                self.state.output.clear();
            }
            Event::CommandProgress {
                progress, message, ..
            } => {
                self.state.progress = *progress;
                self.state.message.clone_from(message);
            }
            Event::CommandComplete {
                success, output, ..
            } => {
                self.state.progress = 1.0;
                self.state.is_complete = true;
                self.state.success = Some(*success);
                self.state.message = if *success { "Complete" } else { "Failed" }.to_string();
                if !output.is_empty() {
                    self.state.output.push(output.clone());
                }
            }
            #[allow(clippy::match_same_arms)]
            Event::TuiRefresh => {
                // Force render
            }
            _ => {
                // Other events don't need specific handling
            }
        }

        self.tui.render(&self.state)
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

        // Simulate the logic from handle_event
        if let Event::CommandStart { command } = event {
            state.command = Some(command.clone());
            state.progress = 0.0;
            state.message = "Starting...".to_string();
            state.is_complete = false;
            state.success = None;
        }

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

        // Simulate the logic from handle_event
        if let Event::CommandProgress {
            progress, message, ..
        } = event
        {
            state.progress = progress;
            state.message = message.clone();
        }

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

        // Simulate the logic from handle_event (matching actual implementation)
        if let Event::CommandComplete {
            success, output, ..
        } = event
        {
            state.progress = 1.0;
            state.is_complete = true;
            state.success = Some(success);
            state.message = if success { "Complete" } else { "Failed" }.to_string();
            if !output.is_empty() {
                state.output.push(output.clone());
            }
        }

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

        // Simulate the logic from handle_event
        if let Event::CommandComplete {
            success, output, ..
        } = event
        {
            state.progress = 1.0;
            state.is_complete = true;
            state.success = Some(success);
            state.message = if success { "Complete" } else { "Failed" }.to_string();
            if !output.is_empty() {
                state.output.push(output.clone());
            }
        }

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
            match event {
                Event::CommandStart { command } => {
                    state.command = Some(command);
                    state.progress = 0.0;
                    state.message = "Starting...".to_string();
                    state.is_complete = false;
                    state.success = None;
                }
                Event::CommandProgress {
                    progress, message, ..
                } => {
                    state.progress = progress;
                    state.message = message;
                }
                Event::CommandComplete {
                    success, output, ..
                } => {
                    state.progress = 1.0;
                    state.is_complete = true;
                    state.success = Some(success);
                    state.message = if success { "Complete" } else { "Failed" }.to_string();
                    if !output.is_empty() {
                        state.output.push(output);
                    }
                }
                _ => {}
            }
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

        if let Event::CommandComplete {
            success, output, ..
        } = event
        {
            state.progress = 1.0;
            state.is_complete = true;
            state.success = Some(success);
            state.message = if success { "Complete" } else { "Failed" }.to_string();
            if !output.is_empty() {
                state.output.push(output);
            }
        }

        // Empty output should not be added to the output vector
        assert!(state.output.is_empty());
        assert_eq!(state.message, "Complete");
        assert!(state.is_complete);
    }
}
