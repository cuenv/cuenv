use super::config::TerminalConfig;
use super::model::{Color, Rgb};
use super::sizing::CellSize;
use gpui::{Font, FontFallbacks, Pixels, Size, TextSystem, font, px};

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

const ANSI_PALETTE: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(205, 49, 49),
    Rgb(13, 188, 121),
    Rgb(229, 229, 16),
    Rgb(36, 114, 200),
    Rgb(188, 63, 188),
    Rgb(17, 168, 205),
    Rgb(229, 229, 229),
    Rgb(102, 102, 102),
    Rgb(241, 76, 76),
    Rgb(35, 209, 139),
    Rgb(245, 245, 67),
    Rgb(59, 142, 234),
    Rgb(214, 112, 214),
    Rgb(41, 184, 219),
    Rgb(255, 255, 255),
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
    pub host_background: Rgb,
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
    pub ansi: [Rgb; 16],
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self {
            host_background: Rgb(20, 19, 23),
            surface: Rgb(27, 26, 31),
            title_surface: Rgb(37, 35, 42),
            title_text: Rgb(207, 201, 214),
            text: Rgb(226, 222, 231),
            cursor: Rgb(226, 222, 231),
            cursor_text: Rgb(27, 26, 31),
            selection: Rgb(64, 91, 135),
            current_search_match: Rgb(156, 108, 44),
            ansi: ANSI_PALETTE,
        }
    }
}

impl TerminalTheme {
    pub fn from_config(config: &TerminalConfig) -> Self {
        let theme = &config.theme;
        Self {
            host_background: Rgb(
                theme.host_background.0,
                theme.host_background.1,
                theme.host_background.2,
            ),
            surface: Rgb(theme.surface.0, theme.surface.1, theme.surface.2),
            title_surface: Rgb(
                theme.title_surface.0,
                theme.title_surface.1,
                theme.title_surface.2,
            ),
            title_text: Rgb(theme.title_text.0, theme.title_text.1, theme.title_text.2),
            text: Rgb(theme.text.0, theme.text.1, theme.text.2),
            cursor: Rgb(theme.cursor.0, theme.cursor.1, theme.cursor.2),
            cursor_text: Rgb(
                theme.cursor_text.0,
                theme.cursor_text.1,
                theme.cursor_text.2,
            ),
            selection: Self::default().selection,
            current_search_match: Self::default().current_search_match,
            ansi: theme.ansi.map(|color| Rgb(color.0, color.1, color.2)),
        }
    }
    pub fn resolve(&self, color: Color, _foreground: bool) -> Rgb {
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

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalMetrics {
    pub font: Font,
    pub font_size: Pixels,
    /// Raw font-derived geometry. `render_cell` snaps this once so rendering
    /// and Rio's resize boundary share exactly the same terminal grid.
    pub cell: Size<Pixels>,
}

impl TerminalMetrics {
    pub fn from_config(text_system: &TextSystem, config: &TerminalConfig) -> Self {
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
        }
    }

    pub fn rio_cell(&self) -> CellSize {
        let cell = self.render_cell();
        CellSize {
            width: f32::from(cell.width) as u32,
            height: f32::from(cell.height) as u32,
        }
    }

    pub fn render_cell(&self) -> Size<Pixels> {
        snapped_cell(self.cell)
    }
}

fn terminal_line_height(font_size: Pixels, multiplier: f32) -> Pixels {
    px((f32::from(font_size) * multiplier).max(1.0))
}

fn snapped_cell(cell: Size<Pixels>) -> Size<Pixels> {
    Size {
        width: px(f32::from(cell.width).ceil().max(1.0)),
        height: px(f32::from(cell.height).ceil().max(1.0)),
    }
}

pub fn gpui_rgb(rgb: Rgb) -> u32 {
    ((rgb.0 as u32) << 16) | ((rgb.1 as u32) << 8) | rgb.2 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_font_metrics_share_one_snapped_renderer_and_rio_cell() {
        let cell = snapped_cell(Size {
            width: px(8.1),
            height: px(15.01),
        });
        assert_eq!(
            cell,
            Size {
                width: px(9.0),
                height: px(16.0)
            }
        );
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
    fn line_height_contract_is_16px_times_1_point_2_then_shared_snap() {
        let raw_height =
            terminal_line_height(px(TERMINAL_FONT_SIZE), TERMINAL_LINE_HEIGHT_MULTIPLIER);
        assert_eq!(raw_height, px(19.2));
        assert_eq!(
            snapped_cell(Size {
                width: px(9.0),
                height: raw_height
            })
            .height,
            px(20.0)
        );
    }

    #[test]
    fn configured_theme_preserves_semantic_colour_resolution() {
        let mut config = TerminalConfig::default();
        config.theme.text = crate::terminal::config::Rgb(1, 2, 3);
        config.theme.surface = crate::terminal::config::Rgb(4, 5, 6);
        let theme = TerminalTheme::from_config(&config);
        assert_eq!(theme.resolve(Color::DefaultForeground, true), Rgb(1, 2, 3));
        assert_eq!(theme.resolve(Color::DefaultBackground, false), Rgb(4, 5, 6));
    }
}
