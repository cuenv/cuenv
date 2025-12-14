//! Rich TUI with DAG visualization and split-screen task output

use super::state::RichTuiState;
use super::widgets::dag::DagWidget;
use super::widgets::task_panes::TaskPanesWidget;
use crate::coordinator::client::CoordinatorClient;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, cursor::{Hide, Show}};
use cuenv_events::CuenvEvent;
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::io::{self, Stdout};
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

/// Run the rich TUI with DAG visualization and task outputs
pub async fn run_rich_tui(client: &mut CoordinatorClient) -> io::Result<()> {
    // Set up terminal with guard for cleanup on any exit path
    enable_raw_mode()?;
    let _guard = TerminalGuard; // Restores terminal on drop

    let mut stdout = io::stdout();
    stdout.execute(crossterm::terminal::EnterAlternateScreen)?;
    stdout.execute(Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Initialize state
    let mut state = RichTuiState::new(1000, 8);

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
                        KeyCode::Char('q') | KeyCode::Esc => {
                            if state.is_complete {
                                break;
                            }
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        _ => {}
                    }
                }
            }
            // Poll for events from coordinator
            result = client.recv_event() => {
                match result {
                    Ok(Some(event)) => {
                        handle_cuenv_event(&mut state, &event);
                    }
                    Ok(None) => {
                        // No event (ping/pong)
                    }
                    Err(_e) => {
                        // Connection error - break the loop
                        break;
                    }
                }
            }
            // Timeout for periodic refresh
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        // Render
        terminal.draw(|f| {
            let area = f.area();

            // Create main layout: [Header | DAG | Task Panes | Footer]
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),      // Header
                    Constraint::Percentage(25), // DAG (top 25%)
                    Constraint::Percentage(65), // Task panes (middle 65%)
                    Constraint::Length(3),      // Footer (status bar)
                ])
                .split(area);

            // Render header
            let elapsed = state.elapsed();
            let elapsed_secs = elapsed.as_secs();
            let header_text = format!(
                "cuenv Task Execution - Elapsed: {}m {:02}s",
                elapsed_secs / 60,
                elapsed_secs % 60
            );
            let header = Paragraph::new(header_text)
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(header, chunks[0]);

            // Render DAG
            let dag_widget = DagWidget::new(&state.dag_state)
                .block(Block::default().borders(Borders::ALL).title(" Task Graph "))
                .show_levels(true);
            f.render_widget(dag_widget, chunks[1]);

            // Render task panes
            let panes_widget = TaskPanesWidget::new(&state.panes_state)
                .direction(Direction::Horizontal);
            f.render_widget(panes_widget, chunks[2]);

            // Render footer (status bar)
            let status_text = format_status_bar(&state);
            let footer = Paragraph::new(status_text)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[3]);
        })?;

        // Exit if complete and user wants to quit
        if state.is_complete {
            // Allow one more render cycle to show final state
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    Ok(())
}

/// Handle a cuenv event and update TUI state
fn handle_cuenv_event(state: &mut RichTuiState, event: &CuenvEvent) {
    use cuenv_events::{EventCategory, TaskEvent};

    match &event.category {
        EventCategory::Task(task_event) => match task_event {
            TaskEvent::Started { name, .. } => {
                use super::widgets::dag::TaskStatus;
                state.update_task_status(name, TaskStatus::Running);
            }
            TaskEvent::CacheHit { name, .. } => {
                use super::widgets::dag::TaskStatus;
                state.update_task_status(name, TaskStatus::Cached);
            }
            TaskEvent::Output {
                name,
                content,
                ..
            } => {
                state.push_task_output(name, content.clone());
            }
            TaskEvent::Completed { name, success, .. } => {
                use super::widgets::dag::TaskStatus;
                let status = if *success {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };
                state.update_task_status(name, status);
            }
            TaskEvent::GroupStarted { name, task_count, .. } => {
                // Mark group as started (could add visual indicator later)
                state.push_task_output(
                    name,
                    format!("Starting group with {} tasks", task_count),
                );
            }
            TaskEvent::GroupCompleted { name, success, duration_ms } => {
                state.push_task_output(
                    name,
                    format!(
                        "Group completed: {} in {}ms",
                        if *success { "success" } else { "failed" },
                        duration_ms
                    ),
                );
            }
            _ => {}
        },
        EventCategory::System(sys_event) => {
            use cuenv_events::SystemEvent;
            if matches!(sys_event, SystemEvent::Shutdown) {
                state.set_complete(true);
            }
        }
        _ => {}
    }
}

/// Format the status bar text
fn format_status_bar(state: &RichTuiState) -> String {
    use super::widgets::dag::TaskStatus;

    let nodes = &state.dag_state.nodes;

    let pending = nodes.iter().filter(|n| n.status == TaskStatus::Pending).count();
    let running = nodes.iter().filter(|n| n.status == TaskStatus::Running).count();
    let completed = nodes
        .iter()
        .filter(|n| n.status == TaskStatus::Completed || n.status == TaskStatus::Cached)
        .count();
    let failed = nodes.iter().filter(|n| n.status == TaskStatus::Failed).count();

    let mut parts = vec![];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if completed > 0 {
        parts.push(format!("{completed} completed"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if pending > 0 {
        parts.push(format!("{pending} pending"));
    }

    let status = parts.join(", ");

    if state.is_complete {
        format!("Press 'q' or Esc to quit | {status}")
    } else {
        format!("Press Ctrl+C to abort | {status}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::widgets::dag::TaskStatus;

    #[test]
    fn test_format_status_bar_initial() {
        let mut state = RichTuiState::default();
        state.add_dag_task("task1".to_string(), 0, 0);
        state.add_dag_task("task2".to_string(), 1, 0);

        let status = format_status_bar(&state);
        assert!(status.contains("2 pending"));
        assert!(status.contains("Ctrl+C"));
    }

    #[test]
    fn test_format_status_bar_running() {
        let mut state = RichTuiState::default();
        state.add_dag_task("task1".to_string(), 0, 0);
        state.update_task_status("task1", TaskStatus::Running);

        let status = format_status_bar(&state);
        assert!(status.contains("1 running"));
    }

    #[test]
    fn test_format_status_bar_complete() {
        let mut state = RichTuiState::default();
        state.add_dag_task("task1".to_string(), 0, 0);
        state.update_task_status("task1", TaskStatus::Completed);
        state.set_complete(true);

        let status = format_status_bar(&state);
        assert!(status.contains("1 completed"));
        assert!(status.contains("Press 'q'"));
    }
}
