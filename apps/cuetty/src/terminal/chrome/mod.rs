//! Window chrome: the title strip, tab rail, task sidebar, and task palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod clamp;
mod field;
mod icons;
mod layout;
mod rail;
mod search_bar;
mod session_state;
mod task_palette;
mod task_sidebar;
mod tokens;
mod tooltip;

pub(super) use icons::ChromeAssets;
pub(super) use layout::{ShellPanels, exit_notice_height, render_exit_notice, sidebar_width};
pub(super) use session_state::SessionProblem;
pub(super) use tokens::{UNFOCUSED_TERMINAL_FADE, resolve_ui_font, traffic_light_position};

use super::engine::TerminalView;
use super::motion::{HoverTarget, MotionPace, PANEL_FADE_IN};
use crate::integrations::cuenv::Keybindings;
use gpui::{Animation, AnimationExt, App, Context, Div, IntoElement, Styled, Window};

/// `panel` fading in as it first appears, which starts again each time it reopens because
/// GPUI forgets the animation of an element that has left the screen. At a reduced pace it
/// is simply there. Keys typed into it meanwhile are not held up: it is already live.
fn fading_in(panel: Div, id: &'static str, pace: MotionPace) -> gpui::AnyElement {
    match pace.duration(PANEL_FADE_IN) {
        duration if duration.is_zero() => panel.into_any_element(),
        duration => panel
            .with_animation(id, Animation::new(duration), |panel, delta| {
                panel.opacity(delta)
            })
            .into_any_element(),
    }
}

/// A listener for `.on_hover` that fades `target`'s hover in and out.
fn hover_fade(
    target: HoverTarget,
    cx: &mut Context<TerminalView>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |view, hovered: &bool, _window, cx| {
        let pace = view.motion_pace();
        let now = std::time::Instant::now();
        if *hovered {
            view.motion.hover_on(target, pace, now);
        } else {
            view.motion.hover_off(target, pace, now);
        }
        cx.notify();
    })
}

impl TerminalView {
    /// How lit `target` is under the pointer in this draw.
    fn hover_level(&self, target: HoverTarget) -> f32 {
        self.motion.hover_level(target, std::time::Instant::now())
    }

    /// The terminal's own monospaced family, which chrome uses for paths and
    /// shortcuts so they read the way they do in the terminal.
    fn mono_font_family(&self) -> gpui::SharedString {
        self.metrics.font.family.clone()
    }

    /// The active tab's task palette chord as shown in hints.
    fn palette_chord_label(&self) -> String {
        self.active_state().map_or_else(
            || Keybindings::default().palette.label(),
            |state| state.cuenv.presentation.keybindings.palette.label(),
        )
    }
}
