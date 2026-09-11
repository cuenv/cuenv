//! Small host-side interaction layer for the currently sampled Rio frame.
//!
//! This deliberately addresses the visible frame only. It neither asks Rio for
//! history nor maintains a second scrollback transcript. Search remains
//! literal-only; regex, scrollback, and full-history search are staged work.

use super::model::TerminalFrame;
use super::search::{LiteralSearch, SearchMatch, SearchOptions};
use super::selection::{LogicalPosition, Selection, TextSource};

/// A `TextSource` view over one sampled terminal frame. Physical rows, soft
/// wrap boundaries, and spacer cells are intentionally preserved verbatim.
pub struct FrameTextSource<'a> {
    frame: &'a TerminalFrame,
}

impl<'a> FrameTextSource<'a> {
    pub fn new(frame: &'a TerminalFrame) -> Self {
        Self { frame }
    }
}

impl TextSource for FrameTextSource<'_> {
    fn line_count(&self) -> usize {
        self.frame.rows.len()
    }

    fn line_cells(&self, line: usize) -> &[super::model::TerminalCell] {
        &self.frame.rows[line].cells
    }

    fn line_soft_wrapped(&self, line: usize) -> bool {
        self.frame.rows[line].soft_wrapped
    }
}

/// Converts pointer offsets within a terminal viewport to physical cell
/// positions, using the same snapped metrics as Rio sizing and rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellHitTest {
    pub cell_width: f32,
    pub cell_height: f32,
    pub rows: usize,
    pub columns: usize,
}

impl CellHitTest {
    pub fn hit(self, x: f32, y: f32) -> LogicalPosition {
        let column = (x.max(0.0) / self.cell_width.max(1.0)).floor() as usize;
        let line = (y.max(0.0) / self.cell_height.max(1.0)).floor() as usize;
        LogicalPosition {
            line: line.min(self.rows.saturating_sub(1)),
            column: column.min(self.columns.saturating_sub(1)),
        }
    }

    /// A pointer owns the cell it lands in, so its exclusive selection end is
    /// one physical column beyond that cell where possible.
    pub fn exclusive_end(self, hit: LogicalPosition) -> LogicalPosition {
        LogicalPosition {
            line: hit.line,
            column: (hit.column + 1).min(self.columns),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKey {
    Character(char),
    Backspace,
    Enter { reverse: bool },
    Escape,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractionState {
    selection: Option<Selection>,
    /// The cell where the pointer press began. This is intentionally kept
    /// separate from the half-open selection endpoints: when dragging
    /// backwards, the endpoint containing the press must be one cell past the
    /// origin while the other endpoint starts at the destination.
    pointer_origin: Option<LogicalPosition>,
    dragging: bool,
    search: Option<LiteralSearch>,
    current_match: Option<SearchMatch>,
}

impl InteractionState {
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn search(&self) -> Option<&LiteralSearch> {
        self.search.as_ref()
    }

    pub fn current_match(&self) -> Option<SearchMatch> {
        self.current_match
    }

    /// Visible-frame coordinates cannot be carried across a history viewport
    /// change without pointing at different content.
    pub fn viewport_changed(&mut self) {
        self.selection = None;
        self.pointer_origin = None;
        self.dragging = false;
        self.search = None;
        self.current_match = None;
    }

    pub fn begin_selection(&mut self, _hit: CellHitTest, position: LogicalPosition) {
        self.selection = Some(Selection::new(position, position));
        self.pointer_origin = Some(position);
        self.dragging = true;
    }

    pub fn extend_selection(&mut self, hit: CellHitTest, position: LogicalPosition) {
        if self.dragging
            && let (Some(origin), Some(selection)) = (self.pointer_origin, &mut self.selection)
        {
            // A click, or a drag that returns to its origin, is not a
            // selection. The destination cell is otherwise inclusive;
            // convert the two inclusive cells into a half-open range in
            // either direction.
            if origin == position {
                *selection = Selection::new(origin, origin);
            } else if origin < position {
                *selection = Selection::new(origin, hit.exclusive_end(position));
            } else {
                *selection = Selection::new(hit.exclusive_end(origin), position);
            }
        }
    }

    pub fn end_selection(&mut self) {
        self.dragging = false;
        self.pointer_origin = None;
    }

    pub fn copy(&self, frame: &TerminalFrame) -> Option<String> {
        self.selection
            .filter(|selection| selection.anchor != selection.active)
            .map(|selection| selection.copy(&FrameTextSource::new(frame)))
    }

    pub fn enter_search(&mut self, frame: &TerminalFrame) {
        let mut search = LiteralSearch::new("", SearchOptions::default());
        let _ = search.rebuild(&FrameTextSource::new(frame));
        self.search = Some(search);
        self.current_match = None;
    }

    /// Updates the literal search state and returns the currently navigated
    /// match, suitable for display as the active selection.
    pub fn search_key(&mut self, key: SearchKey, frame: &TerminalFrame) -> Option<SearchMatch> {
        if matches!(key, SearchKey::Escape) {
            self.search = None;
            self.current_match = None;
            return None;
        }
        let search = self.search.as_mut()?;
        let matched = match key {
            SearchKey::Escape => None,
            SearchKey::Character(character) => {
                let mut query = search.query().to_owned();
                query.push(character);
                search.set_query(query);
                let _ = search.rebuild(&FrameTextSource::new(frame));
                search.next()
            }
            SearchKey::Backspace => {
                let mut query = search.query().to_owned();
                query.pop();
                search.set_query(query);
                let _ = search.rebuild(&FrameTextSource::new(frame));
                search.next()
            }
            SearchKey::Enter { reverse } => {
                if reverse {
                    search.previous()
                } else {
                    search.next()
                }
            }
        };
        self.current_match = matched;
        matched
    }

    pub fn select_match(&mut self, matched: SearchMatch) {
        self.selection = Some(Selection::new(matched.start, matched.end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::{
        CursorShape, CursorState, Known, SamplingToken, TerminalCell, TerminalDimensions,
        TerminalRow,
    };

    fn frame(rows: Vec<(&str, bool)>) -> TerminalFrame {
        TerminalFrame {
            dimensions: TerminalDimensions::new(rows.len() as u16, 4),
            rows: rows
                .into_iter()
                .map(|(text, soft_wrapped)| TerminalRow {
                    soft_wrapped,
                    dirty: false,
                    cells: text.chars().map(TerminalCell::narrow).collect(),
                })
                .collect(),
            cursor: CursorState {
                position: None,
                shape: Known::Known(CursorShape::Block),
                visible: Known::Known(true),
            },
            viewport_offset: None,
            sampling_token: SamplingToken(1),
            stable_revision: None,
        }
    }

    #[test]
    fn hit_testing_clamps_and_uses_snapped_cell_coordinates() {
        let hit = CellHitTest {
            cell_width: 9.0,
            cell_height: 16.0,
            rows: 2,
            columns: 4,
        };
        assert_eq!(hit.hit(18.1, 17.0), LogicalPosition { line: 1, column: 2 });
        assert_eq!(hit.hit(-3.0, 999.0), LogicalPosition { line: 1, column: 0 });
        assert_eq!(
            hit.exclusive_end(LogicalPosition { line: 1, column: 3 }),
            LogicalPosition { line: 1, column: 4 }
        );
    }

    #[test]
    fn frame_source_preserves_soft_wraps_and_spacers_for_copy() {
        let mut sampled = frame(vec![("ab", true), ("cd", false)]);
        sampled.rows[0].cells.push(TerminalCell::trailing_spacer());
        let source = FrameTextSource::new(&sampled);
        assert!(source.line_soft_wrapped(0));
        assert_eq!(
            Selection::new(
                LogicalPosition { line: 0, column: 0 },
                LogicalPosition { line: 1, column: 2 }
            )
            .copy(&source),
            "abcd"
        );
    }

    #[test]
    fn drag_copy_and_literal_search_state_are_deterministic() {
        let sampled = frame(vec![("one ", false), ("one ", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 2,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.begin_selection(hit, LogicalPosition { line: 0, column: 0 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.end_selection();
        assert_eq!(state.copy(&sampled).as_deref(), Some("one"));

        state.enter_search(&sampled);
        let first = state
            .search_key(SearchKey::Character('o'), &sampled)
            .unwrap();
        assert_eq!(state.current_match(), Some(first));
        state.select_match(first);
        assert_eq!(
            state.selection(),
            Some(Selection::new(first.start, first.end))
        );
        state.search_key(SearchKey::Character('n'), &sampled);
        let next = state
            .search_key(SearchKey::Enter { reverse: false }, &sampled)
            .unwrap();
        assert_eq!(next.start.line, 1);
        state.search_key(SearchKey::Escape, &sampled);
        assert!(state.search().is_none());
        assert!(state.current_match().is_none());
    }

    #[test]
    fn drag_includes_both_end_cells_in_either_direction() {
        let sampled = frame(vec![("abcd", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };

        let mut forward = InteractionState::default();
        forward.begin_selection(hit, LogicalPosition { line: 0, column: 1 });
        forward.extend_selection(hit, LogicalPosition { line: 0, column: 3 });
        assert_eq!(forward.copy(&sampled).as_deref(), Some("bcd"));

        let mut reverse = InteractionState::default();
        reverse.begin_selection(hit, LogicalPosition { line: 0, column: 3 });
        reverse.extend_selection(hit, LogicalPosition { line: 0, column: 1 });
        assert_eq!(reverse.copy(&sampled).as_deref(), Some("bcd"));
    }

    #[test]
    fn same_cell_click_does_not_create_a_selection() {
        let sampled = frame(vec![("abcd", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.begin_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.end_selection();
        assert_eq!(state.copy(&sampled), None);
    }

    #[test]
    fn cross_soft_wrap_drag_keeps_the_logical_line_contiguous() {
        let sampled = frame(vec![("ab", true), ("cd", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 2,
            columns: 2,
        };
        let mut state = InteractionState::default();
        state.begin_selection(hit, LogicalPosition { line: 1, column: 1 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 0 });
        assert_eq!(state.copy(&sampled).as_deref(), Some("abcd"));
    }

    #[test]
    fn viewport_change_clears_visible_frame_coordinates() {
        let sampled = frame(vec![("abcd", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.begin_selection(hit, LogicalPosition { line: 0, column: 0 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.enter_search(&sampled);

        state.viewport_changed();

        assert_eq!(state.copy(&sampled), None);
        assert!(state.search().is_none());
        assert!(state.current_match().is_none());
    }

    #[test]
    fn wide_cell_hit_on_trailing_spacer_copies_the_glyph_once() {
        let mut sampled = frame(vec![("a", false)]);
        sampled.rows[0].cells = vec![
            TerminalCell::narrow('a'),
            TerminalCell::wide('界'),
            TerminalCell::trailing_spacer(),
            TerminalCell::narrow('b'),
        ];
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.begin_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 1 });
        assert_eq!(state.copy(&sampled).as_deref(), Some("界"));
    }
}
