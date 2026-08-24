//! Cell-coordinate selection and clipboard-safe text export.

use super::model::{CellOccupancy, TerminalCell};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalPosition {
    pub line: usize,
    pub column: usize,
}

pub trait TextSource {
    fn line_count(&self) -> usize;
    fn line_cells(&self, line: usize) -> &[TerminalCell];
    fn line_soft_wrapped(&self, line: usize) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Both endpoints are physical cell coordinates in a half-open range:
    /// `[anchor, active)`. In particular, `active` is the first cell not
    /// selected. This makes a `SearchMatch` directly usable as a selection.
    pub anchor: LogicalPosition,
    pub active: LogicalPosition,
}

impl Selection {
    pub fn new(anchor: LogicalPosition, active: LogicalPosition) -> Self {
        Self { anchor, active }
    }
    pub fn bounds(self) -> (LogicalPosition, LogicalPosition) {
        if self.anchor <= self.active {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }
    pub fn copy<S: TextSource>(&self, source: &S) -> String {
        let (start, end) = self.bounds();
        if source.line_count() == 0 || start.line >= source.line_count() {
            return String::new();
        }
        let last = end.line.min(source.line_count() - 1);
        let mut out = String::new();
        for line in start.line..=last {
            let cells = source.line_cells(line);
            let mut from = if line == start.line { start.column } else { 0 };
            // A wide glyph occupies a real cell followed by a trailing
            // spacer. Pointer hit testing can land on that spacer, but it is
            // still the same user-visible cell. Include the glyph when the
            // half-open range starts there; spacers themselves never become
            // clipboard text.
            if from > 0
                && from < cells.len()
                && cells[from].occupancy == CellOccupancy::TrailingSpacer
                && cells[from - 1].occupancy == CellOccupancy::Wide
            {
                from -= 1;
            }
            let to_exclusive = if line == end.line {
                end.column.min(cells.len())
            } else {
                cells.len()
            };
            if from < to_exclusive {
                for cell in cells.get(from..to_exclusive).into_iter().flatten() {
                    if matches!(cell.occupancy, CellOccupancy::Narrow | CellOccupancy::Wide)
                        && let Some(ch) = cell.codepoint
                    {
                        out.push(ch);
                    }
                }
            }
            if line != last && !source.line_soft_wrapped(line) {
                out.push('\n');
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct S {
        rows: Vec<(Vec<TerminalCell>, bool)>,
    }
    impl TextSource for S {
        fn line_count(&self) -> usize {
            self.rows.len()
        }
        fn line_cells(&self, l: usize) -> &[TerminalCell] {
            &self.rows[l].0
        }
        fn line_soft_wrapped(&self, l: usize) -> bool {
            self.rows[l].1
        }
    }
    #[test]
    fn copy_skips_spacers_and_keeps_soft_wraps() {
        let s = S {
            rows: vec![
                (
                    vec![
                        TerminalCell::narrow('a'),
                        TerminalCell::wide('界'),
                        TerminalCell::trailing_spacer(),
                    ],
                    true,
                ),
                (
                    vec![TerminalCell::leading_spacer(), TerminalCell::narrow('b')],
                    false,
                ),
                (vec![TerminalCell::narrow('c')], false),
            ],
        };
        assert_eq!(
            Selection::new(
                LogicalPosition { line: 0, column: 0 },
                LogicalPosition { line: 1, column: 2 }
            )
            .copy(&s),
            "a界b"
        );
        assert_eq!(
            Selection::new(
                LogicalPosition { line: 0, column: 0 },
                LogicalPosition { line: 2, column: 1 }
            )
            .copy(&s),
            "a界b\nc"
        );
    }

    #[test]
    fn copy_uses_an_exclusive_active_endpoint() {
        let s = S {
            rows: vec![("abcd".chars().map(TerminalCell::narrow).collect(), false)],
        };
        assert_eq!(
            Selection::new(
                LogicalPosition { line: 0, column: 1 },
                LogicalPosition { line: 0, column: 3 }
            )
            .copy(&s),
            "bc"
        );
    }
}
