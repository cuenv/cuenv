//! Window chrome: the title strip, tab rail, task sidebar, and task palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod layout;
mod rail;
mod task_palette;
mod task_sidebar;

use super::model::Rgb;
use super::presentation::{TerminalTheme, gpui_rgb};
use gpui::Rgba;

const CHROME_GLASS_OPACITY: f32 = 0.72;

const TAB_RAIL_WIDTH: f32 = 56.0;
const TAB_RAIL_FOOTER_HEIGHT: f32 = 112.0;
const SHELL_TITLEBAR_HEIGHT: f32 = 34.0;
const TASK_SIDEBAR_WIDTH: f32 = 280.0;

fn color_with_opacity(color: Rgb, opacity: f32) -> Rgba {
    let mut color = gpui::rgb(gpui_rgb(color));
    color.a = opacity.clamp(0.0, 1.0);
    color
}

pub(super) fn terminal_background_color(theme: &TerminalTheme, color: Rgb) -> Rgba {
    color_with_opacity(color, theme.background_opacity)
}

fn chrome_glass_color(color: Rgb) -> Rgba {
    color_with_opacity(color, CHROME_GLASS_OPACITY)
}
