use super::config::{Rgb as ConfigRgb, TerminalConfig};
use super::model::{Color, Rgb};
use super::sizing::CellSize;
use gpui::{
    Font, FontFallbacks, Pixels, Point, Size, TextSystem, WindowBackgroundAppearance, font, point,
    px,
};

#[cfg(test)]
const TERMINAL_FONT_FAMILY: &str = "MonaspiceNe Nerd Font";
#[cfg(test)]
const TERMINAL_FONT_SIZE: f32 = 16.0;
#[cfg(test)]
const TERMINAL_LINE_HEIGHT_MULTIPLIER: f32 = 1.2;
#[cfg(test)]
const TERMINAL_FALLBACKS: &[&str] = &[
    // Prefer the configured terminal family, then use stable platform fallbacks.
    "Noto Color Emoji",
    "Monaspace Neon",
    "SF Mono",
    "Menlo",
    "Monaco",
    "Apple Symbols",
];

#[cfg(test)]
fn terminal_font() -> Font {
    font_from_config(&TerminalConfig::default())
}

fn font_from_config(config: &TerminalConfig) -> Font {
    let mut font = font(config.font.family.clone());
    font.fallbacks = Some(FontFallbacks::from_fonts(config.font.fallbacks.clone()));
    font
}

/// Presentation values are intentionally independent from Rio and replaceable
/// by another host renderer without changing the terminal session.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTheme {
    /// Alpha shared by terminal background fills and structural chrome, so the
    /// window reads as one material. Glyphs, overlays, and floating panels
    /// remain opaque.
    pub background_opacity: f32,
    pub surface: Rgb,
    pub title_surface: Rgb,
    pub title_text: Rgb,
    pub text: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    /// Passive selection uses a cool, readable tint. It is painted above cell
    /// backgrounds and below glyphs by host renderers.
    pub selection: Rgb,
    /// The active visible search result remains distinct from selection where
    /// the two ranges overlap.
    pub current_search_match: Rgb,
    /// Glyphs inside the active search result.
    pub current_search_match_text: Rgb,
    /// The single restrained accent chrome may use for small markers.
    pub accent: Rgb,
    /// Hints and metadata, quieter than `title_text`.
    pub muted_text: Rgb,
    /// Faint line between chrome regions.
    pub divider: Rgb,
    /// One step above the chrome: floating panels and hovered controls.
    pub raised_surface: Rgb,
    /// Hovered rows on a floating panel, legible beneath muted metadata.
    pub hover_surface: Rgb,
    pub ansi: [Rgb; 16],
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self::from_config(&TerminalConfig::default())
    }
}

impl TerminalTheme {
    pub fn from_config(config: &TerminalConfig) -> Self {
        let theme = &config.theme;
        let rgb = |color: ConfigRgb| Rgb(color.0, color.1, color.2);
        Self {
            background_opacity: 1.0 - f32::from(config.terminal_transparency_percent) / 100.0,
            surface: rgb(theme.surface),
            title_surface: rgb(theme.title_surface),
            title_text: rgb(theme.title_text),
            text: rgb(theme.text),
            cursor: rgb(theme.cursor),
            cursor_text: rgb(theme.cursor_text),
            selection: rgb(theme.selection),
            current_search_match: rgb(theme.current_search_match),
            current_search_match_text: rgb(theme.current_search_match_text),
            accent: rgb(theme.accent),
            muted_text: rgb(theme.muted_text),
            divider: rgb(theme.divider),
            raised_surface: rgb(theme.raised_surface),
            hover_surface: rgb(theme.hover_surface),
            ansi: theme.ansi.map(rgb),
        }
    }

    /// How the platform should composite the window behind this theme. A fully
    /// opaque window lets the compositor skip whatever is behind it.
    pub fn window_background(&self) -> WindowBackgroundAppearance {
        if self.background_opacity >= 1.0 {
            WindowBackgroundAppearance::Opaque
        } else {
            WindowBackgroundAppearance::Blurred
        }
    }

    pub fn resolve(&self, color: Color) -> Rgb {
        match color {
            Color::DefaultForeground => self.text,
            Color::DefaultBackground => self.surface,
            Color::Indexed(index) => self.indexed(index),
            Color::Rgb(rgb) => rgb,
        }
    }

    fn indexed(&self, index: u8) -> Rgb {
        if index < 16 {
            return self.ansi[index as usize];
        }
        if index >= 232 {
            let value = 8 + (index - 232) * 10;
            return Rgb(value, value, value);
        }
        let n = index - 16;
        let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        Rgb(channel(n / 36), channel((n / 6) % 6), channel(n % 6))
    }
}

/// Device pixels per logical pixel for the window being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleFactor(f32);

impl ScaleFactor {
    /// A logical length that a float error leaves this far short of a whole device
    /// pixel still counts as reaching it.
    const WHOLE_PIXEL_TOLERANCE: f32 = 0.001;

    /// A platform reporting a nonsensical scale is treated as an unscaled display.
    pub fn new(scale: f32) -> Self {
        Self(if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        })
    }

    /// How many whole device pixels fit in a logical length, and at least one.
    pub fn whole_device_pixels(self, logical: Pixels) -> u32 {
        (f32::from(logical) * self.0 + Self::WHOLE_PIXEL_TOLERANCE)
            .floor()
            .max(1.0) as u32
    }

    /// The logical length of a run of device pixels.
    pub fn logical(self, device_pixels: u32) -> Pixels {
        px(device_pixels as f32 / self.0)
    }

    /// The logical position of a fractional device pixel coordinate.
    pub fn logical_position(self, device_position: f32) -> Pixels {
        px(device_position / self.0)
    }

    /// The nearest position that falls on a device pixel.
    pub fn snap(self, position: Point<Pixels>) -> Point<Pixels> {
        let snap = |value: Pixels| px((f32::from(value) * self.0).round() / self.0);
        point(snap(position.x), snap(position.y))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalMetrics {
    pub font: Font,
    pub font_size: Pixels,
    /// Raw font-derived geometry in logical pixels. It is only ever read through
    /// `device_cell`, so painting and Rio's resize boundary share one grid.
    pub cell: Size<Pixels>,
    pub scale: ScaleFactor,
}

impl TerminalMetrics {
    pub fn from_config(
        text_system: &TextSystem,
        config: &TerminalConfig,
        scale: ScaleFactor,
    ) -> Self {
        // Match the user's terminal stack explicitly. If the primary family is
        // unavailable, GPUI still resolves its robust platform fallback stack.
        let font = font_from_config(config);
        let font_id = text_system.resolve_font(&font);
        let font_size = px(config.font.size_px);
        let width = text_system
            .ch_advance(font_id, font_size)
            .unwrap_or(px(9.0));
        // Terminal line geometry is a presentation contract, not an accident
        // of a selected font's ascent/descent metrics. It must remain stable
        // across fallback glyphs and match the PTY grid after snapping.
        let height = terminal_line_height(font_size, config.font.line_height_multiplier);
        Self {
            font,
            font_size,
            cell: Size { width, height },
            scale,
        }
    }

    /// The cell in whole device pixels: what the PTY is told and the grid is built on.
    pub fn device_cell(&self) -> CellSize {
        device_cell(self.cell, self.scale)
    }

    /// The same cell in the logical pixels GPUI lays out and paints in.
    pub fn render_cell(&self) -> Size<Pixels> {
        let cell = self.device_cell();
        Size {
            width: self.scale.logical(cell.width),
            height: self.scale.logical(cell.height),
        }
    }
}

fn terminal_line_height(font_size: Pixels, multiplier: f32) -> Pixels {
    px((f32::from(font_size) * multiplier).max(1.0))
}

/// Rounds a cell to the nearest whole device pixels, rather than always up: a 9.6px
/// advance is 19px at 2x, which sits the text as the font intended, where rounding
/// up to a whole logical pixel would space every column out by 0.4px.
fn device_cell(cell: Size<Pixels>, scale: ScaleFactor) -> CellSize {
    let device = |length: Pixels| (f32::from(length) * scale.0).round().max(1.0) as u32;
    CellSize {
        width: device(cell.width),
        height: device(cell.height),
    }
}

pub fn gpui_rgb(rgb: Rgb) -> u32 {
    ((rgb.0 as u32) << 16) | ((rgb.1 as u32) << 8) | rgb.2 as u32
}

#[cfg(test)]
mod tests {
    use super::super::sizing::terminal_size;
    use super::*;

    fn metrics(scale: f32) -> TerminalMetrics {
        TerminalMetrics {
            font: terminal_font(),
            font_size: px(TERMINAL_FONT_SIZE),
            cell: Size {
                width: px(9.6),
                height: terminal_line_height(
                    px(TERMINAL_FONT_SIZE),
                    TERMINAL_LINE_HEIGHT_MULTIPLIER,
                ),
            },
            scale: ScaleFactor::new(scale),
        }
    }

    #[test]
    fn a_cell_snaps_to_device_pixels_not_logical_ones() {
        let snapped = |scale| {
            let metrics = metrics(scale);
            (metrics.device_cell(), metrics.render_cell().width)
        };
        let (cell, width) = snapped(2.0);
        assert_eq!(
            cell,
            CellSize {
                width: 19,
                height: 38
            }
        );
        assert_eq!(width, px(9.5));
        let (cell, width) = snapped(1.0);
        assert_eq!(
            cell,
            CellSize {
                width: 10,
                height: 19
            }
        );
        assert_eq!(width, px(10.0));
    }

    #[test]
    fn a_fractional_scale_keeps_the_renderer_and_pty_on_one_grid() {
        let metrics = metrics(1.5);
        let cell = metrics.device_cell();
        let surface = Size {
            width: px(900.0),
            height: px(600.0),
        };
        let grid = terminal_size(
            metrics.scale.whole_device_pixels(surface.width),
            metrics.scale.whole_device_pixels(surface.height),
            cell,
        );
        let painted = metrics.render_cell();
        // Everything the PTY was told about is painted, and it all fits.
        assert!(f32::from(painted.width) * f32::from(grid.cols) <= f32::from(surface.width));
        assert!(f32::from(painted.height) * f32::from(grid.rows) <= f32::from(surface.height));
        // Every column boundary lands on a whole device pixel.
        for column in 0..grid.cols {
            let x = f32::from(painted.width) * f32::from(column) * 1.5;
            assert!((x - x.round()).abs() < 0.01, "column {column} is at {x}");
        }
        assert_eq!(grid.pixels_width, u32::from(grid.cols) * cell.width);
    }

    #[test]
    fn a_tiny_cell_is_still_a_cell() {
        let cell = device_cell(
            Size {
                width: px(0.1),
                height: px(0.1),
            },
            ScaleFactor::new(1.0),
        );
        assert_eq!(
            cell,
            CellSize {
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn a_length_short_of_a_device_pixel_only_by_float_error_reaches_it() {
        let scale = ScaleFactor::new(1.25);
        assert_eq!(scale.whole_device_pixels(px(799.9999)), 1000);
        assert_eq!(scale.whole_device_pixels(px(799.5)), 999);
        assert_eq!(scale.whole_device_pixels(px(0.0)), 1);
    }

    #[test]
    fn a_nonsensical_scale_falls_back_to_unscaled() {
        for scale in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(ScaleFactor::new(scale), ScaleFactor::new(1.0));
        }
    }

    #[test]
    fn the_grid_origin_snaps_onto_a_device_pixel() {
        let snapped = ScaleFactor::new(2.0).snap(point(px(14.3), px(14.2)));
        assert_eq!(snapped, point(px(14.5), px(14.0)));
    }

    #[test]
    fn terminal_font_preserves_nerd_and_emoji_fallback_order() {
        let font = terminal_font();
        assert_eq!(font.family.as_ref(), TERMINAL_FONT_FAMILY);
        assert_eq!(
            font.fallbacks
                .expect("terminal font needs fallbacks")
                .fallback_list(),
            TERMINAL_FALLBACKS
        );
    }

    #[test]
    fn configured_font_family_reaches_the_renderer() {
        let mut config = TerminalConfig::default();
        config.font.family = "Monaspace Neon".into();
        let font = font_from_config(&config);
        assert_eq!(font.family.as_ref(), "Monaspace Neon");
        assert_eq!(
            font.fallbacks
                .expect("terminal font needs fallbacks")
                .fallback_list(),
            TERMINAL_FALLBACKS
        );
    }

    #[test]
    fn configured_theme_preserves_semantic_colour_resolution() {
        let mut config = TerminalConfig::default();
        config.theme.text = crate::terminal::config::Rgb(1, 2, 3);
        config.theme.surface = crate::terminal::config::Rgb(4, 5, 6);
        let theme = TerminalTheme::from_config(&config);
        assert_eq!(theme.resolve(Color::DefaultForeground), Rgb(1, 2, 3));
        assert_eq!(theme.resolve(Color::DefaultBackground), Rgb(4, 5, 6));
    }

    #[test]
    fn configured_chrome_and_overlay_colours_reach_the_theme() {
        let mut config = TerminalConfig::default();
        config.theme.selection = ConfigRgb(1, 1, 1);
        config.theme.current_search_match = ConfigRgb(2, 2, 2);
        config.theme.current_search_match_text = ConfigRgb(8, 8, 8);
        config.theme.accent = ConfigRgb(3, 3, 3);
        config.theme.muted_text = ConfigRgb(4, 4, 4);
        config.theme.divider = ConfigRgb(5, 5, 5);
        config.theme.raised_surface = ConfigRgb(6, 6, 6);
        config.theme.hover_surface = ConfigRgb(7, 7, 7);
        let theme = TerminalTheme::from_config(&config);
        assert_eq!(theme.selection, Rgb(1, 1, 1));
        assert_eq!(theme.current_search_match, Rgb(2, 2, 2));
        assert_eq!(theme.current_search_match_text, Rgb(8, 8, 8));
        assert_eq!(theme.accent, Rgb(3, 3, 3));
        assert_eq!(theme.muted_text, Rgb(4, 4, 4));
        assert_eq!(theme.divider, Rgb(5, 5, 5));
        assert_eq!(theme.raised_surface, Rgb(6, 6, 6));
        assert_eq!(theme.hover_surface, Rgb(7, 7, 7));
    }

    #[test]
    fn default_theme_is_catppuccin_mocha() {
        let theme = TerminalTheme::default();
        assert_eq!(theme.background_opacity, 1.0);
        assert_eq!(theme.surface, Rgb(30, 30, 46));
        assert_eq!(theme.title_surface, Rgb(24, 24, 37));
        assert_eq!(theme.text, Rgb(205, 214, 244));
        assert_eq!(theme.cursor, Rgb(245, 224, 230));
        assert_eq!(theme.ansi[2], Rgb(166, 227, 161));
        assert_eq!(theme.ansi[12], Rgb(137, 180, 250));
    }

    #[test]
    fn only_full_opacity_gives_the_compositor_an_opaque_window() {
        let theme = |terminal_transparency_percent| {
            TerminalTheme::from_config(&TerminalConfig {
                terminal_transparency_percent,
                ..TerminalConfig::default()
            })
        };
        assert_eq!(
            theme(0).window_background(),
            WindowBackgroundAppearance::Opaque
        );
        for percent in [1, 100] {
            assert_eq!(
                theme(percent).window_background(),
                WindowBackgroundAppearance::Blurred
            );
        }
    }

    #[test]
    fn transparency_changes_background_opacity_but_not_text_or_cursor() {
        let config = TerminalConfig {
            terminal_transparency_percent: 65,
            ..TerminalConfig::default()
        };
        let theme = TerminalTheme::from_config(&config);
        assert!((theme.background_opacity - 0.35).abs() < f32::EPSILON);
        assert_eq!(theme.text, Rgb(205, 214, 244));
        assert_eq!(theme.cursor, Rgb(245, 224, 230));
    }
}
