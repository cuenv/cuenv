//! The one token set behind every piece of Cuetty's window chrome.
//!
//! Chrome views take spacing, radii, type, borders, elevation, and colours
//! from here and never write their own literals, so a design decision changes
//! in a single place.

use crate::integrations::cuenv::TitlebarVisibility;
use crate::terminal::model::Rgb;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use gpui::{BoxShadow, Decorations, Pixels, Point, Rgba, TextSystem, font, point};
use std::sync::OnceLock;

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
    pub(in crate::terminal::chrome) const MEDIUM: FontWeight = FontWeight::MEDIUM;
    pub(in crate::terminal::chrome) const STRONG: FontWeight = FontWeight::SEMIBOLD;
}

/// GPUI's name for the platform UI font. On macOS and Windows it is the real one.
const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// Sans-serif families to use where the platform has no UI font to ask for. GPUI
/// maps its name to IBM Plex Sans on Linux and, when that is absent, walks a stack
/// that starts with a monospaced font, which is what chrome text must avoid.
const LINUX_UI_FONTS: &[&str] = &[
    "Inter",
    "Adwaita Sans",
    "Cantarell",
    "Ubuntu",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
];

/// A family no system has, which resolves to whatever GPUI falls back to.
const MISSING_FONT: &str = "cuetty-no-such-font";

static UI_FONT_FAMILY: OnceLock<&'static str> = OnceLock::new();

fn first_available_family(
    candidates: &[&'static str],
    available: impl Fn(&str) -> bool,
) -> &'static str {
    candidates
        .iter()
        .copied()
        .find(|family| available(family))
        .unwrap_or(SYSTEM_UI_FONT)
}

/// Settles the chrome's font for the life of the process. GPUI offers no way to
/// ask whether a family exists, and its name list includes its own fallbacks
/// whether installed or not, so a family counts as present when it resolves to
/// something other than the fallback. The fallback's own family is therefore
/// passed over, which only ever picks another family from this list.
pub(in crate::terminal) fn resolve_ui_font(text_system: &TextSystem) {
    let candidates = if cfg!(target_os = "linux") {
        LINUX_UI_FONTS
    } else {
        &[]
    };
    let fallback = text_system.resolve_font(&font(MISSING_FONT));
    let family = first_available_family(candidates, |family| {
        text_system.resolve_font(&font(family.to_string())) != fallback
    });
    let _ = UI_FONT_FAMILY.set(family);
}

/// The sans-serif family all chrome text uses, other than paths and shortcuts.
pub(super) fn ui_font_family() -> &'static str {
    UI_FONT_FAMILY.get().copied().unwrap_or(SYSTEM_UI_FONT)
}

/// Border widths.
pub(super) mod border {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const HAIRLINE: Pixels = px(1.0);
}

/// Fixed dimensions of chrome regions and controls.
pub(super) mod size {
    use gpui::{Pixels, px};

    pub(in crate::terminal::chrome) const SIDEBAR_EXPANDED_WIDTH: Pixels = px(200.0);
    pub(in crate::terminal::chrome) const SIDEBAR_COLLAPSED_WIDTH: Pixels = px(56.0);
    pub(in crate::terminal::chrome) const TITLEBAR_HEIGHT: Pixels = px(34.0);
    /// Height of the native macOS window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_HEIGHT: Pixels = px(16.0);
    /// Horizontal room reserved for native window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_WIDTH: Pixels = px(82.0);
    pub(in crate::terminal::chrome) const CONTROL_HEIGHT: Pixels = px(30.0);
    pub(in crate::terminal::chrome) const CONTROL_WIDTH: Pixels = px(34.0);
    pub(in crate::terminal::chrome) const ICON: Pixels = px(16.0);
    pub(in crate::terminal::chrome) const TAB_ROW_HEIGHT: Pixels = px(32.0);
    /// A tab's row in the list, with room for a second line beneath the name, or for an
    /// indicator beneath a collapsed tab's mark. It has this height with or without
    /// them and in both sidebar modes, so neither arriving nor collapsing shifts the list.
    pub(in crate::terminal::chrome) const TAB_ROW_TWO_LINE_HEIGHT: Pixels = px(44.0);
    /// The line a collapsed tab's mark sits on, in the middle of its row.
    pub(in crate::terminal::chrome) const TAB_MARK_HEIGHT: Pixels = px(16.0);
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
    /// The strip below an exited shell's output.
    pub(in crate::terminal::chrome) const EXIT_NOTICE_HEIGHT: Pixels = px(28.0);
    /// A tab's status indicator, small enough to sit beside its name or its mark.
    pub(in crate::terminal::chrome) const STATUS_ICON: Pixels = px(12.0);
    /// What a tab keeps free for its close button and its indicator, which share it,
    /// so neither appearing moves the name.
    pub(in crate::terminal::chrome) const TAB_ACTION: Pixels = px(20.0);
    pub(in crate::terminal::chrome) const TOOLTIP_WRAP_WIDTH: Pixels = px(360.0);
}

/// How far the terminal's colours move towards its background while its window does
/// not have the keyboard: little enough to go on reading it, and far enough to tell at
/// a glance where typing would go.
pub(in crate::terminal) const UNFOCUSED_TERMINAL_FADE: f32 = 0.2;

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

/// Whether the platform draws window controls over the window's content. GPUI fixes
/// that when it creates a window, so hiding the title strip cannot take them away.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowControls {
    OverContent,
    Elsewhere,
}

impl WindowControls {
    pub(super) const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::OverContent
        } else {
            Self::Elsewhere
        }
    }
}

/// Whether the title strip draws, given what the window has around it. Server-side
/// decorations leave the title and moving the window to the windowing system, so a
/// strip would repeat a native title bar. macOS reports them too, but there the native
/// bar is transparent and the strip sits behind the window controls as its only title.
pub(super) fn drawn_titlebar(
    configured: TitlebarVisibility,
    controls: WindowControls,
    decorations: Decorations,
) -> TitlebarVisibility {
    match (controls, decorations) {
        (WindowControls::Elsewhere, Decorations::Server) => TitlebarVisibility::Hidden,
        _ => configured,
    }
}

/// Where the terminal starts below the top of its panel: its padding, unless that
/// would put the first row under window controls with no strip to hold them.
pub(super) fn terminal_top_inset(
    padding: Pixels,
    titlebar: TitlebarVisibility,
    controls: WindowControls,
) -> Pixels {
    match (titlebar, controls) {
        (TitlebarVisibility::Hidden, WindowControls::OverContent) => {
            padding.max(size::TITLEBAR_HEIGHT)
        }
        _ => padding,
    }
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

/// The least opacity of the sidebar's outline and of hover and active fills. These
/// mark an edge or a state rather than fill a region, and in a very transparent window
/// they would otherwise leave the panel undrawn and its controls without feedback.
const STATE_MIN_ALPHA: f32 = 0.5;

/// A mark of an edge or a state: in the window's material, but never fading out.
fn state_color(theme: &TerminalTheme, color: Rgb) -> Rgba {
    let material = material_color(theme, color);
    Rgba {
        a: material.a.max(STATE_MIN_ALPHA),
        ..material
    }
}

/// `color` part of the way in, for something fading: a hover, or a tab becoming active.
pub(super) fn faded(color: Rgba, level: f32) -> Rgba {
    Rgba {
        a: color.a * level.clamp(0.0, 1.0),
        ..color
    }
}

/// How far down its list the active tab's marker sits at a fractional place in it, so
/// it can travel between rows. Every row is the same height, in both sidebar modes.
pub(super) fn tab_marker_top(place: f32) -> Pixels {
    let row = size::TAB_ROW_TWO_LINE_HEIGHT;
    space::MD + (row + space::XXS) * place + (row - size::TAB_MARKER_HEIGHT) / 2.0
}

/// Semantic chrome colours resolved from the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromeColors {
    /// The one layer behind the terminal, the title strip, and the sidebar's
    /// surroundings. Nothing structural paints a second layer over it, because
    /// alpha would compound and the window would stop reading as one material.
    pub(super) material: Rgba,
    /// The floating sidebar's fill: a neutral step off the terminal in an opaque
    /// window, and nothing in a translucent one, where a fill could only be a
    /// second layer. There the outline, which never fades out, draws the panel.
    pub(super) sidebar: Rgba,
    /// Floating panels. Always opaque, because they cover terminal text that
    /// would otherwise show through their own.
    pub(super) panel: Rgba,
    /// Hovered and active controls in the sidebar, in the window's opacity down to
    /// a floor that keeps them visible.
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
    /// The sidebar's outline. Shares the material's opacity down to a floor, so a
    /// transparent window has no solid lines in it and still shows the panel.
    pub(super) divider: Rgba,
    /// Outline of floating panels, opaque like the panels themselves.
    pub(super) panel_outline: Rgba,
    /// The window's accent: the `accent` setting or the theme's. A project's `rail`
    /// replaces it on the active tab marker only.
    pub(super) accent: Rgba,
    /// The one thing that is not calm: a tab whose shell failed. It comes from the
    /// terminal's own palette, so it is the red the user already reads as an error.
    pub(super) danger: Rgba,
}

impl ChromeColors {
    pub(super) fn from_theme(theme: &TerminalTheme) -> Self {
        Self {
            material: material_color(theme, theme.surface),
            sidebar: if theme.background_opacity >= 1.0 {
                opaque(theme.title_surface)
            } else {
                gpui::transparent_black().to_rgb()
            },
            panel: opaque(theme.raised_surface),
            state: state_color(theme, theme.raised_surface),
            panel_selected: opaque(theme.selection),
            panel_hover: opaque(theme.hover_surface),
            well: opaque(theme.surface),
            text: opaque(theme.text),
            text_secondary: opaque(theme.title_text),
            text_muted: opaque(theme.muted_text),
            divider: state_color(theme, theme.divider),
            panel_outline: opaque(theme.divider),
            accent: opaque(theme.accent),
            danger: opaque(theme.ansi[1]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::TerminalConfig;

    #[test]
    fn a_hidden_strip_keeps_the_terminal_clear_of_controls_drawn_over_it() {
        let inset = |padding| {
            terminal_top_inset(
                padding,
                TitlebarVisibility::Hidden,
                WindowControls::OverContent,
            )
        };
        assert_eq!(inset(gpui::px(0.0)), size::TITLEBAR_HEIGHT);
        assert_eq!(inset(gpui::px(14.0)), size::TITLEBAR_HEIGHT);
        let generous = size::TITLEBAR_HEIGHT + gpui::px(1.0);
        assert_eq!(inset(generous), generous);
    }

    #[test]
    fn only_a_hidden_strip_over_drawn_controls_moves_the_terminal() {
        let padding = gpui::px(14.0);
        for (titlebar, controls) in [
            (TitlebarVisibility::Shown, WindowControls::OverContent),
            (TitlebarVisibility::Shown, WindowControls::Elsewhere),
            (TitlebarVisibility::Hidden, WindowControls::Elsewhere),
        ] {
            assert_eq!(terminal_top_inset(padding, titlebar, controls), padding);
        }
    }

    #[test]
    fn a_native_title_bar_replaces_the_strip_only_where_it_is_visible() {
        let client = Decorations::Client {
            tiling: gpui::Tiling::default(),
        };
        let drawn = |controls, decorations| {
            drawn_titlebar(TitlebarVisibility::Shown, controls, decorations)
        };
        assert_eq!(
            drawn(WindowControls::Elsewhere, Decorations::Server),
            TitlebarVisibility::Hidden
        );
        assert_eq!(
            drawn(WindowControls::Elsewhere, client),
            TitlebarVisibility::Shown
        );
        assert_eq!(
            drawn(WindowControls::OverContent, Decorations::Server),
            TitlebarVisibility::Shown
        );
    }

    #[test]
    fn a_hidden_strip_stays_hidden_whatever_surrounds_the_window() {
        assert_eq!(
            drawn_titlebar(
                TitlebarVisibility::Hidden,
                WindowControls::OverContent,
                Decorations::Client {
                    tiling: gpui::Tiling::default(),
                },
            ),
            TitlebarVisibility::Hidden
        );
    }

    fn theme_with_transparency(percent: u8) -> TerminalTheme {
        TerminalTheme::from_config(&TerminalConfig {
            terminal_transparency_percent: percent,
            ..TerminalConfig::default()
        })
    }

    #[test]
    fn the_first_installed_family_wins_and_the_system_name_is_the_last_resort() {
        let candidates = ["Inter", "Noto Sans"];
        assert_eq!(first_available_family(&candidates, |_| true), "Inter");
        assert_eq!(
            first_available_family(&candidates, |family| family == "Noto Sans"),
            "Noto Sans"
        );
        assert_eq!(
            first_available_family(&candidates, |_| false),
            SYSTEM_UI_FONT
        );
        assert_eq!(first_available_family(&[], |_| true), SYSTEM_UI_FONT);
    }

    #[test]
    fn the_tab_marker_is_centred_on_a_row_at_every_whole_place() {
        let row_top = |place: f32| space::MD + (size::TAB_ROW_TWO_LINE_HEIGHT + space::XXS) * place;
        for place in [0.0, 1.0, 7.0] {
            let marker = tab_marker_top(place);
            let above = marker - row_top(place);
            let below = size::TAB_ROW_TWO_LINE_HEIGHT - size::TAB_MARKER_HEIGHT - above;
            assert_eq!(above, below);
        }
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
        let chrome = ChromeColors::from_theme(&theme).material;
        assert!((chrome.a - 0.35).abs() < f32::EPSILON);
        assert_eq!(chrome, material_color(&theme, theme.surface));
    }

    #[test]
    fn the_sidebar_is_a_neutral_fill_when_opaque_and_no_layer_when_translucent() {
        let theme = theme_with_transparency(0);
        assert_eq!(
            ChromeColors::from_theme(&theme).sidebar,
            opaque(theme.title_surface)
        );
        for percent in [1, 65, 100] {
            let colors = ChromeColors::from_theme(&theme_with_transparency(percent));
            assert_eq!(colors.sidebar.a, 0.0);
        }
    }

    #[test]
    fn structural_dividers_fade_with_the_material_but_panel_outlines_do_not() {
        let colors = ChromeColors::from_theme(&theme_with_transparency(30));
        assert_eq!(colors.divider.a, colors.material.a);
        assert_eq!(colors.panel_outline.a, 1.0);
    }

    #[test]
    fn structural_control_states_share_the_material_opacity() {
        let colors = ChromeColors::from_theme(&theme_with_transparency(30));
        assert_eq!(colors.state.a, colors.material.a);
    }

    #[test]
    fn the_sidebar_outline_and_control_states_survive_any_transparency() {
        for percent in [90, 100] {
            let colors = ChromeColors::from_theme(&theme_with_transparency(percent));
            assert!(colors.material.a < STATE_MIN_ALPHA);
            assert_eq!(colors.divider.a, STATE_MIN_ALPHA);
            assert_eq!(colors.state.a, STATE_MIN_ALPHA);
        }
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
