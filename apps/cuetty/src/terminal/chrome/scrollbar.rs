//! The overlay scrollbar: a thin thumb in the terminal's gutter, there while the view
//! moves or the pointer comes looking for it, and draggable.

use super::tokens::{ChromeColors, faded, radius, size};
use super::{hover_fade, hover_probe};
use crate::terminal::engine::{TerminalView, redraw_after_this_frame, store_if_changed};
use crate::terminal::motion::HoverTarget;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::scrollbar::{ScrollExtent, Track};
use gpui::StatefulInteractiveElement;
use gpui::{
    Context, DispatchPhase, InteractiveElement, IntoElement, MouseButton, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Styled, canvas, div, px,
};

/// The track for a height the surface has measured.
pub(in crate::terminal) fn track_of(height: f32) -> Track {
    Track {
        height,
        min_thumb: f32::from(size::SCROLLBAR_MIN_THUMB),
    }
}

/// What a scrollbar is drawn from.
pub(in crate::terminal) struct ScrollbarRequest<'a> {
    /// `None` where there is nothing to scroll.
    pub(in crate::terminal) extent: Option<ScrollExtent>,
    /// Its room above and below, which are the terminal's own insets so that its ends line
    /// up with the first and last rows.
    pub(in crate::terminal) top: Pixels,
    pub(in crate::terminal) bottom: Pixels,
    pub(in crate::terminal) theme: &'a TerminalTheme,
}

impl TerminalView {
    /// The scrollbar over the terminal, where there is history to scroll. Its hit area
    /// stays in place while the thumb is faded away, so moving the pointer to the edge
    /// is enough to bring it back. The task palette covers it while it is open. The task
    /// sidebar, inset from the surface's edge and worked around, leaves it clear.
    pub(in crate::terminal) fn render_scrollbar(
        &self,
        request: ScrollbarRequest<'_>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let ScrollbarRequest {
            extent,
            top,
            bottom,
            theme,
        } = request;
        let colors = ChromeColors::from_theme(theme);
        let extent = extent.filter(|_| !self.task_palette_open)?;
        let (track_top, track_height) = self.scrollbar_track();
        let thumb = extent.thumb(track_of(track_height));
        let now = std::time::Instant::now();
        let dragging = self.scrollbar_grab.is_some();
        let hover = self.hover_level(HoverTarget::Scrollbar);
        let level = if dragging {
            1.0
        } else {
            self.motion
                .scrollbar_level(now, self.motion_pace())
                .max(hover)
        };
        // Stronger the nearer the pointer has it to hand.
        let resting = colors.scrollbar.a;
        let strength = if dragging {
            1.0
        } else {
            resting + (1.0 - resting) * hover
        };
        let fill = faded(colors.scrollbar_active, level * strength);
        let track = self.scrollbar_track_handle();
        let entity = cx.weak_entity();
        let measured_by = cx.weak_entity();
        let on_press = cx.listener(move |view, event: &gpui::MouseDownEvent, _window, cx| {
            let pointer = f32::from(event.position.y) - track_top;
            view.begin_scrollbar_drag(extent.grab_at(pointer, track_of(track_height)), cx);
            view.drag_scrollbar_to(pointer, cx);
            cx.stop_propagation();
        });
        Some(
            div()
                .id("cuetty-scrollbar")
                .absolute()
                .top(top)
                .bottom(bottom)
                .right_0()
                .w(size::SCROLLBAR_HIT_WIDTH)
                // The press belongs to the thumb and starts no selection beneath it.
                .occlude()
                .on_hover(hover_fade(HoverTarget::Scrollbar, cx))
                .child(hover_probe(HoverTarget::Scrollbar, cx))
                .on_mouse_down(MouseButton::Left, on_press)
                .on_scroll_wheel(cx.listener(Self::scroll_wheel))
                .child(
                    canvas(
                        move |bounds, _window, app| {
                            let next = (
                                f32::from(bounds.origin.y),
                                f32::from(bounds.size.height).max(1.0),
                            );
                            if store_if_changed(&track, next) {
                                redraw_after_this_frame(measured_by, app);
                            }
                        },
                        // A drag belongs to the whole window once it has started: the pointer
                        // does not have to stay on a strip this narrow. These last for one draw,
                        // and every draw during a drag registers them again.
                        move |_bounds, _state, window, _app| {
                            if !dragging {
                                return;
                            }
                            let moved = entity.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _w, app| {
                                if phase == DispatchPhase::Capture {
                                    let _ = moved.update(app, |view, cx| {
                                        // A release that never reached the window.
                                        if !event.dragging() {
                                            view.end_scrollbar_drag(cx);
                                            return;
                                        }
                                        let pointer = f32::from(event.position.y) - track_top;
                                        view.drag_scrollbar_to(pointer, cx);
                                    });
                                }
                            });
                            let released = entity.clone();
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _w, app| {
                                if phase == DispatchPhase::Capture {
                                    let _ = released
                                        .update(app, |view, cx| view.end_scrollbar_drag(cx));
                                }
                            });
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(thumb.top))
                        .h(px(thumb.height))
                        .right(size::SCROLLBAR_INSET)
                        .w(size::SCROLLBAR_THUMB_WIDTH)
                        .rounded(radius::CONTROL)
                        .bg(fill),
                )
                .into_any_element(),
        )
    }
}
