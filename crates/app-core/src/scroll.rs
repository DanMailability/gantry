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

#[cfg(test)]
mod tests {
    use super::*;

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
