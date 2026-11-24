//! Link targets from the same terminal frame that is painted.

use std::sync::LazyLock;

use regex::Regex;

use super::model::{CellOccupancy, TerminalFrame};
use super::selection::LogicalPosition;

const MAX_LINK_CELLS: usize = 4096;
const MAX_LINK_BYTES: usize = 16384;
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>\"']+"#).expect("valid URL pattern"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Link {
    pub destination: String,
    pub cells: Vec<LogicalPosition>,
}

/// Only open when the grid still contains exactly what was shown at the press.
pub(super) fn same_content(shown: &TerminalFrame, current: &TerminalFrame) -> bool {
    shown.dimensions == current.dimensions
        && shown.viewport_offset == current.viewport_offset
        && shown.rows == current.rows
}

fn web_url(text: &str) -> Option<String> {
    if text.len() > MAX_LINK_BYTES || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(text.to_owned())
}

/// Find a link at a visible physical cell, retaining Rio's wrap boundaries.
pub(super) fn link_at(frame: &TerminalFrame, mut position: LogicalPosition) -> Option<Link> {
    let row = frame.rows.get(position.line)?;
    let mut cell = row.cells.get(position.column)?;
    if cell.occupancy == CellOccupancy::TrailingSpacer && position.column > 0 {
        position.column -= 1;
        cell = row.cells.get(position.column)?;
    }
    if cell.style.hidden {
        return None;
    }
    if let Some(target) = &cell.hyperlink {
        let destination = web_url(target)?;
        let connected = |pos: LogicalPosition| {
            frame
                .rows
                .get(pos.line)
                .and_then(|row| row.cells.get(pos.column))
                .is_some_and(|cell| !cell.style.hidden && cell.hyperlink.as_ref() == Some(target))
        };
        let previous = |pos: LogicalPosition| {
            if pos.column > 0 {
                Some(LogicalPosition {
                    line: pos.line,
                    column: pos.column - 1,
                })
            } else if pos.line > 0 && frame.rows[pos.line - 1].soft_wrapped {
                frame.rows[pos.line - 1]
                    .cells
                    .len()
                    .checked_sub(1)
                    .map(|column| LogicalPosition {
                        line: pos.line - 1,
                        column,
                    })
            } else {
                None
            }
        };
        let next = |pos: LogicalPosition| {
            if pos.column + 1 < frame.rows[pos.line].cells.len() {
                Some(LogicalPosition {
                    line: pos.line,
                    column: pos.column + 1,
                })
            } else if frame.rows[pos.line].soft_wrapped && pos.line + 1 < frame.rows.len() {
                Some(LogicalPosition {
                    line: pos.line + 1,
                    column: 0,
                })
            } else {
                None
            }
        };
        let mut start = position;
        let mut preceding = 0;
        while let Some(pos) = previous(start).filter(|pos| connected(*pos)) {
            preceding += 1;
            if preceding >= MAX_LINK_CELLS {
                return None;
            }
            start = pos;
        }
        let mut cells = vec![start];
        while let Some(pos) = next(*cells.last()?).filter(|pos| connected(*pos)) {
            if cells.len() >= MAX_LINK_CELLS {
                return None;
            }
            cells.push(pos);
        }
        return Some(Link { destination, cells });
    }

    let mut first = position.line;
    while first > 0 && frame.rows[first - 1].soft_wrapped {
        first -= 1;
    }
    let mut last = position.line;
    while last + 1 < frame.rows.len() && frame.rows[last].soft_wrapped {
        last += 1;
    }
    // A wrapped line extending beyond the sampled viewport has an unknown prefix or suffix.
    if (first == 0 && frame.viewport_offset.unwrap_or(0) > 0) || frame.rows[last].soft_wrapped {
        return None;
    }
    let mut text = String::new();
    let mut cells = Vec::new();
    let mut point_byte = None;
    let rows = frame.rows.iter().enumerate().take(last + 1).skip(first);
    for (line, row) in rows {
        for (column, cell) in row.cells.iter().enumerate() {
            if cells.len() >= MAX_LINK_CELLS || text.len() >= MAX_LINK_BYTES {
                return None;
            }
            let pos = LogicalPosition { line, column };
            if pos == position {
                point_byte = Some(if cell.occupancy == CellOccupancy::TrailingSpacer {
                    cells.last().map_or(text.len(), |(_, start, _)| *start)
                } else {
                    text.len()
                });
            }
            let start = text.len();
            if cell.hyperlink.is_some() || cell.style.hidden {
                text.push(' ');
            } else if let Some(value) = &cell.text {
                text.push_str(value);
            } else if cell.occupancy == CellOccupancy::TrailingSpacer {
                continue;
            }
            cells.push((pos, start, text.len()));
        }
    }
    if text.len() > MAX_LINK_BYTES {
        return None;
    }
    let point_byte = point_byte?;
    for found in URL.find_iter(&text) {
        let mut end = found.end();
        while end > found.start()
            && matches!(
                text.as_bytes()[end - 1],
                b'.' | b',' | b':' | b';' | b'!' | b'?'
            )
        {
            end -= 1;
        }
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while text[found.start()..end].ends_with(close)
                && text[found.start()..end].matches(close).count()
                    > text[found.start()..end].matches(open).count()
            {
                end -= 1;
            }
        }
        if (found.start()..end).contains(&point_byte) {
            let destination = web_url(&text[found.start()..end])?;
            let cells = cells
                .iter()
                .filter_map(|(pos, start, end_cell)| {
                    (*start < end && *end_cell > found.start()).then_some(*pos)
                })
                .collect();
            return Some(Link { destination, cells });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::{
        CursorState, Known, SamplingToken, TerminalCell, TerminalDimensions, TerminalRow,
    };

    fn frame(lines: &[(&str, bool)]) -> TerminalFrame {
        let rows: Vec<_> = lines
            .iter()
            .map(|(line, wrapped)| TerminalRow {
                soft_wrapped: *wrapped,
                dirty: true,
                cells: line.chars().map(TerminalCell::narrow).collect(),
            })
            .collect();
        TerminalFrame {
            dimensions: TerminalDimensions::new(rows.len() as u16, rows[0].cells.len() as u16),
            rows,
            cursor: CursorState {
                position: None,
                shape: Known::Unknown,
                visible: Known::Unknown,
                blinking: Known::Unknown,
            },
            viewport_offset: Some(0),
            scrollback_rows: Some(0),
            sampling_token: SamplingToken(1),
            stable_revision: None,
        }
    }

    #[test]
    fn detects_wrapped_urls_and_trims_prose_punctuation() {
        let screen = frame(&[("See https://exa", true), ("mple.org/x).", false)]);
        let link = link_at(&screen, LogicalPosition { line: 1, column: 3 }).unwrap();
        assert_eq!(link.destination, "https://example.org/x");
    }

    #[test]
    fn explicit_destination_wins_and_untrusted_scheme_cannot_fall_back() {
        let mut screen = frame(&[("https://example.org", false)]);
        for cell in &mut screen.rows[0].cells {
            cell.hyperlink = Some("file:///etc/passwd".into());
        }
        assert_eq!(
            link_at(&screen, LogicalPosition { line: 0, column: 4 }),
            None
        );
        for cell in &mut screen.rows[0].cells {
            cell.hyperlink = Some("https://other.org/a;b?x=1".into());
        }
        assert_eq!(
            link_at(&screen, LogicalPosition { line: 0, column: 4 })
                .unwrap()
                .destination,
            "https://other.org/a;b?x=1"
        );
    }

    #[test]
    fn hidden_text_and_unknown_wrapped_tail_are_not_links() {
        let mut screen = frame(&[("https://example.org", false)]);
        screen.rows[0].cells[4].style.hidden = true;
        assert!(link_at(&screen, LogicalPosition { line: 0, column: 4 }).is_none());
        screen.rows[0].cells[4].style.hidden = false;
        screen.rows[0].soft_wrapped = true;
        assert!(link_at(&screen, LogicalPosition { line: 0, column: 4 }).is_none());
    }

    #[test]
    fn changing_a_destination_or_scrolling_invalidates_the_pressed_frame() {
        let mut shown = frame(&[("click", false)]);
        shown.rows[0].cells[0].hyperlink = Some("https://one.example/".into());
        let mut current = shown.clone();
        current.sampling_token = SamplingToken(2);
        assert!(same_content(&shown, &current));
        current.rows[0].cells[0].hyperlink = Some("https://two.example/".into());
        assert!(!same_content(&shown, &current));
        current = shown.clone();
        current.viewport_offset = Some(1);
        assert!(!same_content(&shown, &current));
    }

    #[test]
    fn links_in_scrollback_and_independent_explicit_links_work() {
        let mut screen = frame(&[("earlier", false), ("https://example.org", false)]);
        screen.viewport_offset = Some(2);
        assert_eq!(
            link_at(&screen, LogicalPosition { line: 1, column: 4 })
                .unwrap()
                .destination,
            "https://example.org"
        );
        let mut explicit = frame(&[("a b", false)]);
        explicit.rows[0].cells[0].hyperlink = Some("https://example.org".into());
        explicit.rows[0].cells[2].hyperlink = Some("https://example.org".into());
        assert_eq!(
            link_at(&explicit, LogicalPosition { line: 0, column: 0 })
                .unwrap()
                .cells
                .len(),
            1
        );
    }
}
