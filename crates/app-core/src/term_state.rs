//! Terminal emulator state for one pane, wrapping `alacritty_terminal`.

use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb as VteRgb};

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
    /// Report motion while a button is held (1002).
    pub mouse_drag: bool,
    /// Report all motion (1003).
    pub mouse_motion: bool,
    /// DECCKM application cursor keys.
    pub app_cursor: bool,
}

type ColorFormatter = Arc<dyn Fn(VteRgb) -> String + Send + Sync>;
type SizeFormatter = Arc<dyn Fn(WindowSize) -> String + Send + Sync>;

/// A reply the terminal owes the program, in the order it was requested.
enum Reply {
    Text(String),
    Color(usize, ColorFormatter),
    TextAreaSize(SizeFormatter),
}

#[derive(Clone)]
pub(crate) struct Listener {
    replies: Arc<Mutex<Vec<Reply>>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::PtyWrite(text) => Reply::Text(text),
            Event::ColorRequest(index, format) => Reply::Color(index, format),
            Event::TextAreaSizeRequest(format) => Reply::TextAreaSize(format),
            Event::Title(title) => {
                *self.title.lock().unwrap() = Some(title);
                return;
            }
            Event::ResetTitle => {
                *self.title.lock().unwrap() = None;
                return;
            }
            _ => return,
        };
        self.replies.lock().unwrap().push(reply);
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
    pub(crate) term: Term<Listener>,
    parser: Processor,
    replies: Arc<Mutex<Vec<Reply>>>,
    title: Arc<Mutex<Option<String>>>,
    palette: Palette,
    cell_size: (u16, u16),
}

impl TermState {
    /// Create an empty terminal of the given size with kitty keyboard support enabled.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_config(cols, rows, Config { kitty_keyboard: true, ..Config::default() })
    }

    /// Like `new`, keeping `lines` lines of scrollback.
    pub fn with_scrollback(cols: u16, rows: u16, lines: usize) -> Self {
        Self::with_config(cols, rows, Config { kitty_keyboard: true, scrolling_history: lines, ..Config::default() })
    }

    fn with_config(cols: u16, rows: u16, config: Config) -> Self {
        let replies = Arc::new(Mutex::new(Vec::new()));
        let title = Arc::new(Mutex::new(None));
        let listener = Listener { replies: replies.clone(), title: title.clone() };
        let size = GridSize { cols: cols as usize, rows: rows as usize };
        TermState { term: Term::new(config, &size, listener), parser: Processor::new(), replies, title, palette: DARK, cell_size: (0, 0) }
    }

    /// Use `palette` from now on, including for colour query answers.
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Feed program output into the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Take the bytes the terminal wants written back to the program (replies to queries).
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        let replies = std::mem::take(&mut *self.replies.lock().unwrap());
        let mut bytes = Vec::new();
        for reply in replies {
            let text = match reply {
                Reply::Text(text) => text,
                Reply::Color(index, format) => {
                    let color = self.term.colors()[index].unwrap_or_else(|| {
                        let rgb = self.default_color(index);
                        VteRgb { r: rgb.r, g: rgb.g, b: rgb.b }
                    });
                    format(color)
                }
                Reply::TextAreaSize(format) => format(WindowSize {
                    num_lines: self.term.screen_lines() as u16,
                    num_cols: self.term.columns() as u16,
                    cell_width: self.cell_size.0,
                    cell_height: self.cell_size.1,
                }),
            };
            bytes.extend_from_slice(text.as_bytes());
        }
        bytes
    }

    /// Palette color for an OSC color index: 0–255 indexed, 256 foreground, 257 background, 258 cursor.
    fn default_color(&self, index: usize) -> Rgb {
        match index {
            0..=255 => self.palette.indexed(index as u8),
            256 => self.palette.foreground,
            257 => self.palette.background,
            258 => self.palette.cursor,
            _ => self.palette.foreground,
        }
    }

    /// Window title set by the program (OSC 0/2), if any.
    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    /// Change the grid size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols: cols as usize, rows: rows as usize });
    }

    /// Record the cell size in pixels, used to answer text-area size queries.
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_size = (width, height);
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
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
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
    fn color_queries_are_answered_from_the_palette() {
        let mut t = TermState::new(10, 3);
        t.feed(b"\x1b[6n\x1b]11;?\x07\x1b]10;?\x1b\\");
        let replies = String::from_utf8(t.take_pty_writes()).unwrap();
        assert_eq!(
            replies,
            "\x1b[1;1R\x1b]11;rgb:1414/1818/2121\x07\x1b]10;rgb:d8d8/dddd/e8e8\x1b\\"
        );
    }

    #[test]
    fn set_palette_changes_query_answers() {
        let mut t = TermState::new(10, 3);
        t.set_palette(crate::palette::LIGHT);
        t.feed(b"\x1b]11;?\x07");
        assert_eq!(String::from_utf8(t.take_pty_writes()).unwrap(), "\x1b]11;rgb:f8f8/f9f9/fcfc\x07");
    }

    #[test]
    fn text_area_pixel_size_query_is_answered() {
        let mut t = TermState::new(10, 3);
        t.set_cell_size(8, 18);
        t.feed(b"\x1b[14t");
        assert_eq!(t.take_pty_writes(), b"\x1b[4;54;80t".to_vec());
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
    fn osc_title_is_tracked() {
        let mut t = TermState::new(10, 3);
        assert_eq!(t.title(), None);
        t.feed(b"\x1b]2;build: ok\x07");
        assert_eq!(t.title().as_deref(), Some("build: ok"));
        t.feed(b"\x1b]0;second\x1b\\");
        assert_eq!(t.title().as_deref(), Some("second"));
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

    #[test]
    fn with_scrollback_keeps_that_many_lines() {
        let mut t = TermState::with_scrollback(10, 5, 1_000);
        for i in 0..3_000 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        t.scroll_viewport(1_000_000);
        assert_eq!(t.row_text(0), "line1996");
    }
}
