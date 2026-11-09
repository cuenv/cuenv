//! Window chrome: the title strip, tab rail, and command palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod clamp;
mod command_palette;
mod field;
mod hover;
mod icons;
mod layout;
mod project_panel;
mod rail;
mod scrollbar;
mod search_bar;
mod session_state;
mod tokens;
mod tooltip;

pub(super) use icons::ChromeAssets;
pub(super) use layout::{ShellPanels, exit_notice_height, render_exit_notice, sidebar_width};
pub(super) use scrollbar::{ScrollbarRequest, track_of};
pub(super) use session_state::SessionProblem;
pub(super) use tokens::{UNFOCUSED_TERMINAL_FADE, resolve_ui_font, traffic_light_position};
