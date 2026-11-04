//! How the chrome moves: pure timing, so it is tested without a window or a clock.
//!
//! Like the cursor's animation, nothing here runs on a schedule of its own. A draw asks
//! what things look like now and when that next changes, so a window with nothing
//! moving stops drawing.

use super::config::MotionPreference;
use crate::workspace::TabId;
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// The time between frames of a transition.
const FRAME: Duration = Duration::from_millis(16);
/// How long the sidebar takes to open or close, the longest distance anything travels.
const SIDEBAR_TRANSITION: Duration = Duration::from_millis(150);
/// How long the active tab's marker takes to reach another tab, and its fill to follow.
const TAB_TRANSITION: Duration = Duration::from_millis(140);
/// How long a floating panel takes to fade in.
pub(super) const PANEL_FADE_IN: Duration = Duration::from_millis(120);
/// How long the scrollbar stays after the view last moved before it starts to fade.
const SCROLLBAR_HOLD: Duration = Duration::from_millis(800);
/// How long it then takes to fade away.
const SCROLLBAR_FADE: Duration = Duration::from_millis(150);
/// How long a hover takes to show or clear: quick, since it answers the pointer.
const HOVER_TRANSITION: Duration = Duration::from_millis(100);

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
    pub(super) fn duration(self, full: Duration) -> Duration {
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

/// Things that are each somewhere between off (0) and on (1). Whatever has settled off
/// is forgotten, so this only ever holds what is lit or fading.
#[derive(Clone, Debug)]
struct Fades<K> {
    levels: HashMap<K, Tween>,
}

impl<K> Default for Fades<K> {
    fn default() -> Self {
        Self {
            levels: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> Fades<K> {
    fn set(&mut self, key: K, change: FadeChange) {
        let FadeChange {
            target,
            duration,
            now,
        } = change;
        let to = match target {
            FadeTarget::On => 1.0,
            FadeTarget::Off => 0.0,
        };
        self.levels
            .entry(key)
            .or_insert_with(|| Tween::at(0.0, now))
            .head_for(to, duration, now);
    }

    fn level(&self, key: &K, now: Instant) -> f32 {
        self.levels.get(key).map_or(0.0, |tween| tween.value(now))
    }

    fn is_running(&self, now: Instant) -> bool {
        self.levels.values().any(|tween| tween.is_running(now))
    }

    /// Drops what has faded out and what `live` says no longer exists.
    fn retain(&mut self, now: Instant, live: impl Fn(&K) -> bool) {
        self.levels
            .retain(|key, tween| live(key) && (tween.to > 0.0 || tween.is_running(now)));
    }
}

/// Where a fade is heading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FadeTarget {
    On,
    Off,
}

/// A fade setting off.
#[derive(Clone, Copy, Debug)]
struct FadeChange {
    target: FadeTarget,
    duration: Duration,
    now: Instant,
}

/// A button in the sidebar's footer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SidebarButton {
    Notice,
    NewTab,
    Tasks,
    Toggle,
    PaletteHint,
}

/// A button on the search bar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SearchButton {
    Previous,
    Next,
    Close,
}

/// Something in the chrome that lights up under the pointer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum HoverTarget {
    Tab(TabId),
    Button(SidebarButton),
    PaletteRow(usize),
    TaskRow(usize),
    /// A way out of a tab with no terminal to show, by its place among them.
    SessionAction(usize),
    SearchButton(SearchButton),
    Scrollbar,
}

/// The tabs as this draw has them.
#[derive(Clone, Copy, Debug)]
pub(super) struct TabPlaces<'a> {
    pub(super) tabs: &'a [TabId],
    pub(super) active: Option<TabId>,
}

/// Everything in the chrome that is on its way somewhere.
#[derive(Clone, Debug, Default)]
pub(super) struct ChromeMotion {
    sidebar: Option<SidebarMotion>,
    /// Where the active tab's marker is, as a position in the tab list.
    marker: Option<Tween>,
    hover: Fades<HoverTarget>,
    /// How strongly each tab shows as the active one.
    active: Fades<TabId>,
    /// When the terminal's view last moved, which the scrollbar shows for a while.
    scrolled: Option<Instant>,
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

    /// Sends the marker to the active tab and hands the fill over to it, whichever way it
    /// became active and wherever a reorder or a closed tab has left it. The first tab to
    /// be active has no tab to slide from.
    pub(super) fn follow_tabs(&mut self, places: TabPlaces<'_>, pace: MotionPace, now: Instant) {
        let TabPlaces { tabs, active } = places;
        let duration = pace.duration(TAB_TRANSITION);
        let place = active.and_then(|active| tabs.iter().position(|tab| *tab == active));
        self.marker = place.map(|place| {
            let place = place as f32;
            let mut marker = self.marker.unwrap_or_else(|| Tween::at(place, now));
            marker.head_for(place, duration, now);
            marker
        });
        for tab in tabs {
            let target = if active == Some(*tab) {
                FadeTarget::On
            } else {
                FadeTarget::Off
            };
            self.active.set(
                *tab,
                FadeChange {
                    target,
                    duration,
                    now,
                },
            );
        }
        self.active.retain(now, |tab| tabs.contains(tab));
    }

    /// Lets go of whatever has left the screen: the element under the pointer reports no
    /// end to its hover once it has gone, so it would come back lit.
    pub(super) fn forget_hovers(&mut self, now: Instant, live: impl Fn(&HoverTarget) -> bool) {
        self.hover.retain(now, live);
    }

    /// The pointer has arrived over `target`.
    pub(super) fn hover_on(&mut self, target: HoverTarget, pace: MotionPace, now: Instant) {
        self.hover.set(
            target,
            FadeChange {
                target: FadeTarget::On,
                duration: pace.duration(HOVER_TRANSITION),
                now,
            },
        );
    }

    /// The pointer has left `target`.
    pub(super) fn hover_off(&mut self, target: HoverTarget, pace: MotionPace, now: Instant) {
        self.hover.set(
            target,
            FadeChange {
                target: FadeTarget::Off,
                duration: pace.duration(HOVER_TRANSITION),
                now,
            },
        );
    }

    /// How lit something is under the pointer.
    pub(super) fn hover_level(&self, target: HoverTarget, now: Instant) -> f32 {
        self.hover.level(&target, now)
    }

    /// How strongly a tab shows as the active one.
    pub(super) fn active_level(&self, tab: TabId, now: Instant) -> f32 {
        self.active.level(&tab, now)
    }

    /// The terminal's view has just moved, so the scrollbar shows.
    pub(super) fn view_scrolled(&mut self, now: Instant) {
        self.scrolled = Some(now);
    }

    /// How visible the view's last move leaves the scrollbar: held there a while and then
    /// fading, or at a reduced pace simply gone once the hold is over.
    pub(super) fn scrollbar_level(&self, now: Instant, pace: MotionPace) -> f32 {
        let Some(scrolled) = self.scrolled else {
            return 0.0;
        };
        let since = now.saturating_duration_since(scrolled);
        let Some(fading_for) = since.checked_sub(SCROLLBAR_HOLD) else {
            return 1.0;
        };
        let fade = pace.duration(SCROLLBAR_FADE);
        if fading_for >= fade {
            return 0.0;
        }
        1.0 - fading_for.as_secs_f32() / fade.as_secs_f32()
    }

    /// When the scrollbar next looks different because of the view's last move. A hold
    /// needs nothing until it ends, and only the fade needs every frame.
    fn scrollbar_next_frame(&self, now: Instant, pace: MotionPace) -> Option<Duration> {
        let since = now.saturating_duration_since(self.scrolled?);
        match since.checked_sub(SCROLLBAR_HOLD) {
            None => Some(SCROLLBAR_HOLD - since),
            Some(fading_for) => (fading_for < pace.duration(SCROLLBAR_FADE)).then_some(FRAME),
        }
    }

    /// Where the active tab's marker is, as a fractional place in the tab list.
    pub(super) fn marker_place(&self, now: Instant) -> Option<f32> {
        self.marker.map(|marker| marker.value(now))
    }

    /// When the next draw is due, if anything is still moving. Nothing moving asks for
    /// nothing, which leaves an idle window idle.
    pub(super) fn next_frame(&self, now: Instant, pace: MotionPace) -> Option<Duration> {
        let moving = self
            .sidebar
            .is_some_and(|sidebar| sidebar.panel.is_running(now))
            || self.marker.is_some_and(|marker| marker.is_running(now))
            || self.hover.is_running(now)
            || self.active.is_running(now);
        moving
            .then_some(FRAME)
            .into_iter()
            .chain(self.scrollbar_next_frame(now, pace))
            .min()
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
    fn the_marker_slides_between_tabs_and_lands_on_the_active_one() {
        let now = Instant::now();
        let tabs = [TabId::new(1), TabId::new(2), TabId::new(3)];
        let mut motion = ChromeMotion::default();
        let at = |active| TabPlaces {
            tabs: &tabs,
            active: Some(active),
        };
        motion.follow_tabs(at(tabs[0]), MotionPace::Full, now);
        assert_eq!(motion.marker_place(now), Some(0.0), "nothing to slide from");
        motion.follow_tabs(at(tabs[2]), MotionPace::Full, now);
        assert_eq!(motion.marker_place(now), Some(0.0));
        let part_way = motion
            .marker_place(now + TAB_TRANSITION / 2)
            .unwrap_or_default();
        assert!(part_way > 0.0 && part_way < 2.0, "{part_way}");
        assert_eq!(motion.marker_place(now + TAB_TRANSITION), Some(2.0));
    }

    #[test]
    fn the_marker_follows_its_tab_when_the_list_moves_under_it() {
        let now = Instant::now();
        let (first, second) = (TabId::new(1), TabId::new(2));
        let mut motion = ChromeMotion::default();
        let reduced = MotionPace::Reduced;
        let places = [first, second];
        motion.follow_tabs(
            TabPlaces {
                tabs: &places,
                active: Some(second),
            },
            reduced,
            now,
        );
        assert_eq!(motion.marker_place(now), Some(1.0));
        motion.follow_tabs(
            TabPlaces {
                tabs: &[second],
                active: Some(second),
            },
            reduced,
            now,
        );
        assert_eq!(motion.marker_place(now), Some(0.0));
        motion.follow_tabs(
            TabPlaces {
                tabs: &[],
                active: None,
            },
            reduced,
            now,
        );
        assert_eq!(motion.marker_place(now), None);
    }

    #[test]
    fn the_active_fill_hands_over_from_one_tab_to_the_next() {
        let now = Instant::now();
        let (first, second) = (TabId::new(1), TabId::new(2));
        let tabs = [first, second];
        let mut motion = ChromeMotion::default();
        let at = |active| TabPlaces {
            tabs: &tabs,
            active: Some(active),
        };
        motion.follow_tabs(at(first), MotionPace::Reduced, now);
        motion.follow_tabs(at(second), MotionPace::Full, now);
        let midway = now + TAB_TRANSITION / 2;
        let (leaving, arriving) = (
            motion.active_level(first, midway),
            motion.active_level(second, midway),
        );
        assert!(leaving > 0.0 && leaving < 1.0);
        assert!(arriving > 0.0 && arriving < 1.0);
        let done = now + TAB_TRANSITION;
        assert_eq!(motion.active_level(first, done), 0.0);
        assert_eq!(motion.active_level(second, done), 1.0);
    }

    #[test]
    fn a_hover_fades_up_and_away_and_leaves_nothing_behind() {
        let now = Instant::now();
        let target = HoverTarget::PaletteRow(1);
        let mut motion = ChromeMotion::default();
        motion.hover_on(target, MotionPace::Full, now);
        assert!(motion.next_frame(now, MotionPace::Full).is_some());
        let lit = now + HOVER_TRANSITION;
        assert_eq!(motion.hover_level(target, lit), 1.0);
        motion.hover_off(target, MotionPace::Full, lit);
        motion.forget_hovers(lit, |_| true);
        assert_eq!(motion.hover_level(target, lit), 1.0, "still fading");
        let gone = lit + HOVER_TRANSITION;
        assert_eq!(motion.hover_level(target, gone), 0.0);
        motion.forget_hovers(gone, |_| true);
        assert!(motion.hover.levels.is_empty());
    }

    #[test]
    fn a_hovered_control_that_left_the_screen_is_not_lit_when_it_returns() {
        let now = Instant::now();
        let target = HoverTarget::TaskRow(0);
        let mut motion = ChromeMotion::default();
        motion.hover_on(target, MotionPace::Reduced, now);
        assert_eq!(motion.hover_level(target, now), 1.0);
        motion.forget_hovers(now, |target| !matches!(target, HoverTarget::TaskRow(_)));
        assert_eq!(motion.hover_level(target, now), 0.0);
    }

    #[test]
    fn the_scrollbar_holds_after_the_view_moves_and_then_fades_away() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        assert_eq!(motion.scrollbar_level(now, MotionPace::Full), 0.0);
        motion.view_scrolled(now);
        let level = |at| motion.scrollbar_level(at, MotionPace::Full);
        assert_eq!(level(now), 1.0);
        assert_eq!(level(now + SCROLLBAR_HOLD - Duration::from_millis(1)), 1.0);
        let fading = level(now + SCROLLBAR_HOLD + SCROLLBAR_FADE / 2);
        assert!(fading > 0.0 && fading < 1.0, "{fading}");
        assert_eq!(level(now + SCROLLBAR_HOLD + SCROLLBAR_FADE), 0.0);
    }

    #[test]
    fn a_reduced_pace_drops_the_scrollbar_without_a_fade() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        motion.view_scrolled(now);
        let level = |at| motion.scrollbar_level(at, MotionPace::Reduced);
        assert_eq!(level(now + SCROLLBAR_HOLD - Duration::from_millis(1)), 1.0);
        assert_eq!(level(now + SCROLLBAR_HOLD), 0.0);
    }

    #[test]
    fn a_held_scrollbar_only_asks_for_the_draw_that_ends_the_hold() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        motion.view_scrolled(now);
        assert_eq!(
            motion.next_frame(now, MotionPace::Full),
            Some(SCROLLBAR_HOLD)
        );
        assert_eq!(
            motion.next_frame(
                now + SCROLLBAR_HOLD + Duration::from_millis(1),
                MotionPace::Full
            ),
            Some(FRAME)
        );
        assert_eq!(
            motion.next_frame(now + SCROLLBAR_HOLD + SCROLLBAR_FADE, MotionPace::Full),
            None
        );
    }

    #[test]
    fn a_reduced_pace_asks_for_nothing_once_the_scrollbars_hold_has_ended() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        motion.view_scrolled(now);
        assert_eq!(
            motion.next_frame(now, MotionPace::Reduced),
            Some(SCROLLBAR_HOLD)
        );
        assert_eq!(
            motion.next_frame(now + SCROLLBAR_HOLD, MotionPace::Reduced),
            None
        );
    }

    #[test]
    fn frames_are_asked_for_only_while_something_moves() {
        let now = Instant::now();
        let mut motion = ChromeMotion::default();
        motion.sidebar_frame(EXPANDED, MotionPace::Full, now);
        assert_eq!(
            motion.next_frame(now, MotionPace::Full),
            None,
            "a first draw does not animate"
        );
        motion.sidebar_frame(COLLAPSED, MotionPace::Full, now);
        assert_eq!(motion.next_frame(now, MotionPace::Full), Some(FRAME));
        assert_eq!(
            motion.next_frame(now + SIDEBAR_TRANSITION, MotionPace::Full),
            None
        );
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
