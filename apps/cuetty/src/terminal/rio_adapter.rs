//! Renderer-neutral snapshots from Rio's public terminal state.
//!
//! The caller holds the terminal lock for this entire operation. Cell text,
//! row-aware styles, viewport state and cursor therefore describe one frame;
//! no side-table IDs escape the lock.

use rio_vt::ansi::CursorShape as RioCursorShape;
use rio_vt::config::colors::{AnsiColor, NamedColor};
use rio_vt::crosswords::Crosswords;
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::square::{ContentTag, Square};
use rio_vt::crosswords::style::{Style, StyleFlags};
use rio_vt::event::EventListener;

use super::model::{
    CellOccupancy, CellWidth, Color, CursorShape, CursorState, Known, Rgb, SamplingToken,
    StyleFlags as TerminalStyleFlags, TerminalCell, TerminalDimensions, TerminalFrame, TerminalRow,
};

/// Samples Rio while the caller holds its terminal lock.
///
/// `sampling_token` belongs to the caller's sampling loop.  Rio's public
/// terminal has no durable content revision, so this function always
/// leaves `stable_revision` unset.
pub(crate) fn snapshot<U: EventListener>(
    terminal: &Crosswords<U>,
    sampling_token: SamplingToken,
) -> TerminalFrame {
    let visible_rows = terminal.visible_rows();
    let dimensions = TerminalDimensions::new(
        visible_rows.len().min(u16::MAX as usize) as u16,
        terminal.columns().min(u16::MAX as usize) as u16,
    );
    let rows = visible_rows
        .iter()
        .take(dimensions.rows as usize)
        .map(|row| row_from_state(terminal, row, dimensions.columns as usize))
        .collect();
    let cursor = terminal.cursor();
    let shape = match cursor.content {
        RioCursorShape::Block => Known::Known(CursorShape::Block),
        RioCursorShape::Underline => Known::Known(CursorShape::Underline),
        RioCursorShape::Beam => Known::Known(CursorShape::Bar),
        // Rio folds DECTCEM and a scrolled viewport into Hidden. Do not
        // invent the shape that would apply when it becomes visible again.
        RioCursorShape::Hidden => Known::Unknown,
    };

    TerminalFrame {
        dimensions,
        rows,
        cursor: CursorState {
            position: u16::try_from(cursor.pos.row.0)
                .ok()
                .zip(u16::try_from(cursor.pos.col.0).ok()),
            shape,
            visible: Known::Known(cursor.content != RioCursorShape::Hidden),
        },
        viewport_offset: Some(terminal.display_offset()),
        sampling_token,
        stable_revision: None,
    }
}

fn row_from_state<U: EventListener>(
    terminal: &Crosswords<U>,
    row: &Row<Square>,
    columns: usize,
) -> TerminalRow {
    let mut styles = Vec::new();
    terminal.grid.resolve_row_styles(row, &mut styles);
    let cells = row
        .inner
        .iter()
        .zip(styles)
        .take(columns)
        .map(|(square, style)| {
            // Background-only cells store their color inline, not in the
            // resolved row style. Reading those bits as style IDs is invalid.
            let style = if square.content_tag() == ContentTag::Codepoint {
                style
            } else {
                terminal.grid.style_of(square)
            };
            square_to_cell(terminal, square, style)
        })
        .collect();
    // Rio records the wrap marker on the physical last cell.  It is public as
    // `Square::wrapline`; no textual or Unicode-width inference is involved.
    let soft_wrapped = columns
        .checked_sub(1)
        .and_then(|column| row.inner.get(column))
        .is_some_and(|square| square.wrapline());

    TerminalRow {
        soft_wrapped,
        // This baseline copies the complete visible frame. Mark every row
        // dirty, including viewport-only changes; incremental damage is an
        // optimization, not a reason to miss repainting scrolled content.
        dirty: true,
        cells,
    }
}

/// Converts one public Rio square.  Keeping this small makes it suitable for
/// focused adapter tests without constructing a live Rio surface.
fn square_to_cell<U: EventListener>(
    terminal: &Crosswords<U>,
    square: &Square,
    style: Style,
) -> TerminalCell {
    let mut text = if square.c() == '\0' {
        " ".to_owned()
    } else {
        square.c().to_string()
    };
    if square.content_tag() == ContentTag::Codepoint
        && square.has_grapheme()
        && let Some(extras) = square
            .extras_id_checked()
            .and_then(|id| terminal.grid.extras_table.get(id))
    {
        text.extend(extras.zerowidth.iter());
    }
    cell_from_observation(SquareObservation {
        text,
        occupancy: occupancy_of(square),
        foreground: semantic_color(style.fg),
        background: semantic_color(style.bg),
        underline_color: style.underline_color.map(semantic_color),
        style: style_of(style.flags),
    })
}

#[derive(Debug, Clone)]
struct SquareObservation {
    text: String,
    occupancy: CellOccupancy,
    foreground: Color,
    background: Color,
    underline_color: Option<Color>,
    style: TerminalStyleFlags,
}

fn cell_from_observation(observation: SquareObservation) -> TerminalCell {
    let (text, width) = match observation.occupancy {
        CellOccupancy::Narrow => (Some(observation.text), CellWidth::Narrow),
        CellOccupancy::Wide => (Some(observation.text), CellWidth::Wide),
        CellOccupancy::LeadingSpacer | CellOccupancy::TrailingSpacer => (None, CellWidth::Narrow),
    };
    TerminalCell {
        text,
        width,
        occupancy: observation.occupancy,
        foreground: observation.foreground,
        background: observation.background,
        underline_color: observation.underline_color,
        style: observation.style,
    }
}

fn occupancy_of(square: &Square) -> CellOccupancy {
    if square.is_wide() {
        CellOccupancy::Wide
    } else if square.is_spacer() {
        CellOccupancy::TrailingSpacer
    } else if square.is_leading_spacer() {
        // Rio calls this a `LeadingSpacer`, although its public docs describe
        // it as the final soft-wrapped row cell before a wide character on the
        // next physical row. It has no following TrailingSpacer; preserve
        // Rio's declared state instead of repairing it from neighbours.
        CellOccupancy::LeadingSpacer
    } else {
        CellOccupancy::Narrow
    }
}

fn semantic_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Spec(rgb) => Color::Rgb(Rgb(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(index) => Color::Indexed(index),
        AnsiColor::Named(named) => match named {
            NamedColor::Background => Color::DefaultBackground,
            NamedColor::Foreground | NamedColor::LightForeground | NamedColor::DimForeground => {
                Color::DefaultForeground
            }
            // Rio's `Cursor` is theme-resolved, while this P0 cell model has
            // no cursor-colour role.  It is not a stored RGB/palette colour;
            // preserve it as a theme-resolved foreground rather than inventing
            // a palette entry.
            NamedColor::Cursor => Color::DefaultForeground,
            NamedColor::Black | NamedColor::DimBlack => Color::Indexed(0),
            NamedColor::Red | NamedColor::DimRed => Color::Indexed(1),
            NamedColor::Green | NamedColor::DimGreen => Color::Indexed(2),
            NamedColor::Yellow | NamedColor::DimYellow => Color::Indexed(3),
            NamedColor::Blue | NamedColor::DimBlue => Color::Indexed(4),
            NamedColor::Magenta | NamedColor::DimMagenta => Color::Indexed(5),
            NamedColor::Cyan | NamedColor::DimCyan => Color::Indexed(6),
            NamedColor::White | NamedColor::DimWhite => Color::Indexed(7),
            NamedColor::LightBlack => Color::Indexed(8),
            NamedColor::LightRed => Color::Indexed(9),
            NamedColor::LightGreen => Color::Indexed(10),
            NamedColor::LightYellow => Color::Indexed(11),
            NamedColor::LightBlue => Color::Indexed(12),
            NamedColor::LightMagenta => Color::Indexed(13),
            NamedColor::LightCyan => Color::Indexed(14),
            NamedColor::LightWhite => Color::Indexed(15),
        },
    }
}

fn style_of(flags: StyleFlags) -> TerminalStyleFlags {
    TerminalStyleFlags {
        bold: flags.contains(StyleFlags::BOLD),
        dim: flags.contains(StyleFlags::DIM),
        italic: flags.contains(StyleFlags::ITALIC),
        inverse: flags.contains(StyleFlags::INVERSE),
        hidden: flags.contains(StyleFlags::HIDDEN),
        strikeout: flags.contains(StyleFlags::STRIKEOUT),
        underline: underline_style(flags),
    }
}

fn underline_style(flags: StyleFlags) -> Option<super::model::UnderlineStyle> {
    use super::model::UnderlineStyle;

    if flags.contains(StyleFlags::UNDERLINE) {
        Some(UnderlineStyle::Single)
    } else if flags.contains(StyleFlags::DOUBLE_UNDERLINE) {
        Some(UnderlineStyle::Double)
    } else if flags.contains(StyleFlags::UNDERCURL) {
        Some(UnderlineStyle::Curly)
    } else if flags.contains(StyleFlags::DOTTED_UNDERLINE) {
        Some(UnderlineStyle::Dotted)
    } else if flags.contains(StyleFlags::DASHED_UNDERLINE) {
        Some(UnderlineStyle::Dashed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rio_vt::crosswords::CrosswordsSize;
    use rio_vt::event::{VoidListener, WindowId};
    use rio_vt::performer::handler::Processor;

    fn terminal() -> Crosswords<VoidListener> {
        Crosswords::new(
            CrosswordsSize::new(8, 3),
            RioCursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            32,
        )
    }

    #[test]
    fn public_parser_snapshot_preserves_clusters_and_wide_occupancy() {
        let mut terminal = terminal();
        Processor::default().advance(&mut terminal, "e\u{301}界".as_bytes());
        let frame = snapshot(&terminal, SamplingToken(1));
        assert_eq!(frame.rows[0].cells[0].text.as_deref(), Some("e\u{301}"));
        assert_eq!(frame.rows[0].cells[1].text.as_deref(), Some("界"));
        assert_eq!(frame.rows[0].cells[1].occupancy, CellOccupancy::Wide);
        assert_eq!(
            frame.rows[0].cells[2].occupancy,
            CellOccupancy::TrailingSpacer
        );
        assert_eq!(frame.rows[0].cells[2].text, None);
        assert!(frame.validate().is_ok());
    }

    #[test]
    fn public_parser_snapshot_observes_cursor_shape_and_visibility() {
        let mut terminal = terminal();
        let mut parser = Processor::default();
        parser.advance(&mut terminal, b"\x1b[6 q");
        let visible = snapshot(&terminal, SamplingToken(1));
        assert_eq!(visible.cursor.shape, Known::Known(CursorShape::Bar));
        assert_eq!(visible.cursor.visible, Known::Known(true));
        parser.advance(&mut terminal, b"\x1b[?25l");
        let hidden = snapshot(&terminal, SamplingToken(2));
        assert_eq!(hidden.cursor.visible, Known::Known(false));
        assert_eq!(hidden.cursor.shape, Known::Unknown);
    }

    #[test]
    fn styles_and_text_are_owned_across_grid_swaps() {
        let mut terminal = terminal();
        let mut parser = Processor::default();
        parser.advance(&mut terminal, b"\x1b[31;1mA\x1b[0m");
        let primary = snapshot(&terminal, SamplingToken(1));
        parser.advance(&mut terminal, b"\x1b[?1049h\x1b[H\x1b[32mB");
        let alternate = snapshot(&terminal, SamplingToken(2));
        assert_eq!(primary.rows[0].cells[0].text.as_deref(), Some("A"));
        assert_eq!(primary.rows[0].cells[0].foreground, Color::Indexed(1));
        assert!(primary.rows[0].cells[0].style.bold);
        assert_eq!(alternate.rows[0].cells[0].text.as_deref(), Some("B"));
        assert_eq!(alternate.rows[0].cells[0].foreground, Color::Indexed(2));
    }

    #[test]
    fn inline_background_cells_do_not_use_background_bits_as_style_ids() {
        let mut terminal = terminal();
        Processor::default().advance(&mut terminal, b"\x1b[48;2;12;34;56m\x1b[2J");
        let frame = snapshot(&terminal, SamplingToken(1));
        assert_eq!(
            frame.rows[0].cells[0].background,
            Color::Rgb(Rgb(12, 34, 56))
        );
        assert_eq!(frame.rows[0].cells[0].text.as_deref(), Some(" "));
        assert!(frame.validate().is_ok());
    }

    #[test]
    fn scrolled_viewport_hides_cursor_and_keeps_offset() {
        let mut terminal = terminal();
        Processor::default().advance(&mut terminal, b"one\r\ntwo\r\nthree\r\nfour");
        terminal.scroll_display(rio_vt::crosswords::grid::Scroll::Delta(1));
        let frame = snapshot(&terminal, SamplingToken(1));
        assert_eq!(frame.viewport_offset, Some(1));
        assert_eq!(frame.cursor.visible, Known::Known(false));
        assert!(frame.rows.iter().all(|row| row.dirty));
        assert!(frame.validate().is_ok());
    }

    fn observation(occupancy: CellOccupancy) -> SquareObservation {
        SquareObservation {
            text: "界".into(),
            occupancy,
            foreground: Color::Indexed(1),
            background: Color::DefaultBackground,
            underline_color: None,
            style: TerminalStyleFlags::default(),
        }
    }

    #[test]
    fn preserves_semantic_colours_and_style_flags() {
        assert_eq!(semantic_color(AnsiColor::Indexed(42)), Color::Indexed(42));
        assert_eq!(
            semantic_color(AnsiColor::Spec(rio_vt::config::colors::ColorRgb {
                r: 1,
                g: 2,
                b: 3
            })),
            Color::Rgb(Rgb(1, 2, 3))
        );
        assert_eq!(
            semantic_color(AnsiColor::Named(NamedColor::Background)),
            Color::DefaultBackground
        );
        let style = style_of(StyleFlags::BOLD | StyleFlags::ITALIC | StyleFlags::DASHED_UNDERLINE);
        assert!(style.bold);
        assert!(style.italic);
        assert_eq!(
            style.underline,
            Some(super::super::model::UnderlineStyle::Dashed)
        );
    }

    #[test]
    fn declared_occupancy_controls_text_and_width_without_unicode_inference() {
        let narrow = cell_from_observation(observation(CellOccupancy::Narrow));
        assert_eq!(narrow.text.as_deref(), Some("界"));
        assert_eq!(narrow.width, CellWidth::Narrow);

        let wide = cell_from_observation(observation(CellOccupancy::Wide));
        assert_eq!(wide.text.as_deref(), Some("界"));
        assert_eq!(wide.width, CellWidth::Wide);

        for occupancy in [CellOccupancy::LeadingSpacer, CellOccupancy::TrailingSpacer] {
            let spacer = cell_from_observation(observation(occupancy));
            assert_eq!(spacer.text, None);
            assert_eq!(spacer.width, CellWidth::Narrow);
            assert_eq!(spacer.occupancy, occupancy);
        }
    }

    #[test]
    fn wrapped_wide_boundary_fixture_preserves_rio_end_of_row_placeholder() {
        let frame = TerminalFrame {
            dimensions: TerminalDimensions::new(2, 3),
            rows: vec![
                TerminalRow {
                    // Rio records this on the final physical square, which is
                    // also its LeadingSpacer when a wide glyph wraps.
                    soft_wrapped: true,
                    dirty: true,
                    cells: vec![
                        cell_from_observation(observation(CellOccupancy::Narrow)),
                        cell_from_observation(observation(CellOccupancy::Narrow)),
                        cell_from_observation(observation(CellOccupancy::LeadingSpacer)),
                    ],
                },
                TerminalRow {
                    soft_wrapped: false,
                    dirty: true,
                    cells: vec![
                        cell_from_observation(observation(CellOccupancy::Wide)),
                        cell_from_observation(observation(CellOccupancy::TrailingSpacer)),
                        cell_from_observation(observation(CellOccupancy::Narrow)),
                    ],
                },
            ],
            cursor: CursorState {
                position: Some((1, 2)),
                shape: Known::Unknown,
                visible: Known::Unknown,
            },
            viewport_offset: Some(0),
            sampling_token: SamplingToken(1),
            stable_revision: None,
        };

        assert_eq!(frame.rows[0].cells[2].text, None);
        assert!(frame.validate().is_ok());
    }

    #[test]
    fn rio_capabilities_do_not_claim_unobservable_state() {
        let capabilities = super::super::model::TerminalCapabilities::rio_frame();
        assert_eq!(
            capabilities.cursor_shape,
            super::super::model::Capability::Supported
        );
        assert_eq!(
            capabilities.cursor_visibility,
            super::super::model::Capability::Supported
        );
        assert_eq!(
            capabilities.stable_content_revision,
            super::super::model::Capability::Unsupported
        );
        assert_eq!(
            capabilities.scrollback,
            super::super::model::Capability::Unsupported
        );
        assert_eq!(
            capabilities.title_actions,
            super::super::model::Capability::Unsupported
        );
        assert_eq!(
            capabilities.bell_actions,
            super::super::model::Capability::Unsupported
        );
    }
}
