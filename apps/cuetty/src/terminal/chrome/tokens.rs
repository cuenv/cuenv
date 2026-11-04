//! The one token set behind every piece of Cuetty's window chrome.
//!
//! Chrome views take spacing, radii, type, borders, elevation, and colours
//! from here and never write their own literals, so a design decision changes
//! in a single place.

use crate::terminal::model::Rgb;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use gpui::{BoxShadow, Pixels, Point, Rgba, point};

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
    use gpui::{DefiniteLength, FontWeight, Pixels, px, relative};

    pub(in crate::terminal::chrome) const CAPTION: Pixels = px(11.0);
    pub(in crate::terminal::chrome) const LABEL: Pixels = px(12.0);
    pub(in crate::terminal::chrome) const BODY: Pixels = px(13.0);
    pub(in crate::terminal::chrome) const TITLE: Pixels = px(14.0);
    /// Line height of panel text as a multiple of its size. GPUI otherwise
    /// inherits phi (1.618), which is loose for chrome and throws off every
    /// height budget.
    pub(in crate::terminal::chrome) const LINE_HEIGHT_RATIO: f32 = 1.4;
    pub(in crate::terminal::chrome) const LINE_HEIGHT: DefiniteLength = relative(LINE_HEIGHT_RATIO);
    /// Characters of `LABEL` text that sit comfortably on one tooltip line.
    pub(in crate::terminal::chrome) const TOOLTIP_LINE_CHARS: usize = 56;
    /// A tooltip is a glance: longer text is cut here, counting wrapped and
    /// newline-separated lines alike, rather than filling the window, as an
    /// evaluation error can run to many lines.
    pub(in crate::terminal::chrome) const TOOLTIP_MAX_LINES: usize = 12;
    pub(in crate::terminal::chrome) const REGULAR: FontWeight = FontWeight::NORMAL;
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
    pub(in crate::terminal::chrome) const TITLEBAR_HEIGHT: Pixels = px(34.0);
    /// Height of the native macOS window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_HEIGHT: Pixels = px(16.0);
    /// Horizontal room reserved for native window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_WIDTH: Pixels = px(82.0);
    pub(in crate::terminal::chrome) const CONTROL_HEIGHT: Pixels = px(30.0);
    pub(in crate::terminal::chrome) const CONTROL_WIDTH: Pixels = px(34.0);
    pub(in crate::terminal::chrome) const TAB_ROW_HEIGHT: Pixels = px(40.0);
    pub(in crate::terminal::chrome) const TAB_MARK: Pixels = px(30.0);
    pub(in crate::terminal::chrome) const TAB_MARKER_WIDTH: Pixels = px(3.0);
    pub(in crate::terminal::chrome) const TAB_MARKER_HEIGHT: Pixels = px(16.0);
    pub(in crate::terminal::chrome) const TASK_SIDEBAR_WIDTH: Pixels = px(280.0);
    pub(in crate::terminal::chrome) const PALETTE_WIDTH: Pixels = px(520.0);
    pub(in crate::terminal::chrome) const PALETTE_ROW_HEIGHT: Pixels = px(32.0);
    pub(in crate::terminal::chrome) const PALETTE_TOP_OFFSET: Pixels = px(54.0);
    pub(in crate::terminal::chrome) const PALETTE_FIELD_HEIGHT: Pixels = px(38.0);
    pub(in crate::terminal::chrome) const PALETTE_KIND_WIDTH: Pixels = px(64.0);
    pub(in crate::terminal::chrome) const CARET_WIDTH: Pixels = px(2.0);
    pub(in crate::terminal::chrome) const CARET_HEIGHT: Pixels = px(16.0);
    pub(in crate::terminal::chrome) const NOTICE_MARK: Pixels = px(6.0);
    pub(in crate::terminal::chrome) const TOOLTIP_WRAP_WIDTH: Pixels = px(360.0);
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

/// The palette height that holds `rows` results along with its field, hint,
/// gaps, padding, and outline.
pub(super) fn palette_height(rows: usize) -> Pixels {
    let hint = (text::CAPTION * text::LINE_HEIGHT_RATIO).ceil();
    let frame = border::HAIRLINE * 2.0 + space::LG * 2.0;
    let fixed = size::PALETTE_FIELD_HEIGHT + hint + space::MD * 2.0;
    let results =
        size::PALETTE_ROW_HEIGHT * rows as f32 + space::XS * rows.saturating_sub(1) as f32;
    frame + fixed + results
}

/// The height of a tooltip at its line limit, with its padding and outline.
pub(super) fn tooltip_max_height() -> Pixels {
    let line = (text::LABEL * text::LINE_HEIGHT_RATIO).ceil();
    line * text::TOOLTIP_MAX_LINES as f32 + space::XS * 2.0 + border::HAIRLINE * 2.0
}

/// Where native window controls sit: centred in the title strip, so they follow
/// its height.
pub(in crate::terminal) fn traffic_light_position() -> Point<Pixels> {
    point(
        space::LG,
        (size::TITLEBAR_HEIGHT - size::WINDOW_CONTROLS_HEIGHT) * 0.5,
    )
}

pub(super) fn opaque(color: Rgb) -> Rgba {
    gpui::rgb(gpui_rgb(color))
}

/// The window's one material: the terminal background and structural chrome
/// share this opacity, so the window never reads as glass beside paint.
pub(super) fn material_color(theme: &TerminalTheme, color: Rgb) -> Rgba {
    Rgba {
        a: theme.background_opacity.clamp(0.0, 1.0),
        ..opaque(color)
    }
}

/// Semantic chrome colours resolved from the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromeColors {
    /// Structural chrome: the title strip and rail. Shares the terminal's
    /// opacity.
    pub(super) material: Rgba,
    /// Floating panels. Always opaque, because they cover terminal text that
    /// would otherwise show through their own.
    pub(super) panel: Rgba,
    /// Hovered and active controls on structural material, in the same opacity
    /// as that material. The rail stays one material at any transparency; the
    /// marker and text weight carry the active tab where this fill fades out.
    pub(super) state: Rgba,
    /// The keyboard-selected row in a panel. Muted text is not legible on this
    /// fill, so that row's metadata uses secondary text.
    pub(super) panel_selected: Rgba,
    /// Hovered rows in a panel, on which muted metadata stays legible. A row's
    /// text colour is fixed when GPUI lays it out, before hover is known, so
    /// hover can only change a fill and never the text on top of it.
    pub(super) panel_hover: Rgba,
    /// Input wells recessed into a panel.
    pub(super) well: Rgba,
    pub(super) text: Rgba,
    pub(super) text_secondary: Rgba,
    pub(super) text_muted: Rgba,
    /// Lines between structural regions. Shares the material's opacity, so a
    /// transparent window has no solid lines left floating in it.
    pub(super) divider: Rgba,
    /// Outline of floating panels, opaque like the panels themselves.
    pub(super) panel_outline: Rgba,
    /// The theme accent. A project accent replaces it on the active tab marker.
    pub(super) accent: Rgba,
}

impl ChromeColors {
    pub(super) fn from_theme(theme: &TerminalTheme) -> Self {
        Self {
            material: material_color(theme, theme.title_surface),
            panel: opaque(theme.raised_surface),
            state: material_color(theme, theme.raised_surface),
            panel_selected: opaque(theme.selection),
            panel_hover: opaque(theme.hover_surface),
            well: opaque(theme.surface),
            text: opaque(theme.text),
            text_secondary: opaque(theme.title_text),
            text_muted: opaque(theme.muted_text),
            divider: material_color(theme, theme.divider),
            panel_outline: opaque(theme.divider),
            accent: opaque(theme.accent),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::TerminalConfig;

    fn theme_with_transparency(percent: u8) -> TerminalTheme {
        TerminalTheme::from_config(&TerminalConfig {
            terminal_transparency_percent: percent,
            ..TerminalConfig::default()
        })
    }

    #[test]
    fn palette_height_grows_by_a_row_and_a_gap_only_between_rows() {
        let one = palette_height(1);
        assert_eq!(palette_height(0) + size::PALETTE_ROW_HEIGHT, one);
        assert_eq!(
            one + size::PALETTE_ROW_HEIGHT + space::XS,
            palette_height(2)
        );
    }

    #[test]
    fn zero_transparency_makes_chrome_and_terminal_fully_opaque() {
        let theme = theme_with_transparency(0);
        assert_eq!(ChromeColors::from_theme(&theme).material.a, 1.0);
        assert_eq!(material_color(&theme, theme.surface).a, 1.0);
    }

    #[test]
    fn chrome_and_terminal_share_one_opacity_above_zero_transparency() {
        let theme = theme_with_transparency(65);
        let chrome = ChromeColors::from_theme(&theme).material.a;
        assert!((chrome - 0.35).abs() < f32::EPSILON);
        assert_eq!(chrome, material_color(&theme, theme.surface).a);
    }

    #[test]
    fn structural_dividers_fade_with_the_material_but_panel_outlines_do_not() {
        let colors = ChromeColors::from_theme(&theme_with_transparency(65));
        assert_eq!(colors.divider.a, colors.material.a);
        assert_eq!(colors.panel_outline.a, 1.0);
    }

    #[test]
    fn structural_control_states_share_the_material_opacity() {
        let colors = ChromeColors::from_theme(&theme_with_transparency(65));
        assert_eq!(colors.state.a, colors.material.a);
    }

    #[test]
    fn floating_panels_stay_legible_at_full_transparency() {
        let colors = ChromeColors::from_theme(&theme_with_transparency(100));
        assert_eq!(colors.material.a, 0.0);
        for fill in [
            colors.panel,
            colors.panel_selected,
            colors.panel_hover,
            colors.well,
        ] {
            assert_eq!(fill.a, 1.0);
        }
    }
}
