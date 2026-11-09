//! The one token set behind every piece of Cuetty's window chrome.
//!
//! Chrome views take spacing, radii, type, borders, elevation, and colours
//! from here and never write their own literals, so a design decision changes
//! in a single place.

use crate::integrations::cuenv::TitlebarVisibility;
use crate::terminal::color;
use crate::terminal::config::MinimumContrast;
use crate::terminal::model::Rgb;
use crate::terminal::presentation;
use crate::terminal::presentation::TerminalTheme;
use gpui::{BoxShadow, Decorations, Pixels, Point, Rgba, Styled, TextSystem};
use std::sync::OnceLock;

/// Spacing scale for padding, gaps, and offsets.
pub(super) mod space {
    use gpui::Pixels;

    pub(in crate::terminal::chrome) const XXS: Pixels = gpui::px(2.0);
    pub(in crate::terminal::chrome) const XS: Pixels = gpui::px(4.0);
    pub(in crate::terminal::chrome) const SM: Pixels = gpui::px(6.0);
    pub(in crate::terminal::chrome) const MD: Pixels = gpui::px(8.0);
    pub(in crate::terminal::chrome) const LG: Pixels = gpui::px(12.0);
    pub(in crate::terminal::chrome) const XL: Pixels = gpui::px(16.0);
}

/// Corner radii: one for controls and rows, one for floating panels.
pub(super) mod radius {
    use gpui::Pixels;

    pub(in crate::terminal::chrome) const CONTROL: Pixels = gpui::px(8.0);
    pub(in crate::terminal::chrome) const PANEL: Pixels = gpui::px(12.0);
}

/// Type scale for chrome text. Nothing smaller than `CAPTION` stays legible
/// at normal viewing distance, so there is no smaller step.
pub(super) mod text {
    use gpui::{DefiniteLength, FontWeight, Pixels};

    pub(in crate::terminal::chrome) const CAPTION: Pixels = gpui::px(11.0);
    pub(in crate::terminal::chrome) const LABEL: Pixels = gpui::px(12.0);
    pub(in crate::terminal::chrome) const BODY: Pixels = gpui::px(13.0);
    pub(in crate::terminal::chrome) const TITLE: Pixels = gpui::px(14.0);
    /// Line height of panel text as a multiple of its size. GPUI otherwise
    /// inherits phi (1.618), which is loose for chrome and throws off every
    /// height budget.
    pub(in crate::terminal::chrome) const LINE_HEIGHT_RATIO: f32 = 1.4;
    pub(in crate::terminal::chrome) const LINE_HEIGHT: DefiniteLength =
        gpui::relative(LINE_HEIGHT_RATIO);
    /// Characters of `LABEL` text that sit comfortably on one tooltip line.
    pub(in crate::terminal::chrome) const TOOLTIP_LINE_CHARS: usize = 56;
    /// A tooltip is a glance: longer text is cut here, counting wrapped and
    /// newline-separated lines alike, rather than filling the window, as an
    /// evaluation error can run to many lines.
    pub(in crate::terminal::chrome) const TOOLTIP_MAX_LINES: usize = 12;
    /// How much of an error a tab shows in place of its terminal; a session's errors are
    /// a sentence or two, but they are not ours and can run on.
    pub(in crate::terminal::chrome) const SESSION_STATE_DETAIL_LINES: usize = 6;
    /// Characters of `BODY` text that sit comfortably on one line of that message.
    pub(in crate::terminal::chrome) const SESSION_STATE_DETAIL_LINE_CHARS: usize = 64;
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
    let fallback = text_system.resolve_font(&gpui::font(MISSING_FONT));
    let family = first_available_family(candidates, |family| {
        text_system.resolve_font(&gpui::font(family.to_string())) != fallback
    });
    let _ = UI_FONT_FAMILY.set(family);
}

/// The sans-serif family all chrome text uses, other than paths and shortcuts.
pub(super) fn ui_font_family() -> &'static str {
    UI_FONT_FAMILY.get().copied().unwrap_or(SYSTEM_UI_FONT)
}

/// Border widths.
pub(super) mod border {
    use gpui::Pixels;

    pub(in crate::terminal::chrome) const HAIRLINE: Pixels = gpui::px(1.0);
}

/// Fixed dimensions of chrome regions and controls.
pub(super) mod size {
    use gpui::Pixels;

    pub(in crate::terminal::chrome) const SIDEBAR_EXPANDED_WIDTH: Pixels = gpui::px(200.0);
    pub(in crate::terminal::chrome) const SIDEBAR_COLLAPSED_WIDTH: Pixels = gpui::px(56.0);
    pub(in crate::terminal::chrome) const TITLEBAR_HEIGHT: Pixels = gpui::px(34.0);
    /// Height of the native macOS window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_HEIGHT: Pixels = gpui::px(16.0);
    /// Horizontal room reserved for native window controls.
    pub(in crate::terminal::chrome) const WINDOW_CONTROLS_WIDTH: Pixels = gpui::px(82.0);
    pub(in crate::terminal::chrome) const CONTROL_HEIGHT: Pixels = gpui::px(30.0);
    pub(in crate::terminal::chrome) const CONTROL_WIDTH: Pixels = gpui::px(34.0);
    pub(in crate::terminal::chrome) const ICON: Pixels = gpui::px(16.0);
    pub(in crate::terminal::chrome) const TAB_ROW_HEIGHT: Pixels = gpui::px(32.0);
    /// A tab's row in the list, with room for a second line beneath the name, or for an
    /// indicator beneath a collapsed tab's mark. It has this height with or without
    /// them and in both sidebar modes, so neither arriving nor collapsing shifts the list.
    pub(in crate::terminal::chrome) const TAB_ROW_TWO_LINE_HEIGHT: Pixels = gpui::px(44.0);
    /// The most the project panel takes from the tab list, past which it scrolls.
    pub(in crate::terminal::chrome) const PROJECT_PANEL_MAX_HEIGHT: Pixels = gpui::px(240.0);
    /// What the tab list keeps for itself when the window is too short for both: two
    /// rows, so the project panel gives way first.
    pub(in crate::terminal::chrome) const TAB_LIST_MIN_HEIGHT: Pixels = gpui::px(112.0);
    /// The line a collapsed tab's mark sits on, in the middle of its row.
    pub(in crate::terminal::chrome) const TAB_MARK_HEIGHT: Pixels = gpui::px(16.0);
    pub(in crate::terminal::chrome) const TAB_MARKER_WIDTH: Pixels = gpui::px(3.0);
    pub(in crate::terminal::chrome) const TAB_MARKER_HEIGHT: Pixels = gpui::px(16.0);
    pub(in crate::terminal::chrome) const PALETTE_WIDTH: Pixels = gpui::px(520.0);
    pub(in crate::terminal::chrome) const PALETTE_ROW_HEIGHT: Pixels = gpui::px(44.0);
    pub(in crate::terminal::chrome) const PALETTE_TOP_OFFSET: Pixels = gpui::px(54.0);
    pub(in crate::terminal::chrome) const PALETTE_FIELD_HEIGHT: Pixels = gpui::px(38.0);
    pub(in crate::terminal::chrome) const PALETTE_KIND_WIDTH: Pixels = gpui::px(64.0);
    pub(in crate::terminal::chrome) const CARET_WIDTH: Pixels = gpui::px(2.0);
    pub(in crate::terminal::chrome) const CARET_HEIGHT: Pixels = gpui::px(16.0);
    pub(in crate::terminal::chrome) const NOTICE_MARK: Pixels = gpui::px(6.0);
    /// The strip below an exited shell's output.
    pub(in crate::terminal::chrome) const EXIT_NOTICE_HEIGHT: Pixels = gpui::px(28.0);
    /// A tab's status indicator, small enough to sit beside its name or its mark.
    pub(in crate::terminal::chrome) const STATUS_ICON: Pixels = gpui::px(12.0);
    /// What a tab keeps free for its close button and its indicator, which share it,
    /// so neither appearing moves the name.
    pub(in crate::terminal::chrome) const TAB_ACTION: Pixels = gpui::px(20.0);
    pub(in crate::terminal::chrome) const TOOLTIP_WRAP_WIDTH: Pixels = gpui::px(360.0);
    pub(in crate::terminal::chrome) const SEARCH_BAR_WIDTH: Pixels = gpui::px(360.0);
    pub(in crate::terminal::chrome) const SEARCH_BUTTON: Pixels = gpui::px(26.0);
    /// How much of the terminal's edge answers to the pointer. Within its default padding,
    /// so a selection can still start in the last column.
    pub(in crate::terminal::chrome) const SCROLLBAR_HIT_WIDTH: Pixels = gpui::px(12.0);
    pub(in crate::terminal::chrome) const SCROLLBAR_THUMB_WIDTH: Pixels = gpui::px(5.0);
    /// The thumb's gap from the terminal's edge.
    pub(in crate::terminal::chrome) const SCROLLBAR_INSET: Pixels = gpui::px(3.0);
    pub(in crate::terminal::chrome) const SCROLLBAR_MIN_THUMB: Pixels = gpui::px(28.0);
    /// The measure of the message a tab shows in place of its terminal.
    pub(in crate::terminal::chrome) const SESSION_STATE_WIDTH: Pixels = gpui::px(440.0);
}

/// How far the terminal's colours move towards its background while its window does
/// not have the keyboard: little enough to go on reading it, and far enough to tell at
/// a glance where typing would go.
pub(in crate::terminal) const UNFOCUSED_TERMINAL_FADE: f32 = 0.2;

/// How far something floats above the terminal, which sets its shadow, its corners, and
/// the outline and fill every floating surface shares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Elevation {
    /// A small label just off what it belongs to: tooltips and a tab being dragged.
    Raised,
    /// A panel beside the terminal that the user can keep working around.
    Floating,
    /// A panel the keyboard has moved into, above everything else.
    Modal,
}

/// How much of a dark theme's shadow a light theme casts. Black at the same alpha is far
/// more contrast against a pale page, and would read as a smudge around the panel.
const LIGHT_THEME_SHADOW_STRENGTH: f32 = 0.5;

impl Elevation {
    fn shadows(self, strength: f32) -> Vec<BoxShadow> {
        let contact = |alpha: f32| BoxShadow {
            color: gpui::hsla(0.0, 0.0, 0.0, alpha * strength),
            offset: gpui::point(gpui::px(0.0), gpui::px(2.0)),
            blur_radius: gpui::px(6.0),
            spread_radius: gpui::px(-2.0),
        };
        let cast = |alpha: f32, offset, blur| BoxShadow {
            color: gpui::hsla(0.0, 0.0, 0.0, alpha * strength),
            offset: gpui::point(gpui::px(0.0), gpui::px(offset)),
            blur_radius: gpui::px(blur),
            spread_radius: gpui::px(-4.0),
        };
        match self {
            Self::Raised => vec![contact(0.24)],
            Self::Floating => vec![cast(0.28, 8.0, 24.0), contact(0.18)],
            Self::Modal => vec![cast(0.36, 16.0, 40.0), contact(0.18)],
        }
    }

    const fn corners(self) -> Pixels {
        match self {
            Self::Raised => radius::CONTROL,
            Self::Floating | Self::Modal => radius::PANEL,
        }
    }
}

/// Floats an element over the terminal: opaque, because it covers text that would
/// otherwise show through its own, with an outline to hold its edge on a dark theme
/// where a shadow alone would not.
pub(super) fn elevated<E: Styled>(element: E, level: Elevation, colors: &ChromeColors) -> E {
    element
        .rounded(level.corners())
        .border(border::HAIRLINE)
        .border_color(colors.panel_outline)
        .bg(colors.panel)
        .shadow(level.shadows(colors.shadow_strength))
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
    gpui::point(
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
    gpui::rgb(presentation::gpui_rgb(color))
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

/// How strong the scrollbar's thumb is against the same colour once the pointer is on it.
const SCROLLBAR_RESTING_STRENGTH: f32 = 0.55;

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
    /// A control on a row already filled with `state`, one step off that fill, with
    /// the same opacity. In an opaque window `state` over itself would show nothing.
    pub(super) state_on_row: Rgba,
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
    /// The scrollbar's thumb while the view moves, quiet enough to sit over terminal text.
    pub(super) scrollbar: Rgba,
    /// The thumb once the pointer is on it.
    pub(super) scrollbar_active: Rgba,
    /// The window's accent: the `accent` setting or the theme's. A project's `rail`
    /// replaces it on the active tab marker only.
    pub(super) accent: Rgba,
    /// The one thing that is not calm: a tab whose shell failed. It comes from the
    /// terminal's own palette, so it is the red the user already reads as an error.
    pub(super) danger: Rgba,
    /// How strongly floating surfaces cast their shadows on this theme's page.
    shadow_strength: f32,
    /// What marks are seen against: the sidebar, and the active row they sit on most of
    /// all. See [`Self::mark`].
    mark_grounds: [Rgb; 2],
}

impl ChromeColors {
    pub(super) fn from_theme(theme: &TerminalTheme) -> Self {
        let state = state_color(theme, theme.raised_surface);
        let mark_grounds = if theme.background_opacity >= 1.0 {
            [theme.title_surface, theme.raised_surface]
        } else {
            // There is no sidebar fill here, and what shows through is unknowable, so the
            // page stands in for whatever is behind the window's one material.
            [
                theme.surface,
                color::blend(theme.surface, theme.raised_surface, state.a),
            ]
        };
        let mark = |color| mark_on(mark_grounds, color);
        Self {
            material: material_color(theme, theme.surface),
            sidebar: if theme.background_opacity >= 1.0 {
                opaque(theme.title_surface)
            } else {
                gpui::transparent_black().to_rgb()
            },
            panel: opaque(theme.raised_surface),
            state,
            state_on_row: state_color(theme, theme.hover_surface),
            panel_selected: opaque(theme.selection),
            panel_hover: opaque(theme.hover_surface),
            well: opaque(theme.surface),
            text: opaque(theme.text),
            text_secondary: opaque(theme.title_text),
            text_muted: opaque(theme.muted_text),
            divider: state_color(theme, theme.divider),
            panel_outline: opaque(theme.divider),
            scrollbar: faded(opaque(theme.muted_text), SCROLLBAR_RESTING_STRENGTH),
            scrollbar_active: opaque(theme.muted_text),
            accent: mark(theme.accent),
            danger: mark(theme.ansi[1]),
            shadow_strength: if crate::terminal::color::is_light(theme.surface) {
                LIGHT_THEME_SHADOW_STRENGTH
            } else {
                1.0
            },
            mark_grounds,
        }
    }

    /// A colour set from outside the theme, as a small mark that has to be seen on it. A
    /// project's `rail` and a user's `accent` are chosen without knowing which theme they
    /// will land on, so the colour moves just far enough to show.
    pub(super) fn mark(&self, color: Rgb) -> Rgba {
        mark_on(self.mark_grounds, color)
    }
}

/// `color` moved far enough to show on each ground. Grounds of one theme lie on the same
/// side of a mark, so moving for one never undoes the other.
fn mark_on(grounds: [Rgb; 2], color: Rgb) -> Rgba {
    opaque(grounds.into_iter().fold(color, |color, ground| {
        crate::terminal::color::with_minimum_contrast(color, ground, MinimumContrast::NON_TEXT)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::{TerminalConfig, ThemeName};

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
    fn a_higher_surface_casts_a_longer_softer_shadow() {
        let reach = |level: Elevation| {
            level
                .shadows(1.0)
                .iter()
                .map(|shadow| (shadow.offset.y, shadow.blur_radius))
                .fold((gpui::px(0.0), gpui::px(0.0)), |far, next| {
                    (far.0.max(next.0), far.1.max(next.1))
                })
        };
        let (raised, floating, modal) = (
            reach(Elevation::Raised),
            reach(Elevation::Floating),
            reach(Elevation::Modal),
        );
        assert!(raised.0 < floating.0 && floating.0 < modal.0);
        assert!(raised.1 < floating.1 && floating.1 < modal.1);
    }

    fn ink_of(color: Rgba) -> Rgb {
        let channel = |value: f32| (value * 255.0).round() as u8;
        Rgb(channel(color.r), channel(color.g), channel(color.b))
    }

    fn theme_named(name: ThemeName) -> TerminalTheme {
        TerminalTheme::from_config(&TerminalConfig::default().with_theme(name.colors()))
    }

    #[test]
    fn a_light_theme_casts_a_softer_shadow_than_a_dark_one() {
        let strength = |name| ChromeColors::from_theme(&theme_named(name)).shadow_strength;
        assert_eq!(strength(ThemeName::Graphite), 1.0);
        assert!(strength(ThemeName::Paper) < 1.0);
        let alpha = |strength| Elevation::Modal.shadows(strength)[0].color.a;
        assert!(alpha(strength(ThemeName::Paper)) < alpha(1.0));
    }

    #[test]
    fn a_mark_from_outside_the_theme_shows_on_every_theme() {
        // Pale and deep on purpose: each disappears on one family of themes. The mid grey
        // clears the sidebar but not the lighter active row it is painted on.
        let outsiders = [
            Rgb(0x89, 0xb4, 0xfa),
            Rgb(0x10, 0x20, 0x40),
            Rgb(0x66, 0x66, 0x66),
        ];
        for outsider in outsiders {
            for name in ThemeName::ALL {
                let theme = theme_named(name);
                let colors = ChromeColors::from_theme(&theme);
                let ink = ink_of(colors.mark(outsider));
                for (fill, ground) in [
                    ("sidebar", theme.title_surface),
                    ("active row", ink_of(colors.state)),
                ] {
                    let contrast = color::contrast_ratio(ink, ground);
                    assert!(
                        contrast >= 3.0,
                        "{outsider:?} on {} {fill}: {contrast}",
                        name.id()
                    );
                }
            }
        }
    }

    #[test]
    fn a_mark_shows_on_the_active_row_of_a_translucent_window() {
        for name in ThemeName::ALL {
            let theme = TerminalTheme::from_config(&TerminalConfig {
                terminal_transparency_percent: 65,
                ..TerminalConfig::default().with_theme(name.colors())
            });
            let colors = ChromeColors::from_theme(&theme);
            let ink = ink_of(colors.mark(Rgb(0x66, 0x66, 0x66)));
            let row = color::blend(theme.surface, theme.raised_surface, colors.state.a);
            for ground in [theme.surface, row] {
                let contrast = color::contrast_ratio(ink, ground);
                assert!(contrast >= 3.0, "on {}: {contrast}", name.id());
            }
        }
    }

    #[test]
    fn a_mark_that_already_shows_keeps_the_colour_it_was_given() {
        let colors = ChromeColors::from_theme(&theme_named(ThemeName::CatppuccinMocha));
        let rail = Rgb(0x89, 0xb4, 0xfa);
        assert_eq!(colors.mark(rail), opaque(rail));
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
    fn a_control_on_a_filled_row_shows_against_it_at_the_rows_own_opacity() {
        for percent in [0, 30, 100] {
            let colors = ChromeColors::from_theme(&theme_with_transparency(percent));
            let color = |fill: Rgba| (fill.r, fill.g, fill.b);
            assert_ne!(color(colors.state_on_row), color(colors.state));
            assert_eq!(colors.state_on_row.a, colors.state.a);
        }
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
