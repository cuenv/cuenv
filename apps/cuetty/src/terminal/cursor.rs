//! How the cursor animates: pure timing, so it is tested without a window or a clock.
//!
//! Nothing here runs on a schedule of its own. A draw asks for the current frame, which
//! says when the next one is due, if ever. A cursor that is not animating asks for none,
//! which is what lets an idle window stop drawing entirely.

use super::config::CursorBlink;
use super::model::Known;
use crate::workspace::PaneId;
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

/// How long the optional trail takes to catch up with a cursor that jumped.
const TRAIL_DURATION: Duration = Duration::from_millis(140);
/// How strong it starts, against the cursor's own full strength.
const TRAIL_STRENGTH: f32 = 0.35;

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
    pub trail: Option<TrailSpan>,
}

impl CursorFrame {
    const SOLID: Self = Self {
        opacity: 1.0,
        next_frame: None,
        trail: None,
    };
}

/// A blinking cursor, this long after the last output or key press.
fn blink_frame(since_activity: Duration) -> CursorFrame {
    let Some(blinking_for) = since_activity.checked_sub(BLINK_PAUSE) else {
        return CursorFrame {
            opacity: 1.0,
            next_frame: Some(BLINK_PAUSE.saturating_sub(since_activity)),
            trail: None,
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
            trail: None,
        },
        Some(into_fade) => {
            let progress = into_fade.as_secs_f32() / BLINK_FADE.as_secs_f32();
            let eased = (1.0 + (std::f32::consts::PI * progress).cos()) / 2.0;
            CursorFrame {
                opacity: if visible_at_start { eased } else { 1.0 - eased },
                next_frame: Some(ANIMATION_FRAME.min(BLINK_FADE - into_fade)),
                trail: None,
            }
        }
    }
}

/// A stretch of a row the trail covers, in fractional columns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TrailSpan {
    pub row: u16,
    pub start: f32,
    pub end: f32,
    pub opacity: f32,
}

/// A move the trail is following, as (row, column) cells.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CursorJump {
    from: (u16, u16),
    to: (u16, u16),
    started: Instant,
}

impl CursorJump {
    /// Along a row, the trail's tail closes on the cursor. Between rows there is no
    /// straight stretch to sweep, so it fades where the cursor was.
    fn span(self, now: Instant) -> Option<TrailSpan> {
        let elapsed = now.saturating_duration_since(self.started);
        let progress = elapsed.as_secs_f32() / TRAIL_DURATION.as_secs_f32();
        if progress >= 1.0 {
            return None;
        }
        let (from_row, from_column) = self.from;
        let (to_row, to_column) = self.to;
        let (from_column, to_column) = (f32::from(from_column), f32::from(to_column));
        let (start, end) = if from_row == to_row {
            let tail = from_column + (to_column - from_column) * progress;
            (tail.min(to_column), tail.max(to_column) + 1.0)
        } else {
            (from_column, from_column + 1.0)
        };
        Some(TrailSpan {
            row: from_row,
            start,
            end,
            opacity: TRAIL_STRENGTH * (1.0 - progress),
        })
    }
}

/// Moving to the next cell is what typing looks like, and a trail on that would only
/// smear the text being typed.
fn is_a_jump(from: (u16, u16), to: (u16, u16)) -> bool {
    from.0 != to.0 || from.1.abs_diff(to.1) > 1
}

/// What a draw knows about the cursor it is about to paint.
#[derive(Debug, Clone, Copy)]
pub(super) struct CursorObservation {
    pub pane: PaneId,
    /// (row, column), or `None` while it is not shown.
    pub position: Option<(u16, u16)>,
    pub focus: CursorFocus,
    pub blink: bool,
    pub trail: bool,
    pub now: Instant,
}

/// Remembers what the cursor's animation is timed from.
#[derive(Debug, Clone)]
pub(super) struct CursorAnimator {
    last_activity: Instant,
    focus: CursorFocus,
    /// Which pane's cursor was last seen and where, so switching tabs is not a jump.
    seen_at: Option<(PaneId, (u16, u16))>,
    jump: Option<CursorJump>,
}

impl CursorAnimator {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            focus: CursorFocus::Focused,
            seen_at: None,
            jump: None,
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
        let focused = seen.focus == CursorFocus::Focused;
        // A hidden cursor has nothing to fade, whatever it has been asked to do.
        let frame = if focused && seen.blink && seen.position.is_some() {
            blink_frame(seen.now.saturating_duration_since(self.last_activity))
        } else {
            CursorFrame::SOLID
        };
        let trail = self.follow(seen).filter(|_| focused && seen.trail);
        CursorFrame {
            next_frame: match (frame.next_frame, trail) {
                (Some(next), Some(_)) => Some(next.min(ANIMATION_FRAME)),
                (None, Some(_)) => Some(ANIMATION_FRAME),
                (next, None) => next,
            },
            trail,
            ..frame
        }
    }

    /// Notes where the cursor is now, and returns what trail it is leaving.
    fn follow(&mut self, seen: CursorObservation) -> Option<TrailSpan> {
        let here = seen.position.map(|position| (seen.pane, position));
        if let Some(((was_pane, from), (pane, to))) = self.seen_at.zip(here)
            && from != to
        {
            self.jump =
                (seen.trail && was_pane == pane && is_a_jump(from, to)).then_some(CursorJump {
                    from,
                    to,
                    started: seen.now,
                });
        }
        if self.seen_at.map(|(pane, _)| pane) != here.map(|(pane, _)| pane) {
            self.jump = None;
        }
        self.seen_at = here;
        self.jump = self.jump.filter(|jump| jump.span(seen.now).is_some());
        self.jump.and_then(|jump| jump.span(seen.now))
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

    /// A focused cursor sitting still, not asked to blink or leave a trail.
    fn resting(now: Instant) -> CursorObservation {
        CursorObservation {
            pane: PaneId::new(1),
            position: Some((0, 0)),
            focus: CursorFocus::Focused,
            blink: false,
            trail: false,
            now,
        }
    }

    fn blinking(now: Instant) -> CursorObservation {
        CursorObservation {
            blink: true,
            ..resting(now)
        }
    }

    #[test]
    fn a_cursor_with_no_reason_to_animate_asks_for_no_frames() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let later = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        assert_eq!(animator.frame(resting(later)), CursorFrame::SOLID);
        assert_eq!(
            animator.frame(CursorObservation {
                focus: CursorFocus::Unfocused,
                ..blinking(later)
            }),
            CursorFrame::SOLID
        );
    }

    #[test]
    fn a_hidden_cursor_asks_for_no_frames_however_it_was_told_to_blink() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let hidden = |now| CursorObservation {
            position: None,
            ..blinking(now)
        };
        // Through the pause, a hold, and a fade.
        for after in [
            ms(0),
            BLINK_PAUSE + ms(10),
            BLINK_PAUSE + BLINK_HOLD + ms(10),
        ] {
            assert_eq!(animator.frame(hidden(start + after)), CursorFrame::SOLID);
        }
    }

    #[test]
    fn output_or_typing_restarts_the_pause() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let off = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        assert_eq!(animator.frame(blinking(off)).opacity, 0.0);
        animator.touch(off);
        assert_eq!(animator.frame(blinking(off + ms(1))).opacity, 1.0);
    }

    #[test]
    fn coming_back_to_the_window_starts_from_solid() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let off = start + BLINK_PAUSE + BLINK_HOLD + BLINK_FADE;
        animator.frame(CursorObservation {
            focus: CursorFocus::Unfocused,
            ..blinking(off)
        });
        assert_eq!(animator.frame(blinking(off)).opacity, 1.0);
    }

    fn trailing(position: (u16, u16), now: Instant) -> CursorObservation {
        CursorObservation {
            position: Some(position),
            trail: true,
            ..resting(now)
        }
    }

    #[test]
    fn a_jump_along_a_row_leaves_a_trail_that_closes_and_fades() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((2, 3), start));
        let first = animator.frame(trailing((2, 10), start));
        let first_span = first.trail.expect("a jump leaves a trail");
        assert_eq!((first_span.start, first_span.end), (3.0, 11.0));
        assert_eq!(first_span.row, 2);
        assert_eq!(first.next_frame, Some(ANIMATION_FRAME));

        let later = animator
            .frame(trailing((2, 10), start + TRAIL_DURATION / 2))
            .trail
            .expect("still catching up");
        assert!(later.start > first_span.start, "the tail closes in");
        assert_eq!(later.end, 11.0);
        assert!(later.opacity < first_span.opacity);
    }

    #[test]
    fn a_leftward_jump_trails_on_the_right() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((0, 10), start));
        let span = animator
            .frame(trailing((0, 2), start))
            .trail
            .expect("a jump leaves a trail");
        assert_eq!((span.start, span.end), (2.0, 11.0));
    }

    #[test]
    fn a_jump_between_rows_fades_where_the_cursor_was() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((1, 4), start));
        let span = animator
            .frame(trailing((5, 8), start))
            .trail
            .expect("a jump leaves a trail");
        assert_eq!((span.row, span.start, span.end), (1, 4.0, 5.0));
    }

    #[test]
    fn a_finished_trail_stops_asking_for_frames() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((0, 0), start));
        animator.frame(trailing((0, 9), start));
        assert_eq!(
            animator.frame(trailing((0, 9), start + TRAIL_DURATION)),
            CursorFrame::SOLID
        );
    }

    #[test]
    fn typing_leaves_no_trail() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((0, 4), start));
        assert_eq!(animator.frame(trailing((0, 5), start)).trail, None);
    }

    #[test]
    fn a_trail_only_appears_when_asked_for() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(resting(start));
        let moved = CursorObservation {
            position: Some((0, 9)),
            ..resting(start)
        };
        assert_eq!(animator.frame(moved), CursorFrame::SOLID);
    }

    #[test]
    fn switching_tabs_is_not_a_jump() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((0, 0), start));
        let other_pane = CursorObservation {
            pane: PaneId::new(2),
            ..trailing((9, 9), start)
        };
        assert_eq!(animator.frame(other_pane).trail, None);
    }

    #[test]
    fn a_cursor_that_loses_focus_loses_its_trail() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        animator.frame(trailing((0, 0), start));
        let unfocused = CursorObservation {
            focus: CursorFocus::Unfocused,
            ..trailing((0, 9), start)
        };
        assert_eq!(animator.frame(unfocused), CursorFrame::SOLID);
    }

    #[test]
    fn a_blink_and_a_trail_ask_for_the_sooner_frame() {
        let start = Instant::now();
        let mut animator = CursorAnimator::new(start);
        let observe = |position| CursorObservation {
            blink: true,
            ..trailing(position, start)
        };
        animator.frame(observe((0, 0)));
        assert_eq!(
            animator.frame(observe((0, 9))).next_frame,
            Some(ANIMATION_FRAME)
        );
    }
}
