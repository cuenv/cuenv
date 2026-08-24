//! Renderer- and backend-neutral terminal interchange types.
//!
//! This is deliberately a *cell* protocol. A content cell carries one Unicode
//! scalar value, not a grapheme cluster; combining clusters and hyperlink
//! destinations therefore remain explicitly unsupported by this P0 contract.

use super::input::TerminalKeyEvent;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A colour whose final value is resolved by the renderer/theme, never by a
/// backend-specific colour type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    DefaultForeground,
    DefaultBackground,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StyleFlags {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikeout: bool,
    pub underline: Option<UnderlineStyle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineStyle {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// The display width of the scalar held by a content cell. It is not Unicode
/// width inference: the backend declares it and the renderer preserves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidth {
    Narrow,
    Wide,
}

/// How this physical grid slot participates in a row.
///
/// `Wide` starts a two-column scalar and must be followed by a
/// `TrailingSpacer`. `LeadingSpacer` is an end-of-row placeholder for a wide
/// scalar which Rio moved onto the next physical row; it is valid only in the
/// final column of a soft-wrapped row and has no following `TrailingSpacer`.
/// Spacers never carry text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellOccupancy {
    Narrow,
    Wide,
    LeadingSpacer,
    TrailingSpacer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    /// One Unicode scalar value. `None` is reserved for spacer cells.
    pub codepoint: Option<char>,
    pub width: CellWidth,
    pub occupancy: CellOccupancy,
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Option<Color>,
    pub style: StyleFlags,
}

impl TerminalCell {
    pub fn narrow(codepoint: char) -> Self {
        Self {
            codepoint: Some(codepoint),
            width: CellWidth::Narrow,
            occupancy: CellOccupancy::Narrow,
            foreground: Color::DefaultForeground,
            background: Color::DefaultBackground,
            underline_color: None,
            style: StyleFlags::default(),
        }
    }

    pub fn wide(codepoint: char) -> Self {
        Self {
            width: CellWidth::Wide,
            occupancy: CellOccupancy::Wide,
            ..Self::narrow(codepoint)
        }
    }

    pub fn trailing_spacer() -> Self {
        Self {
            codepoint: None,
            width: CellWidth::Narrow,
            occupancy: CellOccupancy::TrailingSpacer,
            ..Self::narrow(' ')
        }
    }

    pub fn leading_spacer() -> Self {
        Self {
            codepoint: None,
            width: CellWidth::Narrow,
            occupancy: CellOccupancy::LeadingSpacer,
            ..Self::narrow(' ')
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    pub soft_wrapped: bool,
    pub dirty: bool,
    pub cells: Vec<TerminalCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalDimensions {
    pub rows: u16,
    pub columns: u16,
}

impl TerminalDimensions {
    pub const fn new(rows: u16, columns: u16) -> Self {
        Self { rows, columns }
    }
}

/// A monotonically increasing token assigned when a backend is sampled. Equal
/// content can have different tokens; it is not a content revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SamplingToken(pub u64);

/// An optional backend-provided durable content revision. It is absent when a
/// backend exposes no stable revision, as is the case for the pinned Rio API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known<T> {
    Known(T),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

/// Position is host-known when present. Shape and visibility are independent:
/// unknown backend state must not be silently rendered as a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    pub position: Option<(u16, u16)>,
    pub shape: Known<CursorShape>,
    pub visible: Known<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFrame {
    pub dimensions: TerminalDimensions,
    pub rows: Vec<TerminalRow>,
    pub cursor: CursorState,
    pub viewport_offset: Option<usize>,
    pub sampling_token: SamplingToken,
    pub stable_revision: Option<StableRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    RowCount {
        expected: usize,
        actual: usize,
    },
    ColumnCoverage {
        row: usize,
        expected: usize,
        actual: usize,
    },
    InvalidCell {
        row: usize,
        column: usize,
        reason: &'static str,
    },
}

impl TerminalFrame {
    /// Validates physical grid coverage and renderer-neutral occupancy. A
    /// `Wide` cell owns its following `TrailingSpacer`; a `LeadingSpacer` is
    /// instead the final-cell wrap marker for a soft-wrapped physical row. It
    /// does not attempt grapheme-cluster validation or derive widths from text.
    pub fn validate(&self) -> Result<(), FrameError> {
        if self.rows.len() != self.dimensions.rows as usize {
            return Err(FrameError::RowCount {
                expected: self.dimensions.rows as usize,
                actual: self.rows.len(),
            });
        }
        for (row_index, row) in self.rows.iter().enumerate() {
            if row.cells.len() != self.dimensions.columns as usize {
                return Err(FrameError::ColumnCoverage {
                    row: row_index,
                    expected: self.dimensions.columns as usize,
                    actual: row.cells.len(),
                });
            }
            for (column, cell) in row.cells.iter().enumerate() {
                match cell.occupancy {
                    CellOccupancy::Narrow
                        if cell.codepoint.is_none() || cell.width != CellWidth::Narrow =>
                    {
                        return Err(FrameError::InvalidCell {
                            row: row_index,
                            column,
                            reason: "narrow cells require one narrow codepoint",
                        });
                    }
                    CellOccupancy::Wide
                        if cell.codepoint.is_none() || cell.width != CellWidth::Wide =>
                    {
                        return Err(FrameError::InvalidCell {
                            row: row_index,
                            column,
                            reason: "wide cells require one wide codepoint",
                        });
                    }
                    CellOccupancy::LeadingSpacer | CellOccupancy::TrailingSpacer
                        if cell.codepoint.is_some() || cell.width != CellWidth::Narrow =>
                    {
                        return Err(FrameError::InvalidCell {
                            row: row_index,
                            column,
                            reason: "spacer cells carry no text and occupy one grid slot",
                        });
                    }
                    _ => {}
                }
                if matches!(cell.occupancy, CellOccupancy::Wide)
                    && !matches!(
                        row.cells.get(column + 1).map(|next| next.occupancy),
                        Some(CellOccupancy::TrailingSpacer)
                    )
                {
                    return Err(FrameError::InvalidCell {
                        row: row_index,
                        column,
                        reason: "wide starts require a trailing spacer",
                    });
                }
                if matches!(cell.occupancy, CellOccupancy::LeadingSpacer)
                    && (column + 1 != row.cells.len() || !row.soft_wrapped)
                {
                    return Err(FrameError::InvalidCell {
                        row: row_index,
                        column,
                        reason: "leading spacers require the soft-wrapped end of a row",
                    });
                }
                if matches!(cell.occupancy, CellOccupancy::TrailingSpacer)
                    && !matches!(
                        column
                            .checked_sub(1)
                            .and_then(|index| row.cells.get(index))
                            .map(|previous| previous.occupancy),
                        Some(CellOccupancy::Wide)
                    )
                {
                    return Err(FrameError::InvalidCell {
                        row: row_index,
                        column,
                        reason: "trailing spacers require a wide start",
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Supported,
    Unsupported,
    Unknown,
}

/// Capabilities are declarations about the adapter's safe public contract, not
/// guesses about what a terminal backend might support internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub wide_cell_occupancy: Capability,
    pub soft_wrap: Capability,
    pub row_dirtiness: Capability,
    pub viewport_offset: Capability,
    pub scrollback: Capability,
    pub title_actions: Capability,
    pub bell_actions: Capability,
    pub combining_clusters: Capability,
    pub hyperlink_destinations: Capability,
    pub cursor_shape: Capability,
    pub cursor_visibility: Capability,
    pub bracketed_paste_mode: Capability,
    pub mouse_reporting_mode: Capability,
    pub stable_content_revision: Capability,
}

impl TerminalCapabilities {
    /// Capabilities supplied by a single sampled `RenderState` frame.
    ///
    /// A frame contains viewport metadata and dirty rows, but it cannot
    /// perform scrolling or emit session-level effects by itself.
    pub const fn rio_frame() -> Self {
        Self {
            wide_cell_occupancy: Capability::Supported,
            soft_wrap: Capability::Supported,
            row_dirtiness: Capability::Supported,
            viewport_offset: Capability::Supported,
            scrollback: Capability::Unsupported,
            title_actions: Capability::Unsupported,
            bell_actions: Capability::Unsupported,
            combining_clusters: Capability::Unsupported,
            hyperlink_destinations: Capability::Unsupported,
            cursor_shape: Capability::Unsupported,
            cursor_visibility: Capability::Unsupported,
            bracketed_paste_mode: Capability::Unsupported,
            mouse_reporting_mode: Capability::Unsupported,
            stable_content_revision: Capability::Unsupported,
        }
    }

    /// Capabilities of the complete pinned Rio session adapter.
    pub const fn rio_pinned() -> Self {
        Self {
            wide_cell_occupancy: Capability::Supported,
            soft_wrap: Capability::Supported,
            row_dirtiness: Capability::Supported,
            viewport_offset: Capability::Supported,
            scrollback: Capability::Supported,
            title_actions: Capability::Supported,
            bell_actions: Capability::Supported,
            combining_clusters: Capability::Unsupported,
            hyperlink_destinations: Capability::Unsupported,
            cursor_shape: Capability::Unsupported,
            cursor_visibility: Capability::Unsupported,
            bracketed_paste_mode: Capability::Unsupported,
            mouse_reporting_mode: Capability::Unsupported,
            stable_content_revision: Capability::Unsupported,
        }
    }

    fn mode_support(&self, mode: TerminalMode) -> Capability {
        match mode {
            TerminalMode::BracketedPaste => self.bracketed_paste_mode,
            TerminalMode::MouseReporting => self.mouse_reporting_mode,
        }
    }
}

/// Only modes which an adapter has observed or explicitly declared may enter
/// this collection. Unknown and unsupported modes are rejected, not inferred.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalModes {
    declared: BTreeMap<TerminalMode, bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TerminalMode {
    BracketedPaste,
    MouseReporting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeError {
    NotObservable(TerminalMode),
}

impl TerminalModes {
    pub fn declare(
        &mut self,
        capabilities: &TerminalCapabilities,
        mode: TerminalMode,
        enabled: bool,
    ) -> Result<(), ModeError> {
        if capabilities.mode_support(mode) != Capability::Supported {
            return Err(ModeError::NotObservable(mode));
        }
        self.declared.insert(mode, enabled);
        Ok(())
    }
    pub fn get(&self, mode: TerminalMode) -> Option<bool> {
        self.declared.get(&mode).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandOrder(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCommandKind {
    Key(TerminalKeyEvent),
    Text(String),
    Paste(String),
    Scroll {
        lines: i32,
    },
    Mouse {
        column: u16,
        row: u16,
        button: u8,
        pressed: bool,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCommand {
    pub order: CommandOrder,
    pub kind: TerminalCommandKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEffectKind {
    ClipboardWrite(String),
    Title(String),
    Bell,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEffect {
    pub order: CommandOrder,
    pub kind: TerminalEffectKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(cells: Vec<TerminalCell>) -> TerminalFrame {
        TerminalFrame {
            dimensions: TerminalDimensions::new(1, cells.len() as u16),
            rows: vec![TerminalRow {
                soft_wrapped: false,
                dirty: true,
                cells,
            }],
            cursor: CursorState {
                position: Some((0, 0)),
                shape: Known::Unknown,
                visible: Known::Unknown,
            },
            viewport_offset: Some(0),
            sampling_token: SamplingToken(1),
            stable_revision: None,
        }
    }

    #[test]
    fn rows_must_cover_the_declared_grid() {
        let mut candidate = frame(vec![TerminalCell::narrow('a')]);
        candidate.dimensions.columns = 2;
        assert_eq!(
            candidate.validate(),
            Err(FrameError::ColumnCoverage {
                row: 0,
                expected: 2,
                actual: 1
            })
        );
    }

    #[test]
    fn wide_occupancy_cannot_run_past_the_row() {
        let candidate = frame(vec![TerminalCell::wide('界')]);
        assert!(matches!(
            candidate.validate(),
            Err(FrameError::InvalidCell {
                reason: "wide starts require a trailing spacer",
                ..
            })
        ));
    }

    #[test]
    fn continuation_cells_follow_the_declared_rules() {
        assert!(
            frame(vec![
                TerminalCell::wide('界'),
                TerminalCell::trailing_spacer()
            ])
            .validate()
            .is_ok()
        );
        assert!(matches!(
            frame(vec![
                TerminalCell::narrow('x'),
                TerminalCell::trailing_spacer()
            ])
            .validate(),
            Err(FrameError::InvalidCell {
                reason: "trailing spacers require a wide start",
                ..
            })
        ));
    }

    #[test]
    fn leading_spacers_are_only_soft_wrapped_row_end_placeholders() {
        let mut valid = frame(vec![
            TerminalCell::narrow('x'),
            TerminalCell::leading_spacer(),
        ]);
        valid.rows[0].soft_wrapped = true;
        assert!(valid.validate().is_ok());

        let misplaced = frame(vec![
            TerminalCell::leading_spacer(),
            TerminalCell::narrow('x'),
        ]);
        assert!(matches!(
            misplaced.validate(),
            Err(FrameError::InvalidCell {
                reason: "leading spacers require the soft-wrapped end of a row",
                ..
            })
        ));

        let not_wrapped = frame(vec![TerminalCell::leading_spacer()]);
        assert!(matches!(
            not_wrapped.validate(),
            Err(FrameError::InvalidCell {
                reason: "leading spacers require the soft-wrapped end of a row",
                ..
            })
        ));
    }

    #[test]
    fn unsupported_modes_are_not_silently_declared() {
        let mut modes = TerminalModes::default();
        assert_eq!(
            modes.declare(
                &TerminalCapabilities::rio_pinned(),
                TerminalMode::BracketedPaste,
                true
            ),
            Err(ModeError::NotObservable(TerminalMode::BracketedPaste))
        );
        assert_eq!(modes.get(TerminalMode::BracketedPaste), None);
    }

    #[test]
    fn commands_and_effects_keep_their_explicit_order() {
        let commands = [
            TerminalCommand {
                order: CommandOrder(3),
                kind: TerminalCommandKind::Text("a".into()),
            },
            TerminalCommand {
                order: CommandOrder(4),
                kind: TerminalCommandKind::Paste("b".into()),
            },
        ];
        let effects = [
            TerminalEffect {
                order: CommandOrder(8),
                kind: TerminalEffectKind::Title("x".into()),
            },
            TerminalEffect {
                order: CommandOrder(9),
                kind: TerminalEffectKind::Bell,
            },
        ];
        assert!(
            commands
                .windows(2)
                .all(|pair| pair[0].order < pair[1].order)
        );
        assert!(effects.windows(2).all(|pair| pair[0].order < pair[1].order));
    }

    #[test]
    fn sampling_tokens_are_not_stable_revisions() {
        let first = frame(vec![TerminalCell::narrow('a')]);
        let mut second = first.clone();
        second.sampling_token = SamplingToken(2);
        assert_ne!(first.sampling_token, second.sampling_token);
        assert_eq!(first.stable_revision, None);
        assert_eq!(second.stable_revision, None);
        assert_eq!(first.rows, second.rows);
    }
}
