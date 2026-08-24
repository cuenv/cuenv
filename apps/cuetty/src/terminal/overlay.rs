//! Backend-neutral visual overlays for a sampled terminal frame.
//!
//! Selection and search own logical, half-open cell positions. This module
//! clamps those positions to a single sampled frame and expands partial wide
//! cell coverage so a renderer can paint physical cell rectangles without
//! knowing about GPUI or Rio.

use super::model::{CellOccupancy, TerminalFrame};
use super::search::SearchMatch;
use super::selection::{LogicalPosition, Selection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Selection,
    CurrentSearchMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellOverlaySpan {
    pub row: usize,
    pub start_column: usize,
    pub end_column: usize,
    pub kind: OverlayKind,
}

/// Renderer input which remains independent of both the terminal engine and
/// the selected UI toolkit. Ordering is deliberate: a current search match is
/// emitted after a selection so it remains distinguishable where they overlap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalOverlays {
    pub selection: Option<Selection>,
    pub current_search_match: Option<SearchMatch>,
}

impl TerminalOverlays {
    pub fn spans(self, frame: &TerminalFrame) -> Vec<CellOverlaySpan> {
        let mut spans = Vec::new();
        if let Some(selection) = self.selection {
            spans.extend(range_spans(
                frame,
                selection.anchor,
                selection.active,
                OverlayKind::Selection,
            ));
        }
        if let Some(search_match) = self.current_search_match {
            spans.extend(range_spans(
                frame,
                search_match.start,
                search_match.end,
                OverlayKind::CurrentSearchMatch,
            ));
        }
        spans
    }
}

fn range_spans(
    frame: &TerminalFrame,
    first: LogicalPosition,
    second: LogicalPosition,
    kind: OverlayKind,
) -> Vec<CellOverlaySpan> {
    let (start, end) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let row_count = frame.rows.len().min(frame.dimensions.rows as usize);
    if row_count == 0 || start.line >= row_count {
        return Vec::new();
    }
    let last_row = end.line.min(row_count - 1);
    let mut spans = Vec::new();
    for row in start.line..=last_row {
        let cells = &frame.rows[row].cells;
        let mut from = if row == start.line { start.column } else { 0 }.min(cells.len());
        let mut to = if row == end.line {
            end.column.min(cells.len())
        } else {
            cells.len()
        };
        if from >= to {
            continue;
        }

        // A logical range may begin/end inside the two physical cells occupied
        // by one wide scalar. Paint both cells in that case, but never invent
        // a glyph for either spacer.
        for (column, cell) in cells[from..to].iter().enumerate() {
            let column = from + column;
            match cell.occupancy {
                CellOccupancy::Wide => to = to.max((column + 2).min(cells.len())),
                CellOccupancy::TrailingSpacer => from = from.min(column.saturating_sub(1)),
                CellOccupancy::Narrow | CellOccupancy::LeadingSpacer => {}
            }
        }
        if from < to {
            spans.push(CellOverlaySpan {
                row,
                start_column: from,
                end_column: to,
                kind,
            });
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::{
        CursorState, Known, SamplingToken, TerminalCell, TerminalDimensions, TerminalRow,
    };

    fn frame(rows: Vec<(Vec<TerminalCell>, bool)>) -> TerminalFrame {
        let columns = rows.first().map_or(0, |row| row.0.len()) as u16;
        TerminalFrame {
            dimensions: TerminalDimensions::new(rows.len() as u16, columns),
            rows: rows
                .into_iter()
                .map(|(cells, soft_wrapped)| TerminalRow {
                    cells,
                    soft_wrapped,
                    dirty: false,
                })
                .collect(),
            cursor: CursorState {
                position: None,
                shape: Known::Unknown,
                visible: Known::Unknown,
            },
            viewport_offset: None,
            sampling_token: SamplingToken(0),
            stable_revision: None,
        }
    }

    #[test]
    fn spans_expand_a_selected_trailing_wide_cell() {
        let frame = frame(vec![(
            vec![
                TerminalCell::narrow('a'),
                TerminalCell::wide('界'),
                TerminalCell::trailing_spacer(),
                TerminalCell::narrow('b'),
            ],
            false,
        )]);
        assert_eq!(
            TerminalOverlays {
                selection: Some(Selection::new(
                    LogicalPosition { line: 0, column: 2 },
                    LogicalPosition { line: 0, column: 3 },
                )),
                current_search_match: None,
            }
            .spans(&frame),
            vec![CellOverlaySpan {
                row: 0,
                start_column: 1,
                end_column: 3,
                kind: OverlayKind::Selection,
            }]
        );
    }

    #[test]
    fn spans_clamp_rows_and_layer_current_match_after_selection() {
        let frame = frame(vec![
            (
                vec![TerminalCell::narrow('a'), TerminalCell::narrow('b')],
                true,
            ),
            (
                vec![TerminalCell::leading_spacer(), TerminalCell::narrow('c')],
                false,
            ),
        ]);
        assert_eq!(
            TerminalOverlays {
                selection: Some(Selection::new(
                    LogicalPosition { line: 0, column: 1 },
                    LogicalPosition { line: 9, column: 9 },
                )),
                current_search_match: Some(SearchMatch {
                    start: LogicalPosition { line: 1, column: 0 },
                    end: LogicalPosition { line: 1, column: 2 },
                }),
            }
            .spans(&frame),
            vec![
                CellOverlaySpan {
                    row: 0,
                    start_column: 1,
                    end_column: 2,
                    kind: OverlayKind::Selection,
                },
                CellOverlaySpan {
                    row: 1,
                    start_column: 0,
                    end_column: 2,
                    kind: OverlayKind::Selection,
                },
                CellOverlaySpan {
                    row: 1,
                    start_column: 0,
                    end_column: 2,
                    kind: OverlayKind::CurrentSearchMatch,
                },
            ]
        );
    }

    #[test]
    fn spans_ignore_ranges_starting_outside_the_visible_frame() {
        let frame = frame(vec![(vec![TerminalCell::narrow('a')], false)]);
        assert!(
            TerminalOverlays {
                selection: Some(Selection::new(
                    LogicalPosition { line: 2, column: 0 },
                    LogicalPosition { line: 3, column: 0 },
                )),
                current_search_match: None,
            }
            .spans(&frame)
            .is_empty()
        );
    }
}
