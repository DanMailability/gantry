//! Mouse buttons and motion for programs that turned on mouse reporting (xterm modes 1000/1002/1003; SGR 1006 or
//! the legacy byte encoding). The wheel is in `scroll`.

use crate::term_state::ScrollModes;

/// A mouse button a program can be told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Primary button.
    Left,
    /// Wheel button.
    Middle,
    /// Secondary button.
    Right,
}

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// A button went down.
    Press(Button),
    /// A button came up.
    Release(Button),
    /// The pointer entered another cell, with this button held (if any).
    Motion(Option<Button>),
}

/// Modifier keys held during a mouse event (Shift is never reported: Shift+drag selects).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMods {
    /// Alt / Meta.
    pub alt: bool,
    /// Control.
    pub ctrl: bool,
}

/// The bytes that report `action` at viewport cell `(col, row)`, or `None` when the program's mouse mode does not
/// ask for it.
pub fn encode_mouse(modes: ScrollModes, action: MouseAction, mods: MouseMods, col: u16, row: u16) -> Option<Vec<u8>> {
    if !modes.mouse_report {
        return None;
    }
    let mut code = match action {
        MouseAction::Press(button) | MouseAction::Release(button) => button_code(button),
        MouseAction::Motion(Some(button)) if modes.mouse_drag || modes.mouse_motion => button_code(button) + 32,
        MouseAction::Motion(None) if modes.mouse_motion => 3 + 32,
        MouseAction::Motion(_) => return None,
    };
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    let release = matches!(action, MouseAction::Release(_));
    Some(if modes.sgr_mouse {
        let end = if release { 'm' } else { 'M' };
        format!("\x1b[<{code};{};{}{end}", col as u32 + 1, row as u32 + 1).into_bytes()
    } else {
        // Legacy encoding has no release button: release is button 3.
        let code = if release { (code & !3) | 3 } else { code };
        let coord = |v: u16| (32 + 1 + v.min(222)) as u8;
        vec![0x1b, b'[', b'M', 32 + code, coord(col), coord(row)]
    })
}

fn button_code(button: Button) -> u8 {
    match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term_state::TermState;

    fn sgr() -> ScrollModes {
        ScrollModes { mouse_report: true, sgr_mouse: true, ..Default::default() }
    }
    const NONE: MouseMods = MouseMods { alt: false, ctrl: false };

    #[test]
    fn sgr_reports_press_and_release() {
        assert_eq!(encode_mouse(sgr(), MouseAction::Press(Button::Left), NONE, 4, 2), Some(b"\x1b[<0;5;3M".to_vec()));
        assert_eq!(encode_mouse(sgr(), MouseAction::Release(Button::Left), NONE, 4, 2), Some(b"\x1b[<0;5;3m".to_vec()));
        let ctrl = MouseMods { alt: false, ctrl: true };
        assert_eq!(encode_mouse(sgr(), MouseAction::Press(Button::Right), ctrl, 0, 0), Some(b"\x1b[<18;1;1M".to_vec()));
    }

    #[test]
    fn legacy_release_is_button_three() {
        let legacy = ScrollModes { mouse_report: true, ..Default::default() };
        assert_eq!(
            encode_mouse(legacy, MouseAction::Press(Button::Middle), NONE, 4, 2),
            Some(vec![0x1b, b'[', b'M', 33, 37, 35])
        );
        assert_eq!(
            encode_mouse(legacy, MouseAction::Release(Button::Middle), NONE, 4, 2),
            Some(vec![0x1b, b'[', b'M', 35, 37, 35])
        );
    }

    #[test]
    fn legacy_coordinates_clamp_at_the_protocol_limit() {
        let legacy = ScrollModes { mouse_report: true, ..Default::default() };
        let bytes = encode_mouse(legacy, MouseAction::Press(Button::Left), NONE, 400, 300).unwrap();
        assert_eq!(&bytes[4..], &[255, 255]);
    }

    #[test]
    fn motion_is_reported_only_when_the_mode_asks() {
        let click = sgr();
        assert_eq!(encode_mouse(click, MouseAction::Motion(Some(Button::Left)), NONE, 1, 0), None);
        let drag = ScrollModes { mouse_drag: true, ..sgr() };
        assert_eq!(
            encode_mouse(drag, MouseAction::Motion(Some(Button::Left)), NONE, 1, 0),
            Some(b"\x1b[<32;2;1M".to_vec())
        );
        assert_eq!(encode_mouse(drag, MouseAction::Motion(None), NONE, 1, 0), None);
        let motion = ScrollModes { mouse_motion: true, ..sgr() };
        assert_eq!(encode_mouse(motion, MouseAction::Motion(None), NONE, 0, 0), Some(b"\x1b[<35;1;1M".to_vec()));
    }

    #[test]
    fn nothing_is_reported_without_a_mouse_mode() {
        let off = ScrollModes::default();
        assert_eq!(encode_mouse(off, MouseAction::Press(Button::Left), NONE, 0, 0), None);
        assert_eq!(encode_mouse(off, MouseAction::Release(Button::Left), NONE, 0, 0), None);
    }

    #[test]
    fn the_terminal_reports_which_mouse_mode_is_on() {
        let mut term = TermState::new(10, 3);
        term.feed(b"\x1b[?1002h\x1b[?1006h");
        let modes = term.scroll_modes();
        assert!(modes.mouse_report && modes.mouse_drag && !modes.mouse_motion && modes.sgr_mouse);
        term.feed(b"\x1b[?1003h");
        assert!(term.scroll_modes().mouse_motion);
    }
}
