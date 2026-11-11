//! Pointer selections in Rio's scrollback, projected onto the visible frame.

use std::cmp::Ordering;
use std::ops::RangeInclusive;

use rio_vt::crosswords::Crosswords;
use rio_vt::crosswords::pos::{Column, Line, Pos, Side};
use rio_vt::crosswords::square::{ContentTag, Wide};
use rio_vt::event::EventListener;
use rio_vt::selection::{Selection as RioSelection, SelectionType};

use super::selection::{LogicalPosition, Selection};

/// Rio rotates its selection as output scrolls. Once a drag has a range, its
/// original end can be read back from that range instead of tracking a second
/// copy of the terminal's history.
pub(super) struct SelectionDrag {
    direction: Option<Ordering>,
}

fn grid_position<T: EventListener>(terminal: &Crosswords<T>, position: LogicalPosition) -> Pos {
    let row = i32::try_from(position.line).unwrap_or(i32::MAX)
        - i32::try_from(terminal.display_offset()).unwrap_or(i32::MAX);
    Pos::new(
        Line(row),
        Column(position.column.min(terminal.columns().saturating_sub(1))),
    )
}

impl SelectionDrag {
    pub fn begin<T: EventListener>(
        terminal: &mut Crosswords<T>,
        position: LogicalPosition,
    ) -> Self {
        let anchor = grid_position(terminal, position);
        terminal.selection = Some(single_cell(anchor));
        Self { direction: None }
    }

    pub fn is_empty(&self) -> bool {
        self.direction.is_none()
    }

    /// Returns false when Rio has invalidated the selection, for example on reflow.
    pub fn extend<T: EventListener>(
        &mut self,
        terminal: &mut Crosswords<T>,
        position: LogicalPosition,
    ) -> bool {
        let Some(selection) = terminal.selection.as_ref() else {
            return false;
        };
        let Some(range) = selection.to_range(terminal) else {
            return false;
        };
        let anchor = if self.direction == Some(Ordering::Greater) {
            range.end
        } else {
            range.start
        };
        let active = grid_position(terminal, position);
        self.direction = match (anchor.row.0, anchor.col.0).cmp(&(active.row.0, active.col.0)) {
            Ordering::Equal => {
                terminal.selection = Some(single_cell(anchor));
                None
            }
            direction => {
                let mut selection = RioSelection::new(SelectionType::Simple, anchor, Side::Left);
                selection.update(active, Side::Left);
                selection.include_all();
                terminal.selection = Some(selection);
                Some(direction)
            }
        };
        true
    }
}

// An unpublished one-cell range lets Rio rotate the press anchor even before
// the pointer moves. The session hides and discards it for an ordinary click.
fn single_cell(anchor: Pos) -> RioSelection {
    let mut selection = RioSelection::new(SelectionType::Simple, anchor, Side::Left);
    selection.update(anchor, Side::Right);
    selection
}

/// Rio knows the current tab stops. Use its export to identify which empty
/// cells after a tab are its visual expansion and which are later spacing.
fn tab_expansion_end<T: EventListener>(
    terminal: &Crosswords<T>,
    row: Line,
    column: usize,
) -> usize {
    let cells = &terminal.grid[row];
    let columns = terminal.columns();
    let next_content =
        (column + 1..columns).find(|index| !matches!(cells[Column(*index)].c(), '\0' | ' '));
    if let Some(next) = next_content {
        let probe =
            terminal.bounds_to_string(Pos::new(row, Column(column)), Pos::new(row, Column(next)));
        let spaces = probe.strip_prefix('\t').map_or(0, |text| {
            text.chars()
                .take_while(|character| *character == ' ')
                .count()
        });
        return next.saturating_sub(spaces).max(column + 1);
    }
    (column + 1..columns)
        .find(|index| cells[Column(*index)].c() != '\0')
        .unwrap_or(columns)
}

/// Clip an inclusive Rio range to the viewport and return a half-open overlay.
pub(super) fn visible_selection<T: EventListener>(terminal: &Crosswords<T>) -> Option<Selection> {
    let range = terminal.selection.as_ref()?.to_range(terminal)?;
    let offset = i64::try_from(terminal.display_offset()).ok()?;
    let rows = i64::try_from(terminal.screen_lines()).ok()?;
    if rows == 0 {
        return None;
    }
    let start = i64::from(range.start.row.0) + offset;
    let end = i64::from(range.end.row.0) + offset;
    if end < 0 || start >= rows {
        return None;
    }
    Some(Selection::new(
        LogicalPosition {
            line: start.max(0) as usize,
            column: if start < 0 { 0 } else { range.start.col.0 },
        },
        LogicalPosition {
            line: end.min(rows - 1) as usize,
            column: if end >= rows {
                terminal.columns()
            } else {
                range.end.col.0.saturating_add(1).min(terminal.columns())
            },
        },
    ))
}

/// Copy selected cells, keeping explicit spaces and blank lines while omitting
/// unoccupied padding at hard line breaks.
pub(super) fn selection_text<T: EventListener>(terminal: &Crosswords<T>) -> Option<String> {
    let range = terminal.selection.as_ref()?.to_range(terminal)?;
    text_between(terminal, range.start..=range.end)
}

/// Where the last command's output starts and where its last occupied cell is,
/// from the OSC 133 marks its shell left. `None` when the shell left none or the
/// command printed nothing. Blank cells and rows after the output are not part of
/// it, which leaves out the cursor's row while a command is between lines.
fn last_command_output_span<T: EventListener>(
    terminal: &Crosswords<T>,
) -> Option<RangeInclusive<Pos>> {
    let rows = terminal.last_command_output()?;
    let columns = terminal.columns();
    let end = (rows.start().0..=rows.end().0).rev().find_map(|row| {
        let cells = &terminal.grid[Line(row)];
        (0..columns)
            .rev()
            .find(|column| cells[Column(*column)].c() != '\0')
            .map(|column| Pos::new(Line(row), Column(column)))
    })?;
    Some(Pos::new(*rows.start(), Column(0))..=end)
}

/// Whether the last command left output to copy, without copying it.
pub(super) fn has_last_command_output<T: EventListener>(terminal: &Crosswords<T>) -> bool {
    last_command_output_span(terminal).is_some()
}

/// What the last command printed, running or finished, as selecting it would copy it.
pub(super) fn last_command_output_text<T: EventListener>(
    terminal: &Crosswords<T>,
) -> Option<String> {
    text_between(terminal, last_command_output_span(terminal)?)
}

/// Copy the cells from one position through another, keeping explicit spaces and
/// blank lines while omitting unoccupied padding at hard line breaks.
fn text_between<T: EventListener>(
    terminal: &Crosswords<T>,
    span: RangeInclusive<Pos>,
) -> Option<String> {
    let (from, to) = span.into_inner();
    let columns = terminal.columns();
    if columns == 0 {
        return None;
    }
    let mut text = String::new();
    for row in from.row.0..=to.row.0 {
        let cells = &terminal.grid[Line(row)];
        let mut first = if row == from.row.0 { from.col.0 } else { 0 };
        let mut end = if row == to.row.0 {
            to.col.0.saturating_add(1).min(columns)
        } else {
            columns
        };
        if first > 0
            && first < columns
            && cells[Column(first)].wide() == Wide::Spacer
            && cells[Column(first - 1)].wide() == Wide::Wide
        {
            first -= 1;
        }
        let wrapped = cells[Column(columns - 1)].wrapline();
        if row != to.row.0 && !wrapped {
            while end > first && cells[Column(end - 1)].c() == '\0' {
                end -= 1;
            }
        }
        let mut tab_end = first;
        for column in first..end {
            if column < tab_end {
                continue;
            }
            let cell = &cells[Column(column)];
            if matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer) {
                continue;
            }
            let character = cell.c();
            text.push(if character == '\0' { ' ' } else { character });
            if character == '\t' {
                tab_end = tab_expansion_end(terminal, Line(row), column);
            }
            if cell.content_tag() == ContentTag::Codepoint
                && cell.has_grapheme()
                && let Some(extras) = cell
                    .extras_id_checked()
                    .and_then(|id| terminal.grid.extras_table.get(id))
            {
                text.extend(extras.zerowidth.iter());
            }
        }
        if row != to.row.0 && !wrapped {
            text.push('\n');
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rio_vt::ansi::CursorShape;
    use rio_vt::crosswords::CrosswordsSize;
    use rio_vt::crosswords::grid::Scroll;
    use rio_vt::event::{VoidListener, WindowId};
    use rio_vt::performer::handler::Processor;

    fn terminal() -> Crosswords<VoidListener> {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(8, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(
            &mut terminal,
            b"000\r\n111\r\n222\r\n333\r\n444\r\n555\r\n666",
        );
        terminal
    }

    /// A 20x4 terminal that has been sent `bytes`.
    fn session(bytes: &[u8]) -> Crosswords<VoidListener> {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(20, 4),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            64,
        );
        Processor::default().advance(&mut terminal, bytes);
        terminal
    }

    #[test]
    fn the_last_output_is_what_the_last_finished_command_printed() {
        // A prompt of two rows, whose second row carries no mark of its own.
        let terminal = session(
            b"\x1b]133;A\x07~/src\r\n> \x1b]133;B\x07ls\r\n\
              \x1b]133;C\x07first\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07~/src\r\n> \x1b]133;B\x07cat notes\r\n\
              \x1b]133;C\x07one  two\r\n\r\nthree\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07~/src\r\n> ",
        );
        assert!(has_last_command_output(&terminal));
        assert_eq!(
            last_command_output_text(&terminal).as_deref(),
            Some("one  two\n\nthree"),
        );
    }

    #[test]
    fn the_last_output_of_a_running_command_stops_at_what_it_has_printed() {
        let terminal = session(
            b"\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\
              \x1b]133;C\x07step 1\r\nstep 2\r\n",
        );
        assert_eq!(
            last_command_output_text(&terminal).as_deref(),
            Some("step 1\nstep 2"),
        );
    }

    #[test]
    fn the_last_output_joins_soft_wrapped_rows_and_reaches_into_history() {
        let terminal = session(
            b"\x1b]133;A\x07$ \x1b]133;B\x07seq\r\n\
              \x1b]133;C\x07a line that wraps past twenty columns\r\n\
              2\r\n3\r\n4\r\n5\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07$ ",
        );
        assert!(terminal.history_size() > 0);
        assert_eq!(
            last_command_output_text(&terminal).as_deref(),
            Some("a line that wraps past twenty columns\n2\n3\n4\n5"),
        );
    }

    #[test]
    fn there_is_no_last_output_without_marks_or_without_anything_printed() {
        let unmarked = session(b"$ ls\r\nfile\r\n$ ");
        assert!(!has_last_command_output(&unmarked));
        assert_eq!(last_command_output_text(&unmarked), None);

        let silent = session(
            b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\
              \x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ ",
        );
        assert!(!has_last_command_output(&silent));
        assert_eq!(last_command_output_text(&silent), None);

        // A command that has started and printed nothing yet.
        let waiting = session(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07");
        assert!(!has_last_command_output(&waiting));
    }

    #[test]
    fn the_alternate_screen_has_no_last_output() {
        let terminal = session(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\
              \x1b]133;C\x07file\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07vi\r\n\
              \x1b]133;C\x07\x1b[?1049hbuffer",
        );
        assert!(!has_last_command_output(&terminal));
    }

    fn at(line: usize, column: usize) -> LogicalPosition {
        LogicalPosition { line, column }
    }

    #[test]
    fn drag_through_history_in_both_directions_copies_the_same_text() {
        let expected = "000\n111\n222\n333\n444\n555\n666";
        let mut terminal = terminal();
        let mut drag = SelectionDrag::begin(&mut terminal, at(2, 2));
        for _ in 0..4 {
            terminal.scroll_display(Scroll::Delta(1));
            assert!(drag.extend(&mut terminal, at(0, 0)));
        }
        assert_eq!(selection_text(&terminal).as_deref(), Some(expected));
        assert_eq!(
            visible_selection(&terminal),
            Some(Selection::new(at(0, 0), at(2, 8))),
        );

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        for _ in 0..4 {
            terminal.scroll_display(Scroll::Delta(-1));
            assert!(drag.extend(&mut terminal, at(2, 2)));
        }
        assert_eq!(selection_text(&terminal).as_deref(), Some(expected));
        assert_eq!(
            visible_selection(&terminal),
            Some(Selection::new(at(0, 0), at(2, 3))),
        );
    }

    #[test]
    fn click_and_return_to_origin_leave_no_selection() {
        let mut terminal = terminal();
        let mut drag = SelectionDrag::begin(&mut terminal, at(1, 1));
        assert!(drag.is_empty());
        assert!(drag.extend(&mut terminal, at(0, 0)));
        assert!(drag.extend(&mut terminal, at(1, 1)));
        assert!(drag.is_empty());
        assert!(drag.extend(&mut terminal, at(2, 2)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("55\n666"));
    }

    #[test]
    fn output_keeps_a_history_anchor_and_reflow_invalidates_it() {
        let mut terminal = terminal();
        terminal.scroll_display(Scroll::Top);
        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(1, 2)));
        Processor::default().advance(&mut terminal, b"\r\n777");
        assert!(drag.extend(&mut terminal, at(2, 2)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("000\n111\n222"));

        terminal.resize(CrosswordsSize::new(6, 3));
        assert!(!drag.extend(&mut terminal, at(1, 1)));
        assert_eq!(visible_selection(&terminal), None);
    }

    #[test]
    fn an_initial_anchor_follows_output_scrolling_a_partial_region() {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(8, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"000\r\n111\r\n222");
        let mut drag = SelectionDrag::begin(&mut terminal, at(2, 2));
        processor.advance(&mut terminal, b"\x1b[2;3r\x1b[3;1H\n");
        assert!(drag.extend(&mut terminal, at(0, 0)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("000\n222"));
    }

    #[test]
    fn copy_keeps_wide_cells_graphemes_and_soft_wraps() {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(4, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(&mut terminal, "a界e\u{301}z".as_bytes());
        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 2));
        assert!(drag.extend(&mut terminal, at(0, 1)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("界"));

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(1, 0)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("a界e\u{301}z"));
    }

    #[test]
    fn copy_preserves_selected_spaces_and_blank_lines() {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(4, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(&mut terminal, b"ab  \r\n\r\nz");
        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 2));
        assert!(drag.extend(&mut terminal, at(0, 3)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("  "));

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(2, 0)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("ab  \n\nz"));
    }

    #[test]
    fn copy_tabs_excludes_expansion_but_preserves_selected_text_and_spaces() {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(16, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(&mut terminal, b"a\tb  ");
        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(0, 10)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("a\tb  "));

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 1));
        assert!(drag.extend(&mut terminal, at(0, 6)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("\t"));

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 2));
        assert!(drag.extend(&mut terminal, at(0, 5)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("    "));
    }

    #[test]
    fn copy_tabs_respects_custom_stops_and_later_cursor_movement() {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(16, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(
            &mut terminal,
            b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;1Ha\t\x1b[1;9Hb",
        );
        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(0, 8)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("a\t    b"));

        let mut drag = SelectionDrag::begin(&mut terminal, at(0, 0));
        assert!(drag.extend(&mut terminal, at(0, 5)));
        assert_eq!(selection_text(&terminal).as_deref(), Some("a\t  "));
    }

    #[test]
    fn visible_copy_matches_the_frame_selection_contract() {
        use crate::terminal::interaction::FrameTextSource;
        use crate::terminal::model::SamplingToken;
        use crate::terminal::rio_adapter;

        let mut terminal = Crosswords::new(
            CrosswordsSize::new(4, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        );
        Processor::default().advance(&mut terminal, "a界e\u{301}z\r\n".as_bytes());
        let frame = rio_adapter::snapshot(&terminal, SamplingToken(0));
        let source = FrameTextSource::new(&frame);
        for first in 0..12 {
            for second in 0..12 {
                if first == second {
                    continue;
                }
                let (low, high) = if first < second {
                    (first, second)
                } else {
                    (second, first)
                };
                if (low / 4..high / 4).any(|row| !frame.rows[row].soft_wrapped) {
                    continue;
                }
                let expected =
                    Selection::new(at(low / 4, low % 4), at(high / 4, high % 4 + 1)).copy(&source);
                let mut drag = SelectionDrag::begin(&mut terminal, at(first / 4, first % 4));
                assert!(drag.extend(&mut terminal, at(second / 4, second % 4)));
                assert_eq!(
                    selection_text(&terminal).as_deref(),
                    Some(expected.as_str()),
                    "{first} to {second}"
                );
            }
        }
    }
}
