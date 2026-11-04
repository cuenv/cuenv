//! The one token set behind every piece of Cuetty's window chrome.
//!
//! Chrome views take spacing, radii, type, borders, elevation, and colours
//! from here and never write their own literals, so a design decision changes
//! in a single place.

use crate::terminal::model::Rgb;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use gpui::{BoxShadow, Rgba};

/// Spacing scale for padding, gaps, and offsets.
pub(super) mod space {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const XXS: Pixels = px(2.0);
    pub(in crate::terminal::chrome) const XS: Pixels = px(4.0);
    pub(in crate::terminal::chrome) const SM: Pixels = px(6.0);
    pub(in crate::terminal::chrome) const MD: Pixels = px(8.0);
    pub(in crate::terminal::chrome) const LG: Pixels = px(12.0);
    pub(in crate::terminal::chrome) const XL: Pixels = px(16.0);
}

/// Corner radii: one for controls and rows, one for floating panels.
pub(super) mod radius {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const CONTROL: Pixels = px(8.0);
    pub(in crate::terminal::chrome) const PANEL: Pixels = px(12.0);
}

/// Type scale for chrome text. Nothing smaller than `CAPTION` stays legible
/// at normal viewing distance, so there is no smaller step.
pub(super) mod text {
    use gpui::{FontWeight, Pixels, px};

    pub(in crate::terminal::chrome) const CAPTION: Pixels = px(11.0);
    pub(in crate::terminal::chrome) const LABEL: Pixels = px(12.0);
    pub(in crate::terminal::chrome) const BODY: Pixels = px(13.0);
    pub(in crate::terminal::chrome) const TITLE: Pixels = px(14.0);
    pub(in crate::terminal::chrome) const STRONG: FontWeight = FontWeight::SEMIBOLD;
}

/// Border widths.
pub(super) mod border {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const HAIRLINE: Pixels = px(1.0);
}

/// Fixed dimensions of chrome regions and controls.
pub(super) mod size {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const RAIL_WIDTH: Pixels = px(56.0);
    pub(in crate::terminal::chrome) const RAIL_FOOTER_HEIGHT: Pixels = px(112.0);
    pub(in crate::terminal::chrome) const TITLEBAR_HEIGHT: Pixels = px(34.0);
    /// Horizontal room reserved for native window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_WIDTH: Pixels = px(82.0);
    pub(in crate::terminal::chrome) const CONTROL_HEIGHT: Pixels = px(30.0);
    pub(in crate::terminal::chrome) const CONTROL_WIDTH: Pixels = px(34.0);
    pub(in crate::terminal::chrome) const TAB_ROW_HEIGHT: Pixels = px(40.0);
    pub(in crate::terminal::chrome) const TAB_MARK: Pixels = px(30.0);
    pub(in crate::terminal::chrome) const TASK_SIDEBAR_WIDTH: Pixels = px(280.0);
    pub(in crate::terminal::chrome) const PALETTE_WIDTH: Pixels = px(520.0);
    pub(in crate::terminal::chrome) const PALETTE_MAX_HEIGHT: Pixels = px(430.0);
    pub(in crate::terminal::chrome) const PALETTE_TOP_OFFSET: Pixels = px(54.0);
    pub(in crate::terminal::chrome) const PALETTE_FIELD_HEIGHT: Pixels = px(38.0);
    pub(in crate::terminal::chrome) const PALETTE_KIND_WIDTH: Pixels = px(64.0);
    pub(in crate::terminal::chrome) const CARET_WIDTH: Pixels = px(2.0);
    pub(in crate::terminal::chrome) const CARET_HEIGHT: Pixels = px(16.0);
}

/// Elevation of chrome that floats above the terminal.
pub(super) mod elevation {
    use super::BoxShadow;
    use gpui::{hsla, point, px};

    /// A soft, tight shadow; enough to lift a panel off terminal text without
    /// reading as a glow.
    pub(in crate::terminal::chrome) fn overlay() -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.28),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(-4.0),
            },
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.18),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(6.0),
                spread_radius: px(-2.0),
            },
        ]
    }
}

const CHROME_GLASS_OPACITY: f32 = 0.72;

fn with_opacity(color: Rgb, opacity: f32) -> Rgba {
    Rgba {
        a: opacity.clamp(0.0, 1.0),
        ..gpui::rgb(gpui_rgb(color))
    }
}

fn opaque(color: Rgb) -> Rgba {
    gpui::rgb(gpui_rgb(color))
}

/// The terminal's own background layer at the configured opacity.
pub(in crate::terminal) fn terminal_background_color(theme: &TerminalTheme, color: Rgb) -> Rgba {
    with_opacity(color, theme.background_opacity)
}

/// Semantic chrome colours resolved from the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromeColors {
    /// Structural chrome: the title strip and rail.
    pub(super) material: Rgba,
    /// Floating panels above the terminal.
    pub(super) panel: Rgba,
    /// Hovered and active controls on structural material.
    pub(super) state: Rgba,
    /// Hovered and selected rows inside a panel.
    pub(super) panel_state: Rgba,
    /// Input wells recessed into a panel.
    pub(super) well: Rgba,
    /// Fill of the active tab's mark, and the text on it.
    pub(super) mark: Rgba,
    pub(super) mark_text: Rgba,
    pub(super) text: Rgba,
    pub(super) text_secondary: Rgba,
    pub(super) text_muted: Rgba,
    pub(super) divider: Rgba,
    pub(super) accent: Rgba,
}

impl ChromeColors {
    pub(super) fn from_theme(theme: &TerminalTheme) -> Self {
        Self {
            material: Self::tinted_material(theme.title_surface),
            panel: with_opacity(theme.raised_surface, CHROME_GLASS_OPACITY),
            state: with_opacity(theme.raised_surface, CHROME_GLASS_OPACITY),
            panel_state: with_opacity(theme.selection, CHROME_GLASS_OPACITY),
            well: with_opacity(theme.surface, CHROME_GLASS_OPACITY),
            mark: opaque(theme.cursor),
            mark_text: opaque(theme.cursor_text),
            text: opaque(theme.text),
            text_secondary: opaque(theme.title_text),
            text_muted: opaque(theme.muted_text),
            divider: opaque(theme.divider),
            accent: opaque(theme.accent),
        }
    }

    /// Structural material in another base colour.
    pub(super) fn tinted_material(color: Rgb) -> Rgba {
        with_opacity(color, CHROME_GLASS_OPACITY)
    }
}
