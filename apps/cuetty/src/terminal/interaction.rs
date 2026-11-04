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

/// Who made the selection, which decides whether search may take it back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SelectionSource {
    #[default]
    User,
    SearchMatch,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractionState {
    selection: Option<Selection>,
    selection_source: SelectionSource,
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

    /// Whether a press in the terminal is being dragged into a selection.
    pub fn selecting(&self) -> bool {
        self.dragging
    }

    pub fn search(&self) -> Option<&LiteralSearch> {
        self.search.as_ref()
    }

    pub fn current_match(&self) -> Option<SearchMatch> {
        self.current_match
    }

    /// Every match on screen, while a search is open.
    pub fn search_matches(&self) -> &[SearchMatch] {
        self.search.as_ref().map_or(&[], LiteralSearch::matches)
    }

    /// Takes in the sample of the screen after a scroll, and says whether the view moved.
    /// One that did not, because an application took the wheel or history had run out,
    /// leaves the selection and the search where they were: nothing has moved under them.
    pub fn scrolled_from(&mut self, before: Option<usize>, frame: &TerminalFrame) -> bool {
        let moved = frame.viewport_offset != before;
        if moved {
            self.viewport_changed(frame);
        }
        moved
    }

    /// Visible-frame coordinates cannot be carried across a history viewport
    /// change without pointing at different content. A search stays open for the
    /// text that has come into view, with navigation starting over.
    pub fn viewport_changed(&mut self, frame: &TerminalFrame) {
        self.place_selection(None, SelectionSource::User);
        self.pointer_origin = None;
        self.dragging = false;
        self.current_match = None;
        if let Some(search) = &mut self.search {
            let _ = search.rebuild(&FrameTextSource::new(frame));
        }
    }

    /// Searches a new sample of the screen, so highlights stay on the text they belong to
    /// as output moves it. A selection that was only marking the current match moves with
    /// it; one the user made is left alone.
    pub fn refresh_search(&mut self, frame: &TerminalFrame) {
        let Some(search) = &mut self.search else {
            return;
        };
        let _ = search.refresh(&FrameTextSource::new(frame));
        let current = search.current_match();
        self.current_match = current;
        if self.selection_source == SelectionSource::SearchMatch {
            self.place_selection(current.map(match_selection), SelectionSource::SearchMatch);
        }
    }

    pub fn begin_selection(&mut self, _hit: CellHitTest, position: LogicalPosition) {
        self.place_selection(
            Some(Selection::new(position, position)),
            SelectionSource::User,
        );
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
        self.release_search_selection();
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
                search.next_match()
            }
            SearchKey::Backspace => {
                let mut query = search.query().to_owned();
                query.pop();
                search.set_query(query);
                let _ = search.rebuild(&FrameTextSource::new(frame));
                search.next_match()
            }
            SearchKey::Enter { reverse } => {
                if reverse {
                    search.previous_match()
                } else {
                    search.next_match()
                }
            }
        };
        // What was only marking the last match goes with it, when there is none to move to.
        if matched.is_none() {
            self.release_search_selection();
        }
        self.current_match = matched;
        matched
    }

    pub fn select_match(&mut self, matched: SearchMatch) {
        self.place_selection(Some(match_selection(matched)), SelectionSource::SearchMatch);
    }

    fn place_selection(&mut self, selection: Option<Selection>, source: SelectionSource) {
        self.selection = selection;
        self.selection_source = source;
    }

    /// Closing a search leaves its last match selected, to be copied, so this also finds a
    /// selection from a search that has since closed. One the user made stays.
    fn release_search_selection(&mut self) {
        if self.selection_source == SelectionSource::SearchMatch {
            self.place_selection(None, SelectionSource::User);
        }
    }
}

fn match_selection(matched: SearchMatch) -> Selection {
    Selection::new(matched.start, matched.end)
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
                blinking: Known::Unknown,
            },
            viewport_offset: None,
            scrollback_rows: None,
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
    fn a_search_highlights_every_match_until_it_closes() {
        let sampled = frame(vec![("one ", false), ("one ", false)]);
        let mut state = InteractionState::default();
        assert!(state.search_matches().is_empty());
        state.enter_search(&sampled);
        for key in "one".chars() {
            state.search_key(SearchKey::Character(key), &sampled);
        }
        assert_eq!(state.search_matches().len(), 2);
        state.search_key(SearchKey::Escape, &sampled);
        assert!(state.search_matches().is_empty());
    }

    #[test]
    fn highlights_follow_the_text_as_output_moves_it() {
        let sampled = frame(vec![("one ", false), ("two ", false)]);
        let mut state = InteractionState::default();
        state.enter_search(&sampled);
        for key in "one".chars() {
            let matched = state.search_key(SearchKey::Character(key), &sampled);
            state.select_match(matched.unwrap());
        }
        assert_eq!(state.current_match().map(|m| m.start.line), Some(0));

        let scrolled = frame(vec![("two ", false), ("one ", false)]);
        state.refresh_search(&scrolled);

        assert_eq!(state.search_matches().len(), 1);
        let current = state.current_match().unwrap();
        assert_eq!(current.start.line, 1);
        assert_eq!(
            state.selection(),
            Some(Selection::new(current.start, current.end)),
            "the selection marking it moves too"
        );
    }

    #[test]
    fn a_refresh_leaves_a_selection_the_user_made() {
        let sampled = frame(vec![("one ", false), ("two ", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 2,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.enter_search(&sampled);
        state.search_key(SearchKey::Character('o'), &sampled);
        state.begin_selection(hit, LogicalPosition { line: 1, column: 0 });
        state.extend_selection(hit, LogicalPosition { line: 1, column: 2 });
        let made = state.selection();

        state.refresh_search(&frame(vec![("xxxx", false), ("two ", false)]));

        assert_eq!(state.selection(), made);
    }

    #[test]
    fn a_query_with_no_matches_lets_go_of_the_last_one() {
        let sampled = frame(vec![("one ", false)]);
        let mut state = InteractionState::default();
        state.enter_search(&sampled);
        let matched = state.search_key(SearchKey::Character('o'), &sampled);
        state.select_match(matched.unwrap());
        assert!(state.selection().is_some());

        assert_eq!(state.search_key(SearchKey::Character('z'), &sampled), None);

        assert_eq!(state.selection(), None);
        assert_eq!(state.current_match(), None);
        assert!(state.search_matches().is_empty());
    }

    #[test]
    fn starting_over_lets_go_of_the_match_the_last_search_was_on() {
        let sampled = frame(vec![("one ", false)]);
        let mut state = InteractionState::default();
        state.enter_search(&sampled);
        let matched = state.search_key(SearchKey::Character('o'), &sampled);
        state.select_match(matched.unwrap());

        state.enter_search(&sampled);

        assert_eq!(state.selection(), None);
        assert_eq!(state.search().map(LiteralSearch::query), Some(""));
    }

    /// Finds "o" and closes the search again, the way the bar's Close button does.
    fn closed_search_on_a_match(sampled: &TerminalFrame) -> InteractionState {
        let mut state = InteractionState::default();
        state.enter_search(sampled);
        let matched = state.search_key(SearchKey::Character('o'), sampled);
        state.select_match(matched.unwrap());
        state.search_key(SearchKey::Escape, sampled);
        state
    }

    #[test]
    fn closing_a_search_leaves_its_match_selected_to_copy() {
        let sampled = frame(vec![("one ", false)]);
        let state = closed_search_on_a_match(&sampled);
        assert_eq!(state.copy(&sampled).as_deref(), Some("o"));
    }

    #[test]
    fn reopening_a_closed_search_lets_go_of_the_match_it_left_selected() {
        let sampled = frame(vec![("one ", false)]);
        let mut state = closed_search_on_a_match(&sampled);

        state.enter_search(&sampled);
        assert_eq!(state.selection(), None);

        assert_eq!(state.search_key(SearchKey::Character('z'), &sampled), None);
        assert_eq!(state.copy(&sampled), None);
    }

    #[test]
    fn a_selection_made_after_a_closed_search_outlasts_the_next_one() {
        let sampled = frame(vec![("one ", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };
        let mut state = closed_search_on_a_match(&sampled);
        state.begin_selection(hit, LogicalPosition { line: 0, column: 1 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 2 });
        state.end_selection();

        state.enter_search(&sampled);
        state.search_key(SearchKey::Character('z'), &sampled);

        assert_eq!(state.copy(&sampled).as_deref(), Some("ne"));
    }

    #[test]
    fn a_selection_that_happens_to_cover_the_match_is_still_the_users() {
        let sampled = frame(vec![("one ", false)]);
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 4,
        };
        let mut state = InteractionState::default();
        state.enter_search(&sampled);
        for key in "on".chars() {
            let matched = state.search_key(SearchKey::Character(key), &sampled);
            state.select_match(matched.unwrap());
        }
        let on_the_match = state.selection();
        state.begin_selection(hit, LogicalPosition { line: 0, column: 0 });
        state.extend_selection(hit, LogicalPosition { line: 0, column: 1 });
        state.end_selection();
        assert_eq!(state.selection(), on_the_match);

        state.search_key(SearchKey::Character('z'), &sampled);

        assert_eq!(state.selection(), on_the_match);
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
        state.search_key(SearchKey::Character('b'), &sampled);
        assert!(state.current_match().is_some());

        let scrolled = frame(vec![("xxbb", false)]);
        state.viewport_changed(&scrolled);

        assert_eq!(state.copy(&sampled), None);
        assert!(state.current_match().is_none());
        assert_eq!(state.search().map(LiteralSearch::query), Some("b"));
        assert_eq!(
            state.search_matches().len(),
            2,
            "the search stays open and looks at what came into view"
        );
    }

    #[test]
    fn a_scroll_that_does_not_move_the_view_keeps_the_selection_and_navigation() {
        let at_bottom = TerminalFrame {
            viewport_offset: Some(0),
            ..frame(vec![("one ", false), ("one ", false)])
        };
        let mut state = InteractionState::default();
        state.enter_search(&at_bottom);
        state.search_key(SearchKey::Character('o'), &at_bottom);
        let matched = state.search_key(SearchKey::Enter { reverse: false }, &at_bottom);
        state.select_match(matched.unwrap());
        let selection = state.selection();

        assert!(!state.scrolled_from(Some(0), &at_bottom));

        assert_eq!(state.selection(), selection);
        assert_eq!(state.current_match(), matched);
        assert_eq!(
            state.search().and_then(LiteralSearch::current_index),
            Some(1)
        );
    }

    #[test]
    fn a_scroll_that_moves_the_view_lets_go_of_what_was_on_screen() {
        let at_bottom = TerminalFrame {
            viewport_offset: Some(0),
            ..frame(vec![("one ", false)])
        };
        let mut state = InteractionState::default();
        state.enter_search(&at_bottom);
        let matched = state.search_key(SearchKey::Character('o'), &at_bottom);
        state.select_match(matched.unwrap());

        let scrolled = TerminalFrame {
            viewport_offset: Some(1),
            ..at_bottom.clone()
        };
        assert!(state.scrolled_from(Some(0), &scrolled));

        assert_eq!(state.selection(), None);
        assert_eq!(state.current_match(), None);
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
