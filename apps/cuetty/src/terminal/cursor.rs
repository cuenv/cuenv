//! How the cursor animates: pure timing, so it is tested without a window or a clock.
//!
//! Nothing here runs on a schedule of its own. A draw asks for the current frame, which
//! says when the next one is due, if ever. A cursor that is not animating asks for none,
//! which is what lets an idle window stop drawing entirely.

use super::config::CursorBlink;
use super::model::Known;
use std::time::{Duration, Instant};

/// Whether the cursor belongs to where keys are going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CursorFocus {
    Focused,
    Unfocused,
}

/// How long the cursor stays solid after output or a key press, so it never disappears
/// from under someone typing.
const BLINK_PAUSE: Duration = Duration::from_millis(600);
/// How long it holds fully on, and fully off, each cycle.
const BLINK_HOLD: Duration = Duration::from_millis(410);
/// How long it takes to fade between the two, which is a soft 1.06s cycle in all.
const BLINK_FADE: Duration = Duration::from_millis(120);
/// How many cycles it blinks with nothing happening before it settles, solid, and stops
/// asking for frames: about half a minute.
const BLINK_CYCLES: u32 = 28;
/// The time between frames of a fade.
const ANIMATION_FRAME: Duration = Duration::from_millis(16);

/// Whether the cursor blinks: as the terminal's application asked, unless overridden.
pub(super) fn blink_wanted(policy: CursorBlink, terminal: Known<bool>) -> bool {
    match policy {
        CursorBlink::On => true,
        CursorBlink::Off => false,
        CursorBlink::Terminal => terminal == Known::Known(true),
    }
}

/// How the cursor looks right now, and when that next changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CursorFrame {
    pub opacity: f32,
    /// `None` once nothing will change without something else happening first.
    pub next_frame: Option<Duration>,
}

impl CursorFrame {
    const SOLID: Self = Self {
        opacity: 1.0,
        next_frame: None,
    };
}

/// A blinking cursor, this long after the last output or key press.
fn blink_frame(since_activity: Duration) -> CursorFrame {
    let Some(blinking_for) = since_activity.checked_sub(BLINK_PAUSE) else {
        return CursorFrame {
            opacity: 1.0,
            next_frame: Some(BLINK_PAUSE.saturating_sub(since_activity)),
        };
    };
    let cycle = (BLINK_HOLD + BLINK_FADE) * 2;
    if blinking_for >= cycle * BLINK_CYCLES {
        return CursorFrame::SOLID;
    }
    let phase = Duration::from_nanos((blinking_for.as_nanos() % cycle.as_nanos()) as u64);
    let half = BLINK_HOLD + BLINK_FADE;
    let (visible_at_start, into_half) = match phase.checked_sub(half) {
        Some(into_second_half) => (false, into_second_half),
        None => (true, phase),
    };
    match into_half.checked_sub(BLINK_HOLD) {
        None => CursorFrame {
            opacity: if visible_at_start { 1.0 } else { 0.0 },
            next_frame: Some(BLINK_HOLD - into_half),
        },
        Some(into_fade) => {
            let progress = into_fade.as_secs_f32() / BLINK_FADE.as_secs_f32();
            let eased = (1.0 + (std::f32::consts::PI * progress).cos()) / 2.0;
            CursorFrame {
                opacity: if visible_at_start { eased } else { 1.0 - eased },
                next_frame: Some(ANIMATION_FRAME.min(BLINK_FADE - into_fade)),
            }
        }
    }
}

/// What a draw knows about the cursor it is about to paint.
#[derive(Debug, Clone, Copy)]
pub(super) struct CursorObservation {
    pub focus: CursorFocus,
    pub blink: bool,
    pub now: Instant,
}

/// Remembers what the cursor's animation is timed from.
#[derive(Debug, Clone)]
pub(super) struct CursorAnimator {
    last_activity: Instant,
    focus: CursorFocus,
}

impl CursorAnimator {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            focus: CursorFocus::Focused,
        }
    }

    /// Output or a key press: keep the cursor solid for a moment and blink from there.
    pub(super) fn touch(&mut self, now: Instant) {
        self.last_activity = now;
    }

    pub(super) fn frame(&mut self, seen: CursorObservation) -> CursorFrame {
        // Coming back to a window starts the cycle from solid, so the cursor is easy
        // to find rather than caught mid-blink.
        if seen.focus == CursorFocus::Focused && self.focus == CursorFocus::Unfocused {
            self.touch(seen.now);
        }
        self.focus = seen.focus;
        if seen.focus == CursorFocus::Focused && seen.blink {
            blink_frame(seen.now.saturating_duration_since(self.last_activity))
        } else {
            CursorFrame::SOLID
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn cycle() -> Duration {
        (BLINK_HOLD + BLINK_FADE) * 2
    }

    #[test]
    fn the_terminal_decides_unless_the_setting_overrides_it() {
        let wants = |policy, terminal| blink_wanted(policy, terminal);
        assert!(wants(CursorBlink::Terminal, Known::Known(true)));
        assert!(!wants(CursorBlink::Terminal, Known::Known(false)));
        assert!(!wants(CursorBlink::Terminal, Known::Unknown));
        assert!(wants(CursorBlink::On, Known::Known(false)));
        assert!(!wants(CursorBlink::Off, Known::Known(true)));
    }

    #[test]
    fn the_cursor_stays_solid_right_after_activity() {
        assert_eq!(blink_frame(ms(0)).opacity, 1.0);
        assert_eq!(blink_frame(BLINK_PAUSE - ms(1)).opacity, 1.0);
        assert_eq!(blink_frame(ms(100)).next_frame, Some(BLINK_PAUSE - ms(100)));
    }

    #[test]
    fn a_cycle_holds_on_fades_out_holds_off_and_fades_back() {
        let at = |offset| blink_frame(BLINK_PAUSE + offset).opacity;
        assert_eq!(at(ms(0)), 1.0);
        assert_eq!(at(BLINK_HOLD - ms(1)), 1.0);
        let halfway_out = at(BLINK_HOLD + BLINK_FADE / 2);
        assert!((halfway_out - 0.5).abs() < 0.01, "{halfway_out}");
        assert_eq!(at(BLINK_HOLD + BLINK_FADE), 0.0);
        assert_eq!(at(BLINK_HOLD + BLINK_FADE + BLINK_HOLD - ms(1)), 0.0);
        let halfway_back = at(BLINK_HOLD * 2 + BLINK_FADE + BLINK_FADE / 2);
        assert!((halfway_back - 0.5).abs() < 0.01, "{halfway_back}");
        assert_eq!(at(cycle()), 1.0);
    }

    #[test]
    fn a_fade_never_jumps_between_one_frame_and_the_next() {
        let mut offset = ms(0);
        let mut last = 1.0_f32;
        // Follow the frames the cursor itself asks for, over two whole cycles.
        while offset < cycle() * 2 {
            let frame = blink_frame(BLINK_PAUSE + offset);
            assert!(
                (frame.opacity - last).abs() < 0.3,
                "{last} to {} at {offset:?}",
                frame.opacity
            );
            last = frame.opacity;
            offset += frame.next_frame.expect("still blinking");
        }
    }

    #[test]
    fn holds_wait_for_their_end_and_only_fades_ask_for_quick_frames() {
        let hold = blink_frame(BLINK_PAUSE + ms(10)).next_frame;
        assert_eq!(hold, Some(BLINK_HOLD - ms(10)));
        let fade = blink_frame(BLINK_PAUSE + BLINK_HOLD + ms(10)).next_frame;
        assert_eq!(fade, Some(ANIMATION_FRAME));
        let end_of_fade = blink_frame(BLINK_PAUSE + BLINK_HOLD + BLINK_FADE - ms(5)).next_frame;
        assert_eq!(end_of_fade, Some(ms(5)));
    }

    #[test]
    fn a_long_idle_settles_solid_and_stops_asking_for_frames() {
        let settled = BLINK_PAUSE + cycle() * BLINK_CYCLES;
        assert_eq!(blink_frame(settled), CursorFrame::SOLID);
        assert_eq!(blink_frame(settled + ms(60_000)), CursorFrame::SOLID);
        // It stops at the end of a cycle, so it never snaps on from off.
        assert!(blink_frame(settled - ms(1)).opacity > 0.99);
    }

    fn seen(focus: CursorFocus, blink: bool, now: Instant) -> CursorObservation {
        CursorObservation { focus, blink, now }
    }

    #[test]
    fn a_cursor_with_no_reason_to_animate_asks_for_no_frames() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let later = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        assert_eq!(
            animator.frame(seen(CursorFocus::Focused, false, later)),
            CursorFrame::SOLID
        );
        assert_eq!(
            animator.frame(seen(CursorFocus::Unfocused, true, later)),
            CursorFrame::SOLID
        );
    }

    #[test]
    fn output_or_typing_restarts_the_pause() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let off = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        assert_eq!(
            animator
                .frame(seen(CursorFocus::Focused, true, off))
                .opacity,
            0.0
        );
        animator.touch(off);
        assert_eq!(
            animator
                .frame(seen(CursorFocus::Focused, true, off + ms(1)))
                .opacity,
            1.0
        );
    }

    #[test]
    fn coming_back_to_the_window_starts_from_solid() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let off = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        animator.frame(seen(CursorFocus::Unfocused, true, off));
        assert_eq!(
            animator
                .frame(seen(CursorFocus::Focused, true, off))
                .opacity,
            1.0
        );
    }
}
