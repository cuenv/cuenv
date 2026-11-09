//! Window chrome motion and hover feedback.

use super::super::engine::TerminalView;
use super::super::motion::{HoverTarget, MotionPace, PANEL_FADE_IN};
use crate::integrations::cuenv::Keybindings;
use gpui::{
    Animation, AnimationExt, App, Context, DispatchPhase, Div, Hitbox, HitboxBehavior, IntoElement,
    MouseMoveEvent, Styled, Window,
};

/// `panel` fading in as it first appears, which starts again each time it reopens because
/// GPUI forgets the animation of an element that has left the screen. At a reduced pace it
/// is simply there. Keys typed into it meanwhile are not held up: it is already live.
pub(super) fn fading_in(panel: Div, id: &'static str, pace: MotionPace) -> gpui::AnyElement {
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
pub(super) fn hover_fade(
    target: HoverTarget,
    cx: &mut Context<TerminalView>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |view, hovered: &bool, _window, cx| {
        let pointer = if *hovered {
            Pointer::Over
        } else {
            Pointer::Away
        };
        if fade_hover(view, target, pointer) {
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
pub(super) fn hover_probe(target: HoverTarget, cx: &Context<TerminalView>) -> impl IntoElement {
    let view = cx.weak_entity();
    gpui::canvas(
        |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_bounds, hitbox, window, cx| {
            let pointer =
                if hitbox.is_hovered(window) && window.is_window_hovered() && !cx.has_active_drag()
                {
                    Pointer::Over
                } else {
                    Pointer::Away
                };
            let news = view
                .update(cx, |view, _cx| fade_hover(view, target, pointer))
                .unwrap_or(false);
            if news {
                super::super::engine::redraw_after_this_frame(view, cx);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// Lights `target` while the pointer is anywhere over the element this is the last child
/// of, children included. Chrome floating over the terminal keeps the pointer to itself
/// so a click there starts no selection, which also hides it from its parent's own hover,
/// so this sits above the children and only watches. GPUI draws nothing new for a pointer
/// moving over an element without listeners of its own, so a move that crosses its edge
/// asks for the draw that settles the hover.
pub(super) fn hover_region(target: HoverTarget, cx: &Context<TerminalView>) -> impl IntoElement {
    let view = cx.weak_entity();
    gpui::canvas(
        |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_bounds, hitbox, window, cx| {
            let over = |hitbox: &Hitbox, window: &Window| {
                hitbox.is_hovered(window) && window.is_window_hovered()
            };
            let was_over = over(&hitbox, window);
            let pointer = if was_over {
                Pointer::Over
            } else {
                Pointer::Away
            };
            let news = view
                .update(cx, |view, _cx| fade_hover(view, target, pointer))
                .unwrap_or(false);
            let watcher = view.clone();
            window.on_mouse_event(move |_event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && over(&hitbox, window) != was_over {
                    let _ = watcher.update(cx, |_view, cx| cx.notify());
                }
            });
            if news {
                super::super::engine::redraw_after_this_frame(view, cx);
            }
        },
    )
    .absolute()
    .inset_0()
}

impl TerminalView {
    /// How lit `target` is under the pointer in this draw.
    pub(super) fn hover_level(&self, target: HoverTarget) -> f32 {
        self.motion.hover_level(target, std::time::Instant::now())
    }

    /// The terminal's own monospaced family, which chrome uses for paths and
    /// shortcuts so they read the way they do in the terminal.
    pub(super) fn mono_font_family(&self) -> gpui::SharedString {
        self.metrics.font.family.clone()
    }

    /// The active tab's command palette chord as shown in hints.
    pub(super) fn palette_chord_label(&self) -> String {
        self.active_state().map_or_else(
            || Keybindings::default().palette.label(),
            |state| state.cuenv.presentation.keybindings.palette.label(),
        )
    }
}
