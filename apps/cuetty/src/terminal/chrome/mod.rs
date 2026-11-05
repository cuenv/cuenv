//! Window chrome: the title strip, tab rail, and command palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod clamp;
mod command_palette;
mod field;
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

use super::engine::{TerminalView, redraw_after_this_frame};
use super::motion::{HoverTarget, MotionPace, PANEL_FADE_IN};
use crate::integrations::cuenv::Keybindings;
use gpui::{
    Animation, AnimationExt, App, Context, Div, HitboxBehavior, IntoElement, Styled, Window, canvas,
};

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

/// Where the pointer is, as far as something that can light up is concerned.
#[derive(Clone, Copy)]
enum Pointer {
    Over,
    Away,
}

impl From<bool> for Pointer {
    fn from(over: bool) -> Self {
        if over { Self::Over } else { Self::Away }
    }
}

/// Sets `target`'s hover fading for where the pointer is, and says whether that is news.
fn fade_hover(view: &mut TerminalView, target: HoverTarget, pointer: Pointer) -> bool {
    let pace = view.motion_pace();
    let now = std::time::Instant::now();
    match pointer {
        Pointer::Over => view.motion.hover_on(target, pace, now),
        Pointer::Away => view.motion.hover_off(target, pace, now),
    }
}

/// A listener for `.on_hover` that fades `target`'s hover in and out as the pointer moves.
fn hover_fade(
    target: HoverTarget,
    cx: &mut Context<TerminalView>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |view, hovered: &bool, _window, cx| {
        if fade_hover(view, target, Pointer::from(*hovered)) {
            cx.notify();
        }
    })
}

/// Keeps `target`'s hover true when something other than the pointer moves. GPUI only
/// reports a hover as the pointer moves, so a row scrolled out from under a still pointer,
/// or a button the sidebar carried away from it, would stay lit, with nothing lit in its
/// place. This child of the element asks at each draw whether the pointer is over it, once
/// the draw's layout is known. A pointer that has left the window is over nothing, though
/// GPUI goes on answering from the last place it moved to, and it redraws on that leaving.
/// A draw that changes nothing asks for no other, so an idle window stays idle.
fn hover_probe(target: HoverTarget, cx: &Context<TerminalView>) -> impl IntoElement {
    let view = cx.weak_entity();
    canvas(
        |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_bounds, hitbox, window, cx| {
            let pointer = Pointer::from(
                hitbox.is_hovered(window) && window.is_window_hovered() && !cx.has_active_drag(),
            );
            let news = view
                .update(cx, |view, _cx| fade_hover(view, target, pointer))
                .unwrap_or(false);
            if news {
                redraw_after_this_frame(view, cx);
            }
        },
    )
    .absolute()
    .inset_0()
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

    /// The active tab's command palette chord as shown in hints.
    fn palette_chord_label(&self) -> String {
        self.active_state().map_or_else(
            || Keybindings::default().palette.label(),
            |state| state.cuenv.presentation.keybindings.palette.label(),
        )
    }
}
