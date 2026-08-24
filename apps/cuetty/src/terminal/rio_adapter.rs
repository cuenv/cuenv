//! Safe translation from Rio's public render snapshot into the terminal model.
//!
//! This module is the only new-model boundary which mentions `librio`.  It
//! intentionally samples an already-updated `RenderState`; it neither reaches
//! into Rio's private grid nor assigns meaning to state Rio does not expose.

use librio::{AnsiColor, NamedColor, RenderState, Square, StyleFlags};

use super::model::{
    CellOccupancy, CellWidth, Color, CursorState, Known, Rgb, SamplingToken,
    StyleFlags as TerminalStyleFlags, TerminalCell, TerminalDimensions, TerminalFrame, TerminalRow,
};

/// Samples a Rio render snapshot into the renderer-neutral cell protocol.
///
/// `sampling_token` belongs to the caller's sampling loop.  Rio's public
/// `RenderState` has no durable content revision, so this function always
/// leaves `stable_revision` unset.
pub(crate) fn snapshot(state: &RenderState, sampling_token: SamplingToken) -> TerminalFrame {
    let dimensions = TerminalDimensions::new(
        state.lines().min(u16::MAX as usize) as u16,
        state.columns().min(u16::MAX as usize) as u16,
    );
    let rows = (0..dimensions.rows as usize)
        .map(|line| row_from_state(state, line, dimensions.columns as usize))
        .collect();
    let (line, column) = state.cursor();

    TerminalFrame {
        dimensions,
        rows,
        cursor: CursorState {
            position: Some((
                line.min(u16::MAX as usize) as u16,
                column.min(u16::MAX as usize) as u16,
            )),
            // The public snapshot exposes position only.  Do not convert a
            // renderer default or blinking event into a visibility/shape fact.
            shape: Known::Unknown,
            visible: Known::Unknown,
        },
        viewport_offset: Some(state.display_offset()),
        sampling_token,
        stable_revision: None,
    }
}

fn row_from_state(state: &RenderState, line: usize, columns: usize) -> TerminalRow {
    let cells = (0..columns)
        .map(|column| {
            let square = state.square(line, column).copied().unwrap_or_default();
            square_to_cell(state, &square)
        })
        .collect();
    // Rio records the wrap marker on the physical last cell.  It is public as
    // `Square::wrapline`; no textual or Unicode-width inference is involved.
    let soft_wrapped = columns
        .checked_sub(1)
        .and_then(|column| state.square(line, column))
        .is_some_and(|square| square.wrapline());

    TerminalRow {
        soft_wrapped,
        dirty: state.row_dirty(line),
        cells,
    }
}

/// Converts one public Rio square.  Keeping this small makes it suitable for
/// focused adapter tests without constructing a live Rio surface.
pub(crate) fn square_to_cell(state: &RenderState, square: &Square) -> TerminalCell {
    let style = state.style_of(square);
    cell_from_observation(SquareObservation {
        codepoint: square.c(),
        occupancy: occupancy_of(square),
        foreground: semantic_color(style.fg),
        background: semantic_color(style.bg),
        underline_color: style.underline_color.map(semantic_color),
        style: style_of(style.flags),
    })
}

#[derive(Debug, Clone, Copy)]
struct SquareObservation {
    codepoint: char,
    occupancy: CellOccupancy,
    foreground: Color,
    background: Color,
    underline_color: Option<Color>,
    style: TerminalStyleFlags,
}

fn cell_from_observation(observation: SquareObservation) -> TerminalCell {
    let (codepoint, width) = match observation.occupancy {
        CellOccupancy::Narrow => (
            Some(if observation.codepoint == '\0' {
                ' '
            } else {
                observation.codepoint
            }),
            CellWidth::Narrow,
        ),
        CellOccupancy::Wide => (Some(observation.codepoint), CellWidth::Wide),
        CellOccupancy::LeadingSpacer | CellOccupancy::TrailingSpacer => (None, CellWidth::Narrow),
    };
    TerminalCell {
        codepoint,
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

    fn observation(occupancy: CellOccupancy) -> SquareObservation {
        SquareObservation {
            codepoint: '界',
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
            semantic_color(AnsiColor::Spec(librio::ColorRgb { r: 1, g: 2, b: 3 })),
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
        assert_eq!(narrow.codepoint, Some('界'));
        assert_eq!(narrow.width, CellWidth::Narrow);

        let wide = cell_from_observation(observation(CellOccupancy::Wide));
        assert_eq!(wide.codepoint, Some('界'));
        assert_eq!(wide.width, CellWidth::Wide);

        for occupancy in [CellOccupancy::LeadingSpacer, CellOccupancy::TrailingSpacer] {
            let spacer = cell_from_observation(observation(occupancy));
            assert_eq!(spacer.codepoint, None);
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

        assert_eq!(frame.rows[0].cells[2].codepoint, None);
        assert!(frame.validate().is_ok());
    }

    #[test]
    fn rio_capabilities_do_not_claim_unobservable_state() {
        let capabilities = super::super::model::TerminalCapabilities::rio_frame();
        assert_eq!(
            capabilities.cursor_shape,
            super::super::model::Capability::Unsupported
        );
        assert_eq!(
            capabilities.cursor_visibility,
            super::super::model::Capability::Unsupported
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
