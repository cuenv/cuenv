//! The overlay scrollbar: a thin thumb in the terminal's gutter, there while the view
//! moves or the pointer comes looking for it in the gutter, and draggable.

use super::tokens::{ChromeColors, faded, radius, size};
use super::{hover_fade, hover_probe};
use crate::terminal::engine::{TerminalView, redraw_after_this_frame, store_if_changed};
use crate::terminal::motion::HoverTarget;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::scrollbar::{ScrollExtent, Track};
use gpui::StatefulInteractiveElement;
use gpui::{
    Context, DispatchPhase, Div, InteractiveElement, IntoElement, MouseButton, MouseMoveEvent,
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
    /// The room beside the grid, which padding may leave at nothing.
    pub(in crate::terminal) gutter: Pixels,
    pub(in crate::terminal) theme: &'a TerminalTheme,
}

/// Whether the part of the thumb beyond the gutter takes a press or leaves it to the grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThumbPress {
    Taken,
    LeftToTerminal,
}

/// What a held button is already doing, which the scrollbar must not get in the way of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerGesture {
    Idle,
    DraggingScrollbar,
    SelectingText,
}

/// How much of the thumb shows in this draw, and whether it takes a press there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ThumbReveal {
    level: f32,
    press: ThumbPress,
}

/// A pointer on its way across the grid has not asked for anything, so only the view having
/// just moved opens the thumb to a press there. The pointer's hover shows the thumb but
/// never opens it, and a selection under way must keep reaching the grid beneath, because
/// the press area would stop the terminal hearing the pointer move.
fn thumb_reveal(moved: f32, hover: f32, gesture: PointerGesture) -> ThumbReveal {
    match gesture {
        PointerGesture::DraggingScrollbar => ThumbReveal {
            level: 1.0,
            press: ThumbPress::Taken,
        },
        PointerGesture::SelectingText => ThumbReveal {
            level: moved.max(hover),
            press: ThumbPress::LeftToTerminal,
        },
        PointerGesture::Idle => ThumbReveal {
            level: moved.max(hover),
            press: if moved > 0.0 {
                ThumbPress::Taken
            } else {
                ThumbPress::LeftToTerminal
            },
        },
    }
}

/// The widths, from the surface's right edge, of what takes a press. Over the grid a press
/// is the terminal's unless it lands on a thumb the view's moving has just shown, because a
/// selection has to be able to start in the last column.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PressWidths {
    /// All along the track, in the gutter beside the grid.
    gutter: f32,
    /// Alongside the thumb alone, where it reaches further than the gutter does.
    thumb: Option<f32>,
}

fn press_widths(gutter: Pixels, thumb: ThumbPress) -> PressWidths {
    let gutter = f32::from(gutter).min(f32::from(size::SCROLLBAR_HIT_WIDTH));
    let reach = f32::from(size::SCROLLBAR_INSET + size::SCROLLBAR_THUMB_WIDTH);
    PressWidths {
        gutter,
        thumb: (thumb == ThumbPress::Taken && reach > gutter).then_some(reach),
    }
}

impl TerminalView {
    /// The scrollbar over the terminal, where there is history to scroll. Reaching the gutter
    /// is enough to bring it back from having faded, and that part takes nothing from what
    /// is beneath it. The task palette covers it while it is open. The task sidebar, inset from
    /// the surface's edge and worked around, leaves it clear.
    pub(in crate::terminal) fn render_scrollbar(
        &self,
        request: ScrollbarRequest<'_>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let ScrollbarRequest {
            extent,
            top,
            bottom,
            gutter,
            theme,
        } = request;
        let colors = ChromeColors::from_theme(theme);
        let extent = extent.filter(|_| !self.task_palette_open)?;
        let (track_top, track_height) = self.scrollbar_track();
        let thumb = extent.thumb(track_of(track_height));
        let now = std::time::Instant::now();
        let dragging = self.scrollbar_grab.is_some();
        // With no gutter there is nowhere to hover, whatever padding last left behind.
        let hover = if gutter > px(0.0) {
            self.hover_level(HoverTarget::Scrollbar)
        } else {
            0.0
        };
        let gesture = if dragging {
            PointerGesture::DraggingScrollbar
        } else if self.active_state().is_some_and(|state| state.selecting()) {
            PointerGesture::SelectingText
        } else {
            PointerGesture::Idle
        };
        let ThumbReveal { level, press } = thumb_reveal(
            self.motion.scrollbar_level(now, self.motion_pace()),
            hover,
            gesture,
        );
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
        let press_area = |area: Div, cx: &mut Context<Self>| {
            area.absolute()
                .right_0()
                // The press belongs to the scrollbar and starts no selection beneath it.
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, event: &gpui::MouseDownEvent, _window, cx| {
                        let pointer = f32::from(event.position.y) - track_top;
                        view.begin_scrollbar_drag(
                            extent.grab_at(pointer, track_of(track_height)),
                            cx,
                        );
                        view.drag_scrollbar_to(pointer, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_scroll_wheel(cx.listener(Self::scroll_wheel))
        };
        let widths = press_widths(gutter, press);
        let gutter_area = (widths.gutter > 0.0)
            .then(|| press_area(div().top_0().bottom_0().w(px(widths.gutter)), cx));
        let thumb_area = widths.thumb.map(|width| {
            press_area(
                div().top(px(thumb.top)).h(px(thumb.height)).w(px(width)),
                cx,
            )
        });
        // Over the gutter alone, so that crossing the last column reveals nothing. Drawn after
        // the press areas so that a pointer over them still reaches it: it blocks nothing, and
        // the areas block only what is beneath the scrollbar.
        let hover_area = (widths.gutter > 0.0).then(|| {
            div()
                .id("cuetty-scrollbar")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(widths.gutter))
                .on_hover(hover_fade(HoverTarget::Scrollbar, cx))
                .child(hover_probe(HoverTarget::Scrollbar, cx))
        });
        Some(
            div()
                .absolute()
                .top(top)
                .bottom(bottom)
                .right_0()
                .w(size::SCROLLBAR_HIT_WIDTH)
                .children(gutter_area)
                .children(thumb_area)
                .children(hover_area)
                .child(
                    div()
                        .absolute()
                        .inset_0()
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
                                    window.on_mouse_event(
                                        move |event: &MouseMoveEvent, phase, _w, app| {
                                            if phase == DispatchPhase::Capture {
                                                let _ = moved.update(app, |view, cx| {
                                                    // A release that never reached the window.
                                                    if !event.dragging() {
                                                        view.end_scrollbar_drag(cx);
                                                        return;
                                                    }
                                                    let pointer =
                                                        f32::from(event.position.y) - track_top;
                                                    view.drag_scrollbar_to(pointer, cx);
                                                });
                                            }
                                        },
                                    );
                                    let released = entity.clone();
                                    window.on_mouse_event(
                                        move |_: &MouseUpEvent, phase, _w, app| {
                                            if phase == DispatchPhase::Capture {
                                                let _ = released.update(app, |view, cx| {
                                                    view.end_scrollbar_drag(cx)
                                                });
                                            }
                                        },
                                    );
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
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_gutter_a_thumb_left_to_the_terminal_takes_no_press_from_the_last_column() {
        assert_eq!(
            press_widths(px(0.0), ThumbPress::LeftToTerminal),
            PressWidths {
                gutter: 0.0,
                thumb: None
            }
        );
    }

    #[test]
    fn with_no_gutter_only_a_thumb_open_to_a_press_takes_one() {
        let widths = press_widths(px(0.0), ThumbPress::Taken);
        assert_eq!(widths.gutter, 0.0);
        assert_eq!(
            widths.thumb,
            Some(f32::from(
                size::SCROLLBAR_INSET + size::SCROLLBAR_THUMB_WIDTH
            ))
        );
    }

    #[test]
    fn a_narrow_gutter_is_taken_whole_and_no_more_until_the_thumb_is_open_to_a_press() {
        let narrow = px(4.0);
        let left = press_widths(narrow, ThumbPress::LeftToTerminal);
        assert_eq!(left.gutter, 4.0);
        assert_eq!(left.thumb, None);
        assert!(press_widths(narrow, ThumbPress::Taken).thumb.is_some());
    }

    #[test]
    fn a_wide_gutter_holds_the_whole_strip_and_the_thumb_with_it() {
        let widths = press_widths(px(64.0), ThumbPress::Taken);
        assert_eq!(widths.gutter, f32::from(size::SCROLLBAR_HIT_WIDTH));
        assert_eq!(widths.thumb, None);
    }

    #[test]
    fn a_thumb_the_hover_alone_revealed_shows_but_takes_nothing_from_the_grid() {
        let reveal = thumb_reveal(0.0, 1.0, PointerGesture::Idle);
        assert_eq!(reveal.level, 1.0);
        assert_eq!(reveal.press, ThumbPress::LeftToTerminal);
        assert_eq!(press_widths(px(0.0), reveal.press).thumb, None);
    }

    #[test]
    fn a_thumb_the_view_just_moved_takes_a_press_until_it_has_faded() {
        let fading = thumb_reveal(0.01, 0.0, PointerGesture::Idle);
        assert_eq!(fading.press, ThumbPress::Taken);
        let gone = thumb_reveal(0.0, 0.0, PointerGesture::Idle);
        assert_eq!(gone.level, 0.0);
        assert_eq!(gone.press, ThumbPress::LeftToTerminal);
    }

    #[test]
    fn a_selection_under_way_keeps_the_grid_whatever_the_thumb_is_doing() {
        let reveal = thumb_reveal(1.0, 1.0, PointerGesture::SelectingText);
        assert_eq!(reveal.level, 1.0);
        assert_eq!(reveal.press, ThumbPress::LeftToTerminal);
    }

    #[test]
    fn a_scrollbar_drag_holds_the_thumb_after_the_view_has_stopped_moving() {
        let reveal = thumb_reveal(0.0, 0.0, PointerGesture::DraggingScrollbar);
        assert_eq!(reveal.level, 1.0);
        assert_eq!(reveal.press, ThumbPress::Taken);
    }
}
