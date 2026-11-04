//! Where the overlay scrollbar's thumb goes for a viewport and where dragging it leads:
//! pure geometry, so it is tested without a window.

use super::model::TerminalFrame;

/// How far back a terminal can scroll and where its viewport is now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScrollExtent {
    /// Rows of history above the screen.
    history: usize,
    /// Rows on screen.
    screen: usize,
    /// How far the viewport is from the live bottom, in rows.
    offset: usize,
}

/// The thumb along its track, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Thumb {
    pub(super) top: f32,
    pub(super) height: f32,
}

/// A track to place a thumb on, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Track {
    pub(super) height: f32,
    /// The shortest a thumb may be and still be there to grab.
    pub(super) min_thumb: f32,
}

impl Track {
    /// A thumb cannot be longer than the track that holds it.
    fn thumb_height(self, extent: ScrollExtent) -> f32 {
        let total = extent.history + extent.screen;
        let share = extent.screen as f32 / total.max(1) as f32;
        (self.height * share).max(self.min_thumb).min(self.height)
    }

    fn travel(self, extent: ScrollExtent) -> f32 {
        self.height - self.thumb_height(extent)
    }
}

impl ScrollExtent {
    /// `None` where there is nothing to scroll, as on an alternate screen, so no scrollbar
    /// shows there at all.
    pub(super) fn of_frame(frame: &TerminalFrame) -> Option<Self> {
        let history = frame.scrollback_rows.filter(|rows| *rows > 0)?;
        Some(Self {
            history,
            screen: usize::from(frame.dimensions.rows),
            offset: frame.viewport_offset.unwrap_or(0).min(history),
        })
    }

    /// Where the thumb sits: at the bottom of the track on the live screen and at the top
    /// on the oldest history.
    pub(super) fn thumb(self, track: Track) -> Thumb {
        let scrolled = self.offset as f32 / self.history.max(1) as f32;
        Thumb {
            top: track.travel(self) * (1.0 - scrolled),
            height: track.thumb_height(self),
        }
    }

    /// The viewport offset that puts the thumb's top at `top`.
    pub(super) fn offset_at(self, thumb_top: f32, track: Track) -> usize {
        let travel = track.travel(self);
        if travel <= 0.0 {
            return self.offset;
        }
        let along = (thumb_top / travel).clamp(0.0, 1.0);
        ((1.0 - along) * self.history as f32).round() as usize
    }

    /// Where along the thumb a press at `pointer` takes hold. A press on the thumb holds on
    /// where it landed, and one beside it brings the thumb's middle there.
    pub(super) fn grab_at(self, pointer: f32, track: Track) -> f32 {
        let thumb = self.thumb(track);
        let on_thumb = (thumb.top..=thumb.top + thumb.height).contains(&pointer);
        if on_thumb {
            pointer - thumb.top
        } else {
            thumb.height / 2.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::{CursorState, Known, SamplingToken, TerminalDimensions};

    const TRACK: Track = Track {
        height: 400.0,
        min_thumb: 24.0,
    };

    fn extent(history: usize, offset: usize) -> ScrollExtent {
        ScrollExtent {
            history,
            screen: 40,
            offset,
        }
    }

    #[test]
    fn a_frame_with_no_history_has_no_scrollbar() {
        let frame = |scrollback_rows, viewport_offset| TerminalFrame {
            dimensions: TerminalDimensions::new(40, 80),
            rows: Vec::new(),
            cursor: CursorState {
                position: None,
                shape: Known::Unknown,
                visible: Known::Unknown,
                blinking: Known::Unknown,
            },
            viewport_offset,
            scrollback_rows,
            sampling_token: SamplingToken(0),
            stable_revision: None,
        };
        assert_eq!(ScrollExtent::of_frame(&frame(None, None)), None);
        assert_eq!(ScrollExtent::of_frame(&frame(Some(0), Some(0))), None);
        assert_eq!(
            ScrollExtent::of_frame(&frame(Some(100), Some(7))),
            Some(extent(100, 7))
        );
    }

    #[test]
    fn the_live_screen_is_at_the_bottom_and_the_oldest_history_at_the_top() {
        let bottom = extent(360, 0).thumb(TRACK);
        assert_eq!(bottom.height, 40.0, "a screen's share of all the rows");
        assert_eq!(bottom.top + bottom.height, TRACK.height);
        assert_eq!(extent(360, 360).thumb(TRACK).top, 0.0);
    }

    #[test]
    fn a_long_history_never_shrinks_the_thumb_past_grabbing() {
        let thumb = extent(1_000_000, 0).thumb(TRACK);
        assert_eq!(thumb.height, TRACK.min_thumb);
        assert_eq!(thumb.top + thumb.height, TRACK.height);
    }

    #[test]
    fn a_track_shorter_than_the_least_thumb_holds_what_fits() {
        let short = Track {
            height: 10.0,
            min_thumb: 24.0,
        };
        let thumb = extent(100, 50).thumb(short);
        assert_eq!(thumb.height, 10.0);
        assert_eq!(thumb.top, 0.0);
        assert_eq!(extent(100, 50).offset_at(0.0, short), 50, "nowhere to drag");
    }

    #[test]
    fn dragging_the_thumb_to_where_it_is_leaves_the_viewport_alone() {
        for offset in [0, 1, 17, 180, 359, 360] {
            let at = extent(360, offset);
            assert_eq!(at.offset_at(at.thumb(TRACK).top, TRACK), offset);
        }
    }

    #[test]
    fn dragging_past_either_end_stops_there() {
        let at = extent(360, 100);
        assert_eq!(at.offset_at(-500.0, TRACK), 360);
        assert_eq!(at.offset_at(5_000.0, TRACK), 0);
    }

    #[test]
    fn a_press_on_the_thumb_holds_on_where_it_landed() {
        let at = extent(360, 0);
        let thumb = at.thumb(TRACK);
        assert_eq!(at.grab_at(thumb.top + 3.0, TRACK), 3.0);
    }

    #[test]
    fn a_press_beside_the_thumb_brings_its_middle_to_the_pointer() {
        let at = extent(360, 0);
        assert_eq!(at.grab_at(10.0, TRACK), at.thumb(TRACK).height / 2.0);
    }
}
