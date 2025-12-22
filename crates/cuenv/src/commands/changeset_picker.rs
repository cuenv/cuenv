//! Interactive changeset picker using ratatui.
//!
//! Provides a multi-phase picker UI for creating changesets interactively.
//! Phase 1: Select packages and bump types
//! Phase 2: Enter summary
//! Phase 3: Optionally enter description

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use cuenv_release::BumpType;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::collections::HashMap;
use std::io;

/// Information about a selectable package.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    /// Package name.
    pub name: String,
    /// Current version.
    pub version: String,
}

/// Result of the changeset picker interaction.
#[derive(Debug)]
pub enum ChangesetPickerResult {
    /// User completed the changeset creation.
    Completed {
        /// Package bumps (name -> bump type).
        packages: Vec<(String, BumpType)>,
        /// Summary of the change.
        summary: String,
        /// Optional detailed description.
        description: Option<String>,
    },
    /// User cancelled.
    Cancelled,
}

/// Current phase of the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Selecting packages and bump types.
    PackageSelect,
    /// Entering summary.
    Summary,
    /// Entering description (optional).
    Description,
}

/// Interactive changeset picker state.
struct ChangesetPicker {
    /// Available packages.
    packages: Vec<PackageInfo>,
    /// Selected bump types per package (index -> bump type).
    bumps: HashMap<usize, BumpType>,
    /// List selection state.
    list_state: ListState,
    /// Current phase.
    phase: Phase,
    /// Summary text.
    summary: String,
    /// Description text.
    description: String,
}

impl ChangesetPicker {
    fn new(packages: Vec<PackageInfo>) -> Self {
        let mut list_state = ListState::default();
        if !packages.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            packages,
            bumps: HashMap::new(),
            list_state,
            phase: Phase::PackageSelect,
            summary: String::new(),
            description: String::new(),
        }
    }

    /// Move selection up.
    fn select_previous(&mut self) {
        if self.packages.is_empty() {
            return;
        }

        let i = match self.list_state.selected() {
            Some(0) => self.packages.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move selection down.
    fn select_next(&mut self) {
        if self.packages.is_empty() {
            return;
        }

        let i = match self.list_state.selected() {
            Some(i) if i >= self.packages.len() - 1 => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Cycle the bump type for the selected package.
    fn cycle_bump(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            let current = self.bumps.get(&idx).copied().unwrap_or(BumpType::None);
            let next = match current {
                BumpType::None => BumpType::Patch,
                BumpType::Patch => BumpType::Minor,
                BumpType::Minor => BumpType::Major,
                BumpType::Major => BumpType::None,
            };
            if next == BumpType::None {
                self.bumps.remove(&idx);
            } else {
                self.bumps.insert(idx, next);
            }
        }
    }

    /// Check if any packages are selected for bumping.
    fn has_selections(&self) -> bool {
        !self.bumps.is_empty()
    }

    /// Get the selected packages with their bump types.
    fn get_selections(&self) -> Vec<(String, BumpType)> {
        self.bumps
            .iter()
            .map(|(&idx, &bump)| (self.packages[idx].name.clone(), bump))
            .collect()
    }

    /// Move to the next phase.
    fn next_phase(&mut self) -> bool {
        match self.phase {
            Phase::PackageSelect => {
                if self.has_selections() {
                    self.phase = Phase::Summary;
                    true
                } else {
                    false
                }
            }
            Phase::Summary => {
                if self.summary.trim().is_empty() {
                    false
                } else {
                    self.phase = Phase::Description;
                    true
                }
            }
            Phase::Description => true, // Can skip description
        }
    }

    /// Move to the previous phase.
    fn previous_phase(&mut self) {
        self.phase = match self.phase {
            Phase::PackageSelect | Phase::Summary => Phase::PackageSelect,
            Phase::Description => Phase::Summary,
        };
    }
}

/// Run the interactive changeset picker.
///
/// Returns the changeset details or None if cancelled.
pub fn run_changeset_picker(packages: Vec<PackageInfo>) -> io::Result<ChangesetPickerResult> {
    if packages.is_empty() {
        return Ok(ChangesetPickerResult::Cancelled);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create picker state
    let mut picker = ChangesetPicker::new(packages);

    // Run event loop
    let result = run_event_loop(&mut terminal, &mut picker);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

/// Main event loop.
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    picker: &mut ChangesetPicker,
) -> io::Result<ChangesetPickerResult> {
    loop {
        terminal.draw(|f| draw_ui(f, picker))?;

        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Handle Ctrl+C globally
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(ChangesetPickerResult::Cancelled);
            }

            match picker.phase {
                Phase::PackageSelect => match key.code {
                    KeyCode::Esc => return Ok(ChangesetPickerResult::Cancelled),
                    KeyCode::Up | KeyCode::Char('k') => picker.select_previous(),
                    KeyCode::Down | KeyCode::Char('j') => picker.select_next(),
                    KeyCode::Char(' ') => picker.cycle_bump(),
                    KeyCode::Tab | KeyCode::Enter => {
                        if picker.has_selections() {
                            picker.next_phase();
                        }
                    }
                    _ => {}
                },
                Phase::Summary => match key.code {
                    KeyCode::Esc => picker.previous_phase(),
                    KeyCode::Enter => {
                        if !picker.summary.trim().is_empty() {
                            picker.next_phase();
                        }
                    }
                    KeyCode::Char(c) => picker.summary.push(c),
                    KeyCode::Backspace => {
                        picker.summary.pop();
                    }
                    _ => {}
                },
                Phase::Description => {
                    match key.code {
                        KeyCode::Esc => picker.previous_phase(),
                        KeyCode::Tab | KeyCode::Enter
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            // Ctrl+Enter or Ctrl+Tab to finish
                            let description = if picker.description.trim().is_empty() {
                                None
                            } else {
                                Some(picker.description.clone())
                            };
                            return Ok(ChangesetPickerResult::Completed {
                                packages: picker.get_selections(),
                                summary: picker.summary.clone(),
                                description,
                            });
                        }
                        KeyCode::Enter => {
                            // Regular Enter adds newline or finishes if empty
                            if picker.description.is_empty() {
                                return Ok(ChangesetPickerResult::Completed {
                                    packages: picker.get_selections(),
                                    summary: picker.summary.clone(),
                                    description: None,
                                });
                            }
                            picker.description.push('\n');
                        }
                        KeyCode::Char(c) => picker.description.push(c),
                        KeyCode::Backspace => {
                            picker.description.pop();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Draw the picker UI.
fn draw_ui(f: &mut Frame, picker: &mut ChangesetPicker) {
    let area = f.area();

    match picker.phase {
        Phase::PackageSelect => draw_package_select(f, picker, area),
        Phase::Summary => draw_summary_input(f, picker, area),
        Phase::Description => draw_description_input(f, picker, area),
    }
}

/// Draw the package selection phase.
fn draw_package_select(f: &mut Frame, picker: &mut ChangesetPicker, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // Header
        Constraint::Min(5),    // Package list
        Constraint::Length(1), // Help footer
    ])
    .split(area);

    // Header
    let header = Paragraph::new("Select packages and bump types (space to cycle)").block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Create Changeset ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(header, chunks[0]);

    // Package list with bump type indicators
    let items: Vec<ListItem> = picker
        .packages
        .iter()
        .enumerate()
        .map(|(idx, pkg)| {
            let bump = picker.bumps.get(&idx).copied().unwrap_or(BumpType::None);
            let (bump_str, bump_color) = match bump {
                BumpType::None => ("[ ]", Color::DarkGray),
                BumpType::Patch => ("[patch]", Color::Green),
                BumpType::Minor => ("[minor]", Color::Yellow),
                BumpType::Major => ("[MAJOR]", Color::Red),
            };

            let spans = vec![
                Span::styled(format!("{bump_str} "), Style::default().fg(bump_color)),
                Span::styled(&pkg.name, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!(" ({})", pkg.version),
                    Style::default().fg(Color::DarkGray),
                ),
            ];

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

    f.render_stateful_widget(list, chunks[1], &mut picker.list_state);

    // Help footer
    let selected_count = picker.bumps.len();
    let help = Line::from(vec![
        Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
        Span::raw(" navigate │ "),
        Span::styled("space", Style::default().fg(Color::Cyan)),
        Span::raw(" cycle bump │ "),
        Span::styled("tab/enter", Style::default().fg(Color::Cyan)),
        Span::raw(" next │ "),
        Span::styled("esc", Style::default().fg(Color::Cyan)),
        Span::raw(" cancel │ "),
        Span::styled(
            format!("{selected_count} selected"),
            Style::default().fg(if selected_count > 0 {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ),
    ]);

    f.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(Color::DarkGray))
            .centered(),
        chunks[2],
    );
}

/// Draw the summary input phase.
fn draw_summary_input(f: &mut Frame, picker: &ChangesetPicker, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // Selected packages summary
        Constraint::Length(3), // Summary input
        Constraint::Min(1),    // Spacer
        Constraint::Length(1), // Help footer
    ])
    .split(area);

    // Selected packages summary
    let selected: Vec<String> = picker
        .get_selections()
        .iter()
        .map(|(name, bump)| format!("{name}:{bump}"))
        .collect();
    let packages_text = if selected.is_empty() {
        "No packages selected".to_string()
    } else {
        selected.join(", ")
    };

    let packages_widget = Paragraph::new(packages_text)
        .style(Style::default().fg(Color::Cyan))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Packages "),
        );
    f.render_widget(packages_widget, chunks[0]);

    // Summary input
    let summary_style = if picker.summary.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let summary_text = if picker.summary.is_empty() {
        "Enter a summary of the change...".to_string()
    } else {
        picker.summary.clone()
    };

    let summary_widget = Paragraph::new(summary_text).style(summary_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " Summary ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(summary_widget, chunks[1]);

    // Show cursor
    let cursor_x = chunks[1].x + 1 + u16::try_from(picker.summary.len()).unwrap_or(0);
    let cursor_y = chunks[1].y + 1;
    f.set_cursor_position((cursor_x.min(chunks[1].x + chunks[1].width - 2), cursor_y));

    // Help footer
    let help = Line::from(vec![
        Span::styled("enter", Style::default().fg(Color::Cyan)),
        Span::raw(" next │ "),
        Span::styled("esc", Style::default().fg(Color::Cyan)),
        Span::raw(" back"),
    ]);
    f.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(Color::DarkGray))
            .centered(),
        chunks[3],
    );
}

/// Draw the description input phase.
fn draw_description_input(f: &mut Frame, picker: &ChangesetPicker, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // Summary display
        Constraint::Min(5),    // Description input
        Constraint::Length(1), // Help footer
    ])
    .split(area);

    // Summary display
    let summary_widget = Paragraph::new(picker.summary.clone())
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Summary "),
        );
    f.render_widget(summary_widget, chunks[0]);

    // Description input
    let desc_style = if picker.description.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let desc_text = if picker.description.is_empty() {
        "(Optional) Enter a detailed description...".to_string()
    } else {
        picker.description.clone()
    };

    let desc_widget = Paragraph::new(desc_text).style(desc_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " Description (optional) ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(desc_widget, chunks[1]);

    // Show cursor
    let lines: Vec<&str> = picker.description.lines().collect();
    let last_line_len = lines.last().map_or(0, |l| l.len());
    let line_count = lines.len().max(1);
    let cursor_x = chunks[1].x + 1 + u16::try_from(last_line_len).unwrap_or(0);
    let cursor_y = chunks[1].y + u16::try_from(line_count).unwrap_or(1);
    f.set_cursor_position((
        cursor_x.min(chunks[1].x + chunks[1].width - 2),
        cursor_y.min(chunks[1].y + chunks[1].height - 2),
    ));

    // Help footer
    let help = Line::from(vec![
        Span::styled("enter", Style::default().fg(Color::Cyan)),
        Span::raw(" finish │ "),
        Span::styled("ctrl+enter", Style::default().fg(Color::Cyan)),
        Span::raw(" add newline │ "),
        Span::styled("esc", Style::default().fg(Color::Cyan)),
        Span::raw(" back"),
    ]);
    f.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(Color::DarkGray))
            .centered(),
        chunks[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changeset_picker_cycle_bump() {
        let packages = vec![
            PackageInfo {
                name: "pkg-a".to_string(),
                version: "1.0.0".to_string(),
            },
            PackageInfo {
                name: "pkg-b".to_string(),
                version: "2.0.0".to_string(),
            },
        ];

        let mut picker = ChangesetPicker::new(packages);
        assert!(picker.bumps.is_empty());

        // Cycle through bump types
        picker.cycle_bump(); // None -> Patch
        assert_eq!(picker.bumps.get(&0), Some(&BumpType::Patch));

        picker.cycle_bump(); // Patch -> Minor
        assert_eq!(picker.bumps.get(&0), Some(&BumpType::Minor));

        picker.cycle_bump(); // Minor -> Major
        assert_eq!(picker.bumps.get(&0), Some(&BumpType::Major));

        picker.cycle_bump(); // Major -> None
        assert_eq!(picker.bumps.get(&0), None);
    }

    #[test]
    fn test_changeset_picker_has_selections() {
        let packages = vec![PackageInfo {
            name: "pkg-a".to_string(),
            version: "1.0.0".to_string(),
        }];

        let mut picker = ChangesetPicker::new(packages);
        assert!(!picker.has_selections());

        picker.cycle_bump();
        assert!(picker.has_selections());

        // Cycle back to None
        picker.cycle_bump();
        picker.cycle_bump();
        picker.cycle_bump();
        assert!(!picker.has_selections());
    }

    #[test]
    fn test_changeset_picker_phases() {
        let packages = vec![PackageInfo {
            name: "pkg-a".to_string(),
            version: "1.0.0".to_string(),
        }];

        let mut picker = ChangesetPicker::new(packages);
        assert_eq!(picker.phase, Phase::PackageSelect);

        // Can't advance without selections
        assert!(!picker.next_phase());
        assert_eq!(picker.phase, Phase::PackageSelect);

        // Select a package
        picker.cycle_bump();

        // Now can advance
        assert!(picker.next_phase());
        assert_eq!(picker.phase, Phase::Summary);

        // Can't advance without summary
        assert!(!picker.next_phase());
        assert_eq!(picker.phase, Phase::Summary);

        // Add summary
        picker.summary = "Test summary".to_string();
        assert!(picker.next_phase());
        assert_eq!(picker.phase, Phase::Description);

        // Can go back
        picker.previous_phase();
        assert_eq!(picker.phase, Phase::Summary);
    }
}
