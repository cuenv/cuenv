pub mod rich;
pub mod state;
pub mod widgets;

use crate::coordinator::client::CoordinatorClient;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{
    ExecutableCommand,
    cursor::{Hide, Show},
};
use cuenv_events::CuenvEvent;
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::io;
use std::time::Duration;

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
