//! Decide what a mouse-wheel movement does in a terminal pane.

use crate::term_state::ScrollModes;

/// Result of a wheel movement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrollAction {
    /// Scroll the local viewport by this many lines (positive = towards history).
    Viewport(i32),
    /// Send these bytes to the program instead.
    Send(Vec<u8>),
}

/// Map `lines` of wheel movement (positive = wheel up) at viewport cell `(col, row)` to an action.
pub fn scroll_action(modes: ScrollModes, lines: i32, col: u16, row: u16) -> ScrollAction {
    let count = lines.unsigned_abs() as usize;
    let up = lines > 0;
    if modes.mouse_report {
        let button: u8 = if up { 64 } else { 65 };
        let event = if modes.sgr_mouse {
            format!("\x1b[<{button};{};{}M", col as u32 + 1, row as u32 + 1).into_bytes()
        } else {
            let coord = |v: u16| (32 + 1 + v.min(222)) as u8;
            vec![0x1b, b'[', b'M', 32 + button, coord(col), coord(row)]
        };
        return ScrollAction::Send(event.repeat(count));
    }
    if modes.alt_screen && modes.alternate_scroll {
        let arrow: &[u8] = match (up, modes.app_cursor) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        return ScrollAction::Send(arrow.repeat(count));
    }
    ScrollAction::Viewport(lines)
}

/// Where a scrollbar thumb sits on its track, in the track's own units (pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    /// Distance from the top of the track.
    pub top: f32,
    /// Length along the track.
    pub len: f32,
}

/// The thumb for a viewport `rows` tall that is `offset` lines above the live output, over `history` lines of
/// scrollback, on a track `track` long. `None` when there is nothing to scroll.
pub fn thumb(track: f32, rows: usize, history: usize, offset: usize, min_len: f32) -> Option<Thumb> {
    if history == 0 || track <= 0.0 {
        return None;
    }
    let len = (track * rows as f32 / (history + rows) as f32).max(min_len).min(track);
    let from_top = history - offset.min(history);
    Some(Thumb { top: (track - len) * from_top as f32 / history as f32, len })
}

/// The thumb of a view `viewport` long over `content` long content, `offset` from the start, on a track `track` long.
/// `None` when the content fits. The pixel form of `thumb`, for either axis of a scrolled list.
pub fn thumb_for_extent(track: f32, viewport: f32, content: f32, offset: f32, min_len: f32) -> Option<Thumb> {
    if content <= viewport + 0.5 || track <= 0.0 {
        return None;
    }
    let len = (track * viewport / content).max(min_len).min(track);
    let room = content - viewport;
    Some(Thumb { top: (track - len) * (offset / room).clamp(0.0, 1.0), len })
}

/// The offset for a thumb whose start is at `top`: the inverse of `thumb_for_extent`.
pub fn offset_for_extent(top: f32, track: f32, len: f32, viewport: f32, content: f32) -> f32 {
    let room = track - len;
    if room <= 0.0 {
        return 0.0;
    }
    (top / room).clamp(0.0, 1.0) * (content - viewport).max(0.0)
}

/// How long after the last scroll movement the thumb stays in its active color.
pub const ACTIVE_HOLD_MS: u64 = 600;
/// How long the thumb then takes to fade back to its idle color.
pub const ACTIVE_FADE_MS: u64 = 250;

/// How active the thumb looks `idle_ms` after the last scroll movement: 1 = the active color, 0 = the idle color.
pub fn activity(idle_ms: u64) -> f32 {
    if idle_ms <= ACTIVE_HOLD_MS {
        return 1.0;
    }
    1.0 - ((idle_ms - ACTIVE_HOLD_MS) as f32 / ACTIVE_FADE_MS as f32).min(1.0)
}

/// How long after the last scroll movement the jump to bottom chip stays on screen.
pub const CHIP_HOLD_MS: u64 = 1500;
/// The last stretch of `CHIP_HOLD_MS` in which the chip fades out.
pub const CHIP_FADE_MS: u64 = 250;

/// The jump to bottom chip's opacity `idle_ms` after the last scroll movement: 1 until the fade starts, then down to
/// 0 (gone) at `CHIP_HOLD_MS`.
pub fn chip_opacity(idle_ms: u64) -> f32 {
    let fade_from = CHIP_HOLD_MS - CHIP_FADE_MS;
    if idle_ms <= fade_from {
        return 1.0;
    }
    1.0 - ((idle_ms - fade_from) as f32 / CHIP_FADE_MS as f32).min(1.0)
}

// The chip stays after the thumb has gone back to grey.
const _: () = assert!(CHIP_HOLD_MS > ACTIVE_HOLD_MS + ACTIVE_FADE_MS);

/// How long the jump to bottom chip takes to scroll back to the live output.
pub const JUMP_MS: u64 = 250;

/// The scroll offset `elapsed_ms` into a jump to the bottom that started `start` lines above it: it eases out, fast
/// at first and settling at 0 once `JUMP_MS` have passed.
pub fn jump_offset(start: usize, elapsed_ms: u64) -> usize {
    let left = 1.0 - (elapsed_ms as f32 / JUMP_MS as f32).min(1.0);
    (start as f32 * left.powi(3)).round() as usize
}

/// The scroll offset (lines above the live output) for a thumb whose top edge is at `top`: the inverse of `thumb`.
pub fn offset_for_thumb_top(top: f32, track: f32, len: f32, history: usize) -> usize {
    let room = track - len;
    if room <= 0.0 {
        return 0;
    }
    let from_top = (top / room).clamp(0.0, 1.0) * history as f32;
    history - from_top.round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extent_thumb_sits_by_the_offset_and_round_trips() {
        assert_eq!(thumb_for_extent(100.0, 100.0, 100.0, 0.0, 10.0), None, "fits");
        let top = thumb_for_extent(100.0, 100.0, 400.0, 0.0, 10.0).unwrap();
        assert_eq!((top.top, top.len), (0.0, 25.0));
        let end = thumb_for_extent(100.0, 100.0, 400.0, 300.0, 10.0).unwrap();
        assert_eq!(end.top, 75.0);
        let short = thumb_for_extent(100.0, 100.0, 100_000.0, 0.0, 12.0).unwrap();
        assert_eq!(short.len, 12.0, "never shorter than the minimum");
        let mid = thumb_for_extent(100.0, 100.0, 400.0, 150.0, 10.0).unwrap();
        assert!((offset_for_extent(mid.top, 100.0, mid.len, 100.0, 400.0) - 150.0).abs() < 0.01);
        assert_eq!(offset_for_extent(-5.0, 100.0, 25.0, 100.0, 400.0), 0.0);
        assert_eq!(offset_for_extent(500.0, 100.0, 25.0, 100.0, 400.0), 300.0);
    }

    #[test]
    fn no_thumb_without_history() {
        assert_eq!(thumb(100.0, 24, 0, 0, 20.0), None);
    }

    #[test]
    fn thumb_is_at_the_bottom_when_live_and_at_the_top_at_the_oldest_line() {
        // 24 rows over 76 lines of history: 24% of a 400 px track.
        let live = thumb(400.0, 24, 76, 0, 20.0).unwrap();
        assert_eq!((live.top, live.len), (304.0, 96.0));
        let oldest = thumb(400.0, 24, 76, 76, 20.0).unwrap();
        assert_eq!(oldest.top, 0.0);
    }

    #[test]
    fn thumb_keeps_a_minimum_length() {
        assert_eq!(thumb(400.0, 24, 100_000, 0, 20.0).unwrap().len, 20.0);
    }

    #[test]
    fn the_thumb_holds_its_active_color_then_fades_to_idle() {
        assert_eq!(activity(0), 1.0);
        assert_eq!(activity(ACTIVE_HOLD_MS), 1.0);
        let halfway = activity(ACTIVE_HOLD_MS + ACTIVE_FADE_MS / 2);
        assert!((halfway - 0.5).abs() < 0.01, "{halfway}");
        assert_eq!(activity(ACTIVE_HOLD_MS + ACTIVE_FADE_MS), 0.0);
        assert_eq!(activity(60_000), 0.0);
    }

    #[test]
    fn the_chip_holds_then_fades_out_by_its_hold_time() {
        assert_eq!(chip_opacity(0), 1.0);
        assert_eq!(chip_opacity(CHIP_HOLD_MS - CHIP_FADE_MS), 1.0);
        let halfway = chip_opacity(CHIP_HOLD_MS - CHIP_FADE_MS / 2);
        assert!((halfway - 0.5).abs() < 0.01, "{halfway}");
        assert_eq!(chip_opacity(CHIP_HOLD_MS), 0.0);
        assert_eq!(chip_opacity(60_000), 0.0);
    }

    #[test]
    fn the_jump_to_bottom_eases_out_to_the_live_output() {
        assert_eq!(jump_offset(1000, 0), 1000);
        let mid = [50, 100, 150, 200].map(|ms| jump_offset(1000, ms));
        assert!(mid.windows(2).all(|pair| pair[0] > pair[1]), "{mid:?}");
        // Eased out: more than half the way in the first half of the time.
        assert!(jump_offset(1000, JUMP_MS / 2) < 500);
        assert_eq!(jump_offset(1000, JUMP_MS), 0);
        assert_eq!(jump_offset(1000, 10_000), 0);
        assert_eq!(jump_offset(0, 100), 0);
    }

    #[test]
    fn dragging_maps_back_to_the_offset() {
        for offset in [0, 1, 40, 76] {
            let t = thumb(400.0, 24, 76, offset, 20.0).unwrap();
            assert_eq!(offset_for_thumb_top(t.top, 400.0, t.len, 76), offset);
        }
        assert_eq!(offset_for_thumb_top(-5.0, 400.0, 100.0, 76), 76);
        assert_eq!(offset_for_thumb_top(900.0, 400.0, 100.0, 76), 0);
    }

    #[test]
    fn normal_screen_scrolls_viewport() {
        assert_eq!(scroll_action(ScrollModes::default(), 3, 0, 0), ScrollAction::Viewport(3));
    }

    #[test]
    fn alternate_screen_sends_arrows() {
        let modes = ScrollModes { alt_screen: true, alternate_scroll: true, ..Default::default() };
        assert_eq!(scroll_action(modes, 2, 0, 0), ScrollAction::Send(b"\x1b[A\x1b[A".to_vec()));
        let app = ScrollModes { app_cursor: true, ..modes };
        assert_eq!(scroll_action(app, -1, 0, 0), ScrollAction::Send(b"\x1bOB".to_vec()));
    }

    #[test]
    fn mouse_reporting_sends_wheel_events() {
        let sgr = ScrollModes { mouse_report: true, sgr_mouse: true, ..Default::default() };
        assert_eq!(scroll_action(sgr, 1, 4, 2), ScrollAction::Send(b"\x1b[<64;5;3M".to_vec()));
        assert_eq!(scroll_action(sgr, -1, 0, 0), ScrollAction::Send(b"\x1b[<65;1;1M".to_vec()));
        let legacy = ScrollModes { mouse_report: true, ..Default::default() };
        assert_eq!(scroll_action(legacy, 1, 0, 0), ScrollAction::Send(vec![0x1b, b'[', b'M', 96, 33, 33]));
    }
}
