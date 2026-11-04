//! How the chrome moves: pure timing, so it is tested without a window or a clock.
//!
//! Like the cursor's animation, nothing here runs on a schedule of its own. A draw asks
//! what things look like now and when that next changes, so a window with nothing
//! moving stops drawing.

use super::config::MotionPreference;
use std::time::{Duration, Instant};

/// The time between frames of a transition.
const FRAME: Duration = Duration::from_millis(16);
/// How long the sidebar takes to open or close, the longest distance anything travels.
const SIDEBAR_TRANSITION: Duration = Duration::from_millis(150);

/// How far the chrome is allowed to move, once the setting and the platform have had
/// their say.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum MotionPace {
    #[default]
    Full,
    /// Things change at once rather than travelling there.
    Reduced,
}

impl MotionPace {
    /// The pace a setting asks for, where `system` is what the platform says.
    pub(super) fn resolve(preference: MotionPreference, system: Self) -> Self {
        match preference {
            MotionPreference::System => system,
            MotionPreference::Full => Self::Full,
            MotionPreference::Reduced => Self::Reduced,
        }
    }

    /// How long a transition takes at this pace.
    fn duration(self, full: Duration) -> Duration {
        match self {
            Self::Full => full,
            Self::Reduced => Duration::ZERO,
        }
    }

    /// What the platform's accessibility setting says, where one can be read: GNOME's
    /// `enable-animations` and macOS's Reduce Motion. GPUI exposes neither, and each
    /// takes a process to ask, so this blocks and belongs off the UI thread. Anything
    /// that cannot answer is taken as no preference.
    pub(super) fn from_system() -> Self {
        let read = |program: &str, args: &[&str]| {
            std::process::Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        if cfg!(target_os = "macos") {
            read(
                "defaults",
                &["read", "com.apple.universalaccess", "reduceMotion"],
            )
            .map_or(Self::Full, |answer| Self::from_macos_setting(&answer))
        } else if cfg!(target_os = "linux") {
            read(
                "gsettings",
                &["get", "org.gnome.desktop.interface", "enable-animations"],
            )
            .map_or(Self::Full, |answer| Self::from_gnome_setting(&answer))
        } else {
            Self::Full
        }
    }

    fn from_gnome_setting(enable_animations: &str) -> Self {
        match enable_animations.trim() {
            "false" => Self::Reduced,
            _ => Self::Full,
        }
    }

    fn from_macos_setting(reduce_motion: &str) -> Self {
        match reduce_motion.trim() {
            "1" => Self::Reduced,
            _ => Self::Full,
        }
    }
}

/// Eases a value towards a target, decelerating as it arrives.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tween {
    from: f32,
    to: f32,
    started: Instant,
    duration: Duration,
}

impl Tween {
    fn at(value: f32, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            started: now,
            duration: Duration::ZERO,
        }
    }

    fn is_running(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) < self.duration
    }

    fn value(self, now: Instant) -> f32 {
        if !self.is_running(now) {
            return self.to;
        }
        let progress =
            now.saturating_duration_since(self.started).as_secs_f32() / self.duration.as_secs_f32();
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.from + (self.to - self.from) * eased
    }

    /// Sets off for `to` from wherever this is now, so a change of mind never jumps.
    fn head_for(&mut self, to: f32, duration: Duration, now: Instant) {
        if self.to != to {
            *self = Self {
                from: self.value(now),
                to,
                started: now,
                duration,
            };
        }
    }
}

/// How wide the sidebar is in one draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SidebarFrame {
    /// What is painted, which travels.
    pub(super) panel: f32,
    /// What the layout gives it, which the terminal's surface is measured from.
    pub(super) slot: f32,
}

/// The sidebar's width as it opens and closes.
///
/// The slot moves in one step, so the surface changes size once per toggle and the grid
/// with it, instead of the PTY being told a new size on every frame. Opening, it takes
/// its full width at the start and the panel grows into the room. Closing, it keeps that
/// width until the panel has shrunk away. Either way the terminal is never beneath the
/// panel, and a toggle part way through only moves the slot once the panel has settled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SidebarMotion {
    panel: Tween,
    held_slot: f32,
}

impl SidebarMotion {
    fn settled(width: f32, now: Instant) -> Self {
        Self {
            panel: Tween::at(width, now),
            held_slot: width,
        }
    }

    fn head_for(&mut self, width: f32, pace: MotionPace, now: Instant) {
        if self.panel.to == width {
            return;
        }
        let slot = self.frame(now).slot;
        self.panel
            .head_for(width, pace.duration(SIDEBAR_TRANSITION), now);
        self.held_slot = slot.max(width);
    }

    fn frame(self, now: Instant) -> SidebarFrame {
        SidebarFrame {
            panel: self.panel.value(now),
            slot: if self.panel.is_running(now) {
                self.held_slot
            } else {
                self.panel.to
            },
        }
    }
}

/// Everything in the chrome that is on its way somewhere.
#[derive(Clone, Debug, Default)]
pub(super) struct ChromeMotion {
    sidebar: Option<SidebarMotion>,
}

impl ChromeMotion {
    /// The sidebar's frame on its way to `width`. The first draw has nothing to travel
    /// from, so a window opens with its sidebar already in place.
    pub(super) fn sidebar_frame(
        &mut self,
        width: f32,
        pace: MotionPace,
        now: Instant,
    ) -> SidebarFrame {
        let sidebar = self
            .sidebar
            .get_or_insert_with(|| SidebarMotion::settled(width, now));
        sidebar.head_for(width, pace, now);
        sidebar.frame(now)
    }

    /// When the next draw is due, if anything is still moving. Nothing moving asks for
    /// nothing, which leaves an idle window idle.
    pub(super) fn next_frame(&self, now: Instant) -> Option<Duration> {
        self.sidebar
            .is_some_and(|sidebar| sidebar.panel.is_running(now))
            .then_some(FRAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPANDED: f32 = 200.0;
    const COLLAPSED: f32 = 56.0;

    /// Every distinct slot width across a stretch of frames, in order.
    fn slots_over(
        sidebar: &mut SidebarMotion,
        start: Instant,
        span: std::ops::Range<u64>,
    ) -> Vec<f32> {
        let mut slots: Vec<f32> = span
            .map(|ms| sidebar.frame(start + Duration::from_millis(ms)).slot)
            .collect();
        slots.dedup();
        slots
    }

    #[test]
    fn a_tween_eases_to_its_target_and_stops() {
        let now = Instant::now();
        let mut tween = Tween::at(0.0, now);
        tween.head_for(100.0, Duration::from_millis(100), now);
        let half = tween.value(now + Duration::from_millis(50));
        assert!(half > 50.0 && half < 100.0, "decelerates: {half}");
        assert!(tween.is_running(now + Duration::from_millis(99)));
        assert_eq!(tween.value(now + Duration::from_millis(100)), 100.0);
        assert!(!tween.is_running(now + Duration::from_millis(100)));
    }

    #[test]
    fn a_change_of_mind_sets_off_from_where_the_value_had_got_to() {
        let now = Instant::now();
        let mut tween = Tween::at(0.0, now);
        tween.head_for(100.0, Duration::from_millis(100), now);
        let turned = now + Duration::from_millis(50);
        let reached = tween.value(turned);
        tween.head_for(0.0, Duration::from_millis(100), turned);
        assert_eq!(tween.value(turned), reached);
    }

    #[test]
    fn opening_the_sidebar_resizes_the_surface_once_as_it_starts() {
        let now = Instant::now();
        let mut sidebar = SidebarMotion::settled(COLLAPSED, now);
        assert_eq!(slots_over(&mut sidebar, now, 0..10), [COLLAPSED]);
        sidebar.head_for(EXPANDED, MotionPace::Full, now);
        assert_eq!(slots_over(&mut sidebar, now, 0..400), [EXPANDED]);
        assert!(sidebar.frame(now + Duration::from_millis(1)).panel < EXPANDED);
    }

    #[test]
    fn closing_the_sidebar_resizes_the_surface_once_as_it_ends() {
        let now = Instant::now();
        let mut sidebar = SidebarMotion::settled(EXPANDED, now);
        sidebar.head_for(COLLAPSED, MotionPace::Full, now);
        assert_eq!(slots_over(&mut sidebar, now, 0..400), [EXPANDED, COLLAPSED]);
        let almost = now + SIDEBAR_TRANSITION - Duration::from_millis(1);
        assert_eq!(sidebar.frame(almost).slot, EXPANDED);
        assert_eq!(sidebar.frame(now + SIDEBAR_TRANSITION).slot, COLLAPSED);
    }

    #[test]
    fn toggling_back_part_way_never_moves_the_surface_mid_flight() {
        let now = Instant::now();
        let mut sidebar = SidebarMotion::settled(COLLAPSED, now);
        sidebar.head_for(EXPANDED, MotionPace::Full, now);
        let turned = now + Duration::from_millis(30);
        sidebar.head_for(COLLAPSED, MotionPace::Full, turned);
        assert_eq!(
            slots_over(&mut sidebar, turned, 0..400),
            [EXPANDED, COLLAPSED]
        );
    }

    #[test]
    fn the_panel_is_never_wider_than_the_room_it_is_given() {
        let now = Instant::now();
        let mut sidebar = SidebarMotion::settled(COLLAPSED, now);
        sidebar.head_for(EXPANDED, MotionPace::Full, now);
        let turned = now + Duration::from_millis(30);
        sidebar.head_for(COLLAPSED, MotionPace::Full, turned);
        for ms in 30..300 {
            let frame = sidebar.frame(now + Duration::from_millis(ms));
            assert!(frame.panel <= frame.slot, "{frame:?} at {ms}ms");
        }
    }

    #[test]
    fn reduced_motion_lands_the_sidebar_at_once() {
        let now = Instant::now();
        let mut sidebar = SidebarMotion::settled(EXPANDED, now);
        sidebar.head_for(COLLAPSED, MotionPace::Reduced, now);
        assert_eq!(
            sidebar.frame(now),
            SidebarFrame {
                panel: COLLAPSED,
                slot: COLLAPSED
            }
        );
    }

    #[test]
    fn frames_are_asked_for_only_while_something_moves() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        motion.sidebar_frame(EXPANDED, MotionPace::Full, now);
        assert_eq!(
            motion.next_frame(now),
            None,
            "a first draw does not animate"
        );
        motion.sidebar_frame(COLLAPSED, MotionPace::Full, now);
        assert_eq!(motion.next_frame(now), Some(FRAME));
        assert_eq!(motion.next_frame(now + SIDEBAR_TRANSITION), None);
    }

    #[test]
    fn only_a_system_setting_defers_to_the_platform() {
        for system in [MotionPace::Full, MotionPace::Reduced] {
            assert_eq!(
                MotionPace::resolve(MotionPreference::System, system),
                system
            );
            assert_eq!(
                MotionPace::resolve(MotionPreference::Full, system),
                MotionPace::Full
            );
            assert_eq!(
                MotionPace::resolve(MotionPreference::Reduced, system),
                MotionPace::Reduced
            );
        }
    }

    #[test]
    fn the_platform_only_reduces_motion_when_it_clearly_says_so() {
        assert_eq!(
            MotionPace::from_gnome_setting("false\n"),
            MotionPace::Reduced
        );
        assert_eq!(MotionPace::from_gnome_setting("true\n"), MotionPace::Full);
        assert_eq!(MotionPace::from_macos_setting("1\n"), MotionPace::Reduced);
        assert_eq!(MotionPace::from_macos_setting("0\n"), MotionPace::Full);
        assert_eq!(MotionPace::from_gnome_setting(""), MotionPace::Full);
        assert_eq!(MotionPace::from_macos_setting("???"), MotionPace::Full);
    }
}
