//! Terminal emulator state for one pane, wrapping `alacritty_terminal`.

use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};

use crate::palette::{DARK, Palette};
use crate::snapshot::{CellStyle, CellView, CursorView, Rgb, ScreenSnapshot};

/// Keyboard-relevant terminal modes set by the running program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: arrow keys send `ESC O x` instead of `ESC [ x`.
    pub app_cursor: bool,
    /// Kitty keyboard protocol "disambiguate escape codes" flag.
    pub kitty_disambiguate: bool,
}

/// Mouse-wheel-relevant terminal modes set by the running program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollModes {
    /// Alternate screen is active.
    pub alt_screen: bool,
    /// Wheel should become arrow keys on the alternate screen.
    pub alternate_scroll: bool,
    /// The program wants mouse events.
    pub mouse_report: bool,
    /// Mouse events use SGR encoding.
    pub sgr_mouse: bool,
    /// DECCKM application cursor keys.
    pub app_cursor: bool,
}

#[derive(Clone)]
struct Listener {
    pty_writes: Arc<Mutex<Vec<u8>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.pty_writes.lock().unwrap().extend_from_slice(text.as_bytes());
        }
    }
}

struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// One pane's terminal: parser, grid, scrollback, modes, and selection.
pub struct TermState {
    term: Term<Listener>,
    parser: Processor,
    pty_writes: Arc<Mutex<Vec<u8>>>,
    palette: Palette,
}

impl TermState {
    /// Create an empty terminal of the given size with kitty keyboard support enabled.
    pub fn new(cols: u16, rows: u16) -> Self {
        let pty_writes = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener { pty_writes: pty_writes.clone() };
        let config = Config { kitty_keyboard: true, ..Config::default() };
        let size = GridSize { cols: cols as usize, rows: rows as usize };
        TermState { term: Term::new(config, &size, listener), parser: Processor::new(), pty_writes, palette: DARK }
    }

    /// Feed program output into the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Take the bytes the terminal wants written back to the program (replies to queries).
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.pty_writes.lock().unwrap())
    }

    /// Change the grid size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols: cols as usize, rows: rows as usize });
    }

    /// Modes that affect key encoding.
    pub fn key_modes(&self) -> KeyModes {
        let mode = self.term.mode();
        KeyModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            kitty_disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
        }
    }

    /// Modes that affect mouse-wheel handling.
    pub fn scroll_modes(&self) -> ScrollModes {
        let mode = self.term.mode();
        ScrollModes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            mouse_report: mode.intersects(TermMode::MOUSE_MODE),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
        }
    }

    /// Whether the program enabled bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Scroll the viewport; positive moves towards older output.
    pub fn scroll_viewport(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Jump the viewport back to live output.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Begin a selection at a viewport cell.
    pub fn start_selection(&mut self, col: u16, row: u16, right_half: bool) {
        let point = self.viewport_point(col, row);
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, side(right_half)));
    }

    /// Extend the current selection to a viewport cell.
    pub fn update_selection(&mut self, col: u16, row: u16, right_half: bool) {
        let point = self.viewport_point(col, row);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side(right_half));
        }
    }

    /// Remove any selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Selected text in logical order, or `None` when nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|text| !text.is_empty())
    }

    /// Text of a viewport row, one char per cell (wide-char spacers become spaces), trailing spaces trimmed.
    pub fn row_text(&self, row: u16) -> String {
        let line = Line(row as i32 - self.term.grid().display_offset() as i32);
        let grid_row = &self.term.grid()[line];
        let text: String = (0..self.term.columns())
            .map(|col| {
                let cell = &grid_row[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c }
            })
            .collect();
        text.trim_end().to_string()
    }

    /// Viewport `(col, row)` of the cursor, if it is visible on screen.
    pub fn cursor_cell(&self) -> Option<(usize, usize)> {
        let point = self.term.grid().cursor.point;
        let row = point.line.0 + self.term.grid().display_offset() as i32;
        (0..self.term.screen_lines() as i32).contains(&row).then_some((point.column.0, row as usize))
    }

    /// Resolve the visible screen into plain data for rendering.
    pub fn snapshot(&self) -> ScreenSnapshot {
        let content = self.term.renderable_content();
        let cols = self.term.columns();
        let rows = self.term.screen_lines();
        let offset = content.display_offset as i32;
        let blank = CellView {
            ch: ' ',
            combining: None,
            style: CellStyle {
                fg: self.palette.foreground,
                bg: self.palette.background,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            },
            wide: false,
            spacer: false,
        };
        let mut grid = vec![vec![blank; cols]; rows];
        let selection = content.selection;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= rows || col >= cols {
                continue;
            }
            let selected = selection.is_some_and(|range| range.contains(indexed.point));
            grid[row as usize][col] = self.cell_view(indexed.cell, content.colors, selected);
        }
        let cursor = (content.mode.contains(TermMode::SHOW_CURSOR)
            && content.cursor.shape != CursorShape::Hidden)
            .then(|| {
                let row = content.cursor.point.line.0 + offset;
                let col = content.cursor.point.column.0;
                (row >= 0 && (row as usize) < rows && col < cols).then(|| {
                    let cell = &grid[row as usize][col];
                    CursorView { col, row: row as usize, wide: cell.wide, ch: cell.ch, style: cell.style }
                })
            })
            .flatten();
        ScreenSnapshot {
            cols,
            rows: grid,
            cursor,
            background: self.palette.background,
            cursor_color: self.palette.cursor,
        }
    }

    fn viewport_point(&self, col: u16, row: u16) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        Point::new(Line(row as i32 - offset), Column(col as usize))
    }

    fn cell_view(&self, cell: &Cell, colors: &Colors, selected: bool) -> CellView {
        let flags = cell.flags;
        let mut fg = self.resolve(cell.fg, colors);
        let mut bg = self.resolve(cell.bg, colors);
        if flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        if selected {
            bg = self.palette.selection;
        }
        CellView {
            ch: cell.c,
            combining: cell.zerowidth().map(|marks| marks.to_vec()),
            style: CellStyle {
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: flags.contains(Flags::STRIKEOUT),
            },
            wide: flags.contains(Flags::WIDE_CHAR),
            spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
        }
    }

    fn resolve(&self, color: Color, colors: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => Rgb { r: rgb.r, g: rgb.g, b: rgb.b },
            Color::Indexed(index) => colors[index as usize]
                .map(|rgb| Rgb { r: rgb.r, g: rgb.g, b: rgb.b })
                .unwrap_or_else(|| self.palette.indexed(index)),
            Color::Named(name) => colors[name as usize]
                .map(|rgb| Rgb { r: rgb.r, g: rgb.g, b: rgb.b })
                .unwrap_or_else(|| self.named(name)),
        }
    }

    fn named(&self, name: NamedColor) -> Rgb {
        let index = name as usize;
        match name {
            NamedColor::Foreground | NamedColor::BrightForeground => self.palette.foreground,
            NamedColor::DimForeground => dim(self.palette.foreground),
            NamedColor::Background => self.palette.background,
            NamedColor::Cursor => self.palette.cursor,
            _ if index < 16 => self.palette.ansi[index],
            // DimBlack..=DimWhite follow Cursor (258) in declaration order.
            _ => dim(self.palette.ansi[index - NamedColor::DimBlack as usize]),
        }
    }
}

fn side(right_half: bool) -> Side {
    if right_half { Side::Right } else { Side::Left }
}

fn dim(color: Rgb) -> Rgb {
    let scale = |v: u8| (v as u16 * 2 / 3) as u8;
    Rgb { r: scale(color.r), g: scale(color.g), b: scale(color.b) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::DARK;
    use crate::snapshot::Rgb;

    #[test]
    fn plain_text_lands_in_grid_and_moves_cursor() {
        let mut t = TermState::new(10, 3);
        t.feed(b"hi");
        let s = t.snapshot();
        assert_eq!((s.rows[0][0].ch, s.rows[0][1].ch), ('h', 'i'));
        let c = s.cursor.unwrap();
        assert_eq!((c.col, c.row), (2, 0));
    }

    #[test]
    fn sgr_colors_resolve() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[31mA\x1b[38;2;1;2;3mB\x1b[0mC\x1b[38;5;196mD\x1b[38;5;244mE");
        let row = &t.snapshot().rows[0];
        assert_eq!(row[0].style.fg, DARK.ansi[1]);
        assert_eq!(row[1].style.fg, Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(row[2].style.fg, DARK.foreground);
        assert_eq!(row[3].style.fg, Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(row[4].style.fg, Rgb { r: 128, g: 128, b: 128 });
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[7mZ");
        let style = t.snapshot().rows[0][0].style;
        assert_eq!((style.fg, style.bg), (DARK.background, DARK.foreground));
    }

    #[test]
    fn wide_char_marks_following_spacer() {
        let mut t = TermState::new(10, 3);
        t.feed("漢a".as_bytes());
        let row = &t.snapshot().rows[0];
        assert!(row[0].wide);
        assert!(row[1].spacer);
        assert_eq!(row[2].ch, 'a');
    }

    #[test]
    fn cursor_position_query_is_answered_once() {
        let mut t = TermState::new(10, 5);
        t.feed(b"\x1b[2;3H\x1b[6n");
        assert_eq!(t.take_pty_writes(), b"\x1b[2;3R".to_vec());
        assert!(t.take_pty_writes().is_empty());
    }

    #[test]
    fn kitty_keyboard_query_and_push_pop() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[?u");
        assert_eq!(t.take_pty_writes(), b"\x1b[?0u".to_vec());
        assert!(!t.key_modes().kitty_disambiguate);
        t.feed(b"\x1b[>1u");
        assert!(t.key_modes().kitty_disambiguate);
        t.feed(b"\x1b[<u");
        assert!(!t.key_modes().kitty_disambiguate);
    }

    #[test]
    fn modes_are_reported() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        assert!(t.key_modes().app_cursor);
        assert!(t.bracketed_paste());
        let s = t.scroll_modes();
        assert!(s.alt_screen && s.mouse_report && s.sgr_mouse && s.app_cursor);
    }

    #[test]
    fn selection_copies_logical_text() {
        let mut t = TermState::new(20, 3);
        t.feed(b"hello world");
        t.start_selection(0, 0, false);
        t.update_selection(4, 0, true);
        assert_eq!(t.selection_text().as_deref(), Some("hello"));
        assert_eq!(t.snapshot().rows[0][0].style.bg, DARK.selection);
        t.clear_selection();
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut t = TermState::new(10, 3);
        t.resize(20, 5);
        let s = t.snapshot();
        assert_eq!((s.cols, s.rows.len()), (20, 5));
    }

    #[test]
    fn viewport_scrolls_through_history() {
        let mut t = TermState::new(10, 5);
        for i in 0..30 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(t.row_text(0), "line26");
        t.scroll_viewport(5);
        assert_eq!(t.row_text(0), "line21");
        t.scroll_to_bottom();
        assert_eq!(t.row_text(0), "line26");
    }

    #[test]
    fn history_is_bounded() {
        let mut t = TermState::new(10, 5);
        for i in 0..20_000 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        t.scroll_viewport(1_000_000);
        assert_eq!(t.row_text(0), "line9996");
    }
}
