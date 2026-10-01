//! Serialize a terminal's state back into an escape-sequence stream (for reattaching a GUI).

use std::fmt::Write as _;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use crate::term_state::TermState;

/// DECSET/DECRST modes replayed when they differ from a fresh terminal.
const PRIVATE_MODES: &[(TermMode, u16)] = &[
    (TermMode::APP_CURSOR, 1),
    (TermMode::LINE_WRAP, 7),
    (TermMode::MOUSE_REPORT_CLICK, 1000),
    (TermMode::MOUSE_DRAG, 1002),
    (TermMode::MOUSE_MOTION, 1003),
    (TermMode::FOCUS_IN_OUT, 1004),
    (TermMode::SGR_MOUSE, 1006),
    (TermMode::ALTERNATE_SCROLL, 1007),
    (TermMode::BRACKETED_PASTE, 2004),
];

/// Kitty keyboard flag bits in protocol order.
const KITTY_FLAGS: &[(TermMode, u8)] = &[
    (TermMode::DISAMBIGUATE_ESC_CODES, 1),
    (TermMode::REPORT_EVENT_TYPES, 2),
    (TermMode::REPORT_ALTERNATE_KEYS, 4),
    (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
    (TermMode::REPORT_ASSOCIATED_TEXT, 16),
];

impl TermState {
    /// Escape stream that rebuilds this terminal in a fresh one of the same size.
    ///
    /// Replays the active grid (with scrollback when on the primary screen), cell styles, the
    /// cursor, terminal modes, kitty keyboard flags, cursor style, palette overrides, and the
    /// window title. While the alternate screen is active only the alternate screen is replayed.
    /// Scroll region, charsets, tab stops, and hyperlinks are not replayed.
    pub fn replay_bytes(&self) -> Vec<u8> {
        let term = &self.term;
        let mode = *term.mode();
        let mut out = String::new();
        if let Some(title) = self.title() {
            let title: String = title.chars().filter(|c| !c.is_control()).collect();
            write!(out, "\x1b]2;{title}\x07").unwrap();
        }
        for index in 0..=258 {
            if let Some(rgb) = term.colors()[index] {
                let spec = format!("rgb:{:02x}/{:02x}/{:02x}", rgb.r, rgb.g, rgb.b);
                match index {
                    0..=255 => write!(out, "\x1b]4;{index};{spec}\x07").unwrap(),
                    256..=258 => write!(out, "\x1b]{};{spec}\x07", index - 246).unwrap(),
                    _ => {}
                }
            }
        }
        if mode.contains(TermMode::ALT_SCREEN) {
            out.push_str("\x1b[?1049h\x1b[H");
        }
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        let bottom = term.screen_lines() as i32 - 1;
        let columns = term.columns();
        let mut style = String::from("0");
        for line in top..=bottom {
            let row = &grid[Line(line)];
            let wrapped = row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            let used = if wrapped {
                columns
            } else {
                (0..columns).rev().find(|&c| !is_blank(&row[Column(c)])).map_or(0, |c| c + 1)
            };
            for col in 0..used {
                let cell = &row[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let cell_style = sgr(cell);
                if cell_style != style {
                    write!(out, "\x1b[{cell_style}m").unwrap();
                    style = cell_style;
                }
                out.push(cell.c);
                if let Some(marks) = cell.zerowidth() {
                    out.extend(marks.iter());
                }
            }
            if line < bottom && !wrapped {
                if style != "0" {
                    out.push_str("\x1b[0m");
                    style = String::from("0");
                }
                out.push_str("\r\n");
            }
        }
        out.push_str("\x1b[0m");
        let fresh = TermState::new(columns as u16, term.screen_lines() as u16);
        let fresh_mode = *fresh.term.mode();
        for &(flag, number) in PRIVATE_MODES {
            if mode.contains(flag) != fresh_mode.contains(flag) {
                write!(out, "\x1b[?{number}{}", if mode.contains(flag) { 'h' } else { 'l' }).unwrap();
            }
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            out.push_str("\x1b=");
        }
        if mode.contains(TermMode::INSERT) {
            out.push_str("\x1b[4h");
        }
        let kitty: u8 = KITTY_FLAGS.iter().filter(|(flag, _)| mode.contains(*flag)).map(|(_, bit)| bit).sum();
        if kitty > 0 {
            write!(out, "\x1b[>{kitty}u").unwrap();
        }
        let cursor_style = term.cursor_style();
        if cursor_style != fresh.term.cursor_style() {
            let base = match cursor_style.shape {
                CursorShape::Underline => 3,
                CursorShape::Beam => 5,
                _ => 1,
            };
            write!(out, "\x1b[{} q", if cursor_style.blinking { base } else { base + 1 }).unwrap();
        }
        let cursor = &grid.cursor;
        if cursor.input_needs_wrap {
            // A wrap is pending after writing the last column; re-print that cell to recreate it
            // (positioning the cursor directly would cancel the pending wrap).
            let row = &grid[cursor.point.line];
            let mut col = cursor.point.column.0;
            if col > 0 && row[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
                col -= 1;
            }
            let cell = &row[Column(col)];
            write!(out, "\x1b[{};{}H\x1b[{}m{}", cursor.point.line.0 + 1, col + 1, sgr(cell), cell.c).unwrap();
            if let Some(marks) = cell.zerowidth() {
                out.extend(marks.iter());
            }
            out.push_str("\x1b[0m");
        } else {
            write!(out, "\x1b[{};{}H", cursor.point.line.0 + 1, cursor.point.column.0 + 1).unwrap();
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        out.into_bytes()
    }
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.zerowidth().is_none()
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
}

/// SGR parameters (without `ESC [` / `m`) that recreate a cell's style from a reset state.
fn sgr(cell: &Cell) -> String {
    let mut params = vec![String::from("0")];
    let flags = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if flags.contains(flag) {
            params.push(code.into());
        }
    }
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        params.push("21".into());
    } else if flags.contains(Flags::UNDERCURL) {
        params.push("4:3".into());
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        params.push("4:4".into());
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        params.push("4:5".into());
    } else if flags.contains(Flags::UNDERLINE) {
        params.push("4".into());
    }
    if let Some(fg) = color_param(cell.fg, 30, 90, 38, NamedColor::Foreground) {
        params.push(fg);
    }
    if let Some(bg) = color_param(cell.bg, 40, 100, 48, NamedColor::Background) {
        params.push(bg);
    }
    params.join(";")
}

fn color_param(color: Color, normal: u16, bright: u16, extended: u16, default: NamedColor) -> Option<String> {
    match color {
        Color::Named(name) if name == default => None,
        Color::Named(name) => {
            let index = name as u16;
            match index {
                0..=7 => Some((normal + index).to_string()),
                8..=15 => Some((bright + index - 8).to_string()),
                259..=266 => Some((normal + index - 259).to_string()),
                _ => None,
            }
        }
        Color::Indexed(index) => Some(format!("{extended};5;{index}")),
        Color::Spec(rgb) => Some(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

#[cfg(test)]
mod tests {
    use crate::term_state::TermState;

    fn replayed(original: &TermState, cols: u16, rows: u16) -> TermState {
        let mut copy = TermState::new(cols, rows);
        copy.feed(&original.replay_bytes());
        copy
    }

    fn assert_same_screen(a: &TermState, b: &TermState) {
        let (sa, sb) = (a.snapshot(), b.snapshot());
        assert_eq!(sa.rows, sb.rows, "visible cells differ");
        assert_eq!(sa.cursor, sb.cursor, "cursor differs");
        assert_eq!(a.key_modes(), b.key_modes());
        assert_eq!(a.scroll_modes(), b.scroll_modes());
        assert_eq!(a.bracketed_paste(), b.bracketed_paste());
    }

    #[test]
    fn styled_text_and_cursor_survive() {
        let mut t = TermState::new(20, 4);
        t.feed(b"\x1b[1;31mbold red\x1b[0m \x1b[3;4;38;2;1;2;3mrgb\x1b[0m\r\n\x1b[7;9;48;5;120minv\x1b[0m \x1b[2mdim\x1b[8mhid\x1b[0m\r\nx\x1b[2;5H");
        assert_same_screen(&t, &replayed(&t, 20, 4));
    }

    #[test]
    fn wide_chars_and_combining_marks_survive() {
        let mut t = TermState::new(12, 3);
        t.feed("漢字 ok\r\nש\u{05B8}לום ● 😀".as_bytes());
        assert_same_screen(&t, &replayed(&t, 12, 3));
    }

    #[test]
    fn soft_wrapped_lines_stay_wrapped() {
        let mut t = TermState::new(10, 4);
        t.feed(b"abcdefghijklmnop\r\nnext");
        let copy = replayed(&t, 10, 4);
        assert_same_screen(&t, &copy);
        let mut narrow = copy;
        narrow.resize(20, 4);
        assert_eq!(narrow.row_text(0), "abcdefghijklmnop", "wrap flag lost: line did not reflow");
    }

    #[test]
    fn scrollback_survives() {
        let mut t = TermState::new(10, 3);
        for i in 0..40 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        let mut copy = replayed(&t, 10, 3);
        assert_same_screen(&t, &copy);
        t.scroll_viewport(1000);
        copy.scroll_viewport(1000);
        assert_eq!(copy.row_text(0), t.row_text(0));
        assert_eq!(copy.row_text(0), "line0");
    }

    #[test]
    fn pending_wrap_at_the_last_column_survives() {
        let mut t = TermState::new(10, 3);
        t.feed(b"abcdefghij");
        let mut copy = replayed(&t, 10, 3);
        t.feed(b"K");
        copy.feed(b"K");
        assert_eq!((copy.row_text(0), copy.row_text(1)), (t.row_text(0), t.row_text(1)));
    }

    #[test]
    fn modes_and_hidden_cursor_survive() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[>1u\x1b[?25l\x1b[5 q");
        let copy = replayed(&t, 10, 3);
        assert_same_screen(&t, &copy);
        assert!(copy.snapshot().cursor.is_none());
        assert!(copy.key_modes().kitty_disambiguate);
    }

    #[test]
    fn palette_overrides_survive() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b]11;rgb:10/20/30\x07\x1b]4;1;rgb:ff/00/ff\x07\x1b[31mR");
        assert_same_screen(&t, &replayed(&t, 10, 3));
    }

    #[test]
    fn alternate_screen_content_survives() {
        let mut t = TermState::new(10, 3);
        t.feed(b"primary\x1b[?1049h\x1b[2;2Halt");
        let copy = replayed(&t, 10, 3);
        assert_same_screen(&t, &copy);
        assert!(copy.scroll_modes().alt_screen);
    }

    #[test]
    fn title_survives_replay() {
        let mut a = TermState::new(20, 4);
        a.feed(b"\x1b]2;my task\x07hi");
        let mut b = TermState::new(20, 4);
        b.feed(&a.replay_bytes());
        assert_eq!(b.title().as_deref(), Some("my task"));
        assert_eq!(b.row_text(0), "hi");
    }
}
