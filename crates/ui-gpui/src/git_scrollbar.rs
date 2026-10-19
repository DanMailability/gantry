//! The diff page's scrollbars: a vertical and a horizontal one over the list, drawn like a pane's (accent while the
//! view moves, muted when it rests). `workspace_view` wires the presses and drags; this holds the geometry and the look.

use std::cell::Cell;
use std::time::Instant;

use app_core::scroll::{Thumb, activity, offset_for_extent, thumb_for_extent};
use app_core::theme_colors::{deepen, mix};
use gpui::{Bounds, Pixels, Rgba, UniformListScrollHandle, div, prelude::*, px, rgb};

use crate::theme::Theme;

/// Thickness of a scrollbar track, before the interface scale.
pub(crate) const WIDTH: f32 = 10.0;
/// The shortest a thumb gets, before the interface scale.
const MIN_THUMB: f32 = 24.0;

/// The two directions a list scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bar {
    Vertical,
    Horizontal,
}

/// How far a list is scrolled: all in pixels, `x` and `y` as (horizontal, vertical).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Extent {
    /// The visible size.
    pub viewport: (f32, f32),
    /// The size of everything.
    pub contents: (f32, f32),
    /// How far the start is scrolled out of view.
    pub offset: (f32, f32),
    /// Where the list is drawn.
    pub bounds: Bounds<Pixels>,
}

impl Extent {
    /// The extent of a list, once it has been laid out.
    pub(crate) fn of(handle: &UniformListScrollHandle) -> Option<Extent> {
        let state = handle.0.borrow();
        let size = state.last_item_size?;
        let offset = state.base_handle.offset();
        Some(Extent {
            viewport: (f32::from(size.item.width), f32::from(size.item.height)),
            contents: (f32::from(size.contents.width), f32::from(size.contents.height)),
            offset: (-f32::from(offset.x), -f32::from(offset.y)),
            bounds: state.base_handle.bounds(),
        })
    }

    /// The thumbs, vertical then horizontal; each track gives up a corner to the other bar when both show.
    pub(crate) fn thumbs(&self, ui_scale: f32) -> (Option<Thumb>, Option<Thumb>) {
        let min = MIN_THUMB * ui_scale;
        let corner = WIDTH * ui_scale;
        let (vx, vy) = (self.contents.0 > self.viewport.0 + 0.5, self.contents.1 > self.viewport.1 + 0.5);
        let v_track = self.viewport.1 - if vx { corner } else { 0.0 };
        let h_track = self.viewport.0 - if vy { corner } else { 0.0 };
        (
            thumb_for_extent(v_track, self.viewport.1, self.contents.1, self.offset.1, min),
            thumb_for_extent(h_track, self.viewport.0, self.contents.0, self.offset.0, min),
        )
    }

    /// The offset along `bar` for a thumb of length `len` grabbed so its start is at `top` on the track.
    pub(crate) fn offset_for(&self, bar: Bar, top: f32, len: f32, ui_scale: f32) -> f32 {
        let corner = WIDTH * ui_scale;
        let (vx, vy) = (self.contents.0 > self.viewport.0 + 0.5, self.contents.1 > self.viewport.1 + 0.5);
        match bar {
            Bar::Vertical => {
                let track = self.viewport.1 - if vx { corner } else { 0.0 };
                offset_for_extent(top, track, len, self.viewport.1, self.contents.1)
            }
            Bar::Horizontal => {
                let track = self.viewport.0 - if vy { corner } else { 0.0 };
                offset_for_extent(top, track, len, self.viewport.0, self.contents.0)
            }
        }
    }
}

/// What the page remembers about its scrollbars between frames.
pub(crate) struct Bars {
    /// When the view last moved: the thumbs are in their active color for a while after.
    moved: Cell<Option<Instant>>,
    /// The offset seen at the last frame, to notice a movement.
    seen: Cell<(f32, f32)>,
    /// While a thumb is dragged: which one, and how far below its start it was grabbed.
    pub(crate) grab: Option<(Bar, f32)>,
}

impl Bars {
    pub(crate) fn new() -> Self {
        Bars { moved: Cell::new(None), seen: Cell::new((0.0, 0.0)), grab: None }
    }

    /// Note a frame's offset; a change counts as movement.
    pub(crate) fn see(&self, offset: (f32, f32)) {
        if self.seen.replace(offset) != offset {
            self.moved.set(Some(Instant::now()));
        }
    }

    /// The thumbs' color level: 1 = active (accent), 0 = resting (muted).
    pub(crate) fn level(&self) -> f32 {
        if self.grab.is_some() {
            return 1.0;
        }
        self.moved.get().map_or(0.0, |at| activity(at.elapsed().as_millis() as u64))
    }
}

/// A thumb's color at `level`: the accent, deepened, fading to grey as the view rests.
pub(crate) fn thumb_color(theme: &Theme, level: f32) -> Rgba {
    let mut color = rgb(mix(deepen(theme.tokens.accent, 0.62), theme.tokens.muted, level));
    color.a = 0.45 + 0.5 * level;
    color
}

/// One thumb, drawn inside its track.
pub(crate) fn thumb_div(bar: Bar, thumb: Thumb, color: Rgba, theme: &Theme) -> gpui::Div {
    let thickness = theme.ui(WIDTH - 4.);
    let base = div().absolute().rounded(theme.ui((WIDTH - 4.) / 2.)).bg(color);
    match bar {
        Bar::Vertical => base.top(px(thumb.top)).left(theme.ui(2.)).w(thickness).h(px(thumb.len)),
        Bar::Horizontal => base.left(px(thumb.top)).top(theme.ui(2.)).h(thickness).w(px(thumb.len)),
    }
}
