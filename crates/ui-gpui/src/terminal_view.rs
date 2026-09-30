//! GPUI view that renders one terminal pane and exchanges bytes with its PTY.

use std::io::Read;
use std::ops::Range;

use app_core::input::{KeyInput, encode_key};
use app_core::layout::grid_dims;
use app_core::paste::encode_paste;
use app_core::scroll::{ScrollAction, scroll_action};
use app_core::url::link_at;
use app_core::snapshot::{CellStyle, Rgb, ScreenSnapshot, row_spans};
use app_core::term_state::TermState;
use daemon::pty::PtyProcess;
use gpui::{
    App, BorderStyle, Bounds, Context, FocusHandle, Font, FontStyle, FontWeight, Hsla, Pixels,
    Point, StrikethroughStyle, Task, TextAlign, TextRun, UnderlineStyle, Window, canvas, div,
    fill, font, outline, point, prelude::*, px, rgb, size,
};
use gpui::{
    ClipboardItem, ElementInputHandler, EntityInputHandler, KeyBinding, KeyDownEvent, MouseButton,
    Modifiers, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent,
    UTF16Selection,
};

#[allow(missing_docs)]
mod terminal_actions {
    gpui::actions!(terminal, [Copy, Paste, ZoomIn, ZoomOut, ZoomReset]);
}
use terminal_actions::{Copy, Paste, ZoomIn, ZoomOut, ZoomReset};

/// Register terminal shortcuts: `Ctrl+Shift+…` on Linux/Windows, `Cmd+…` on macOS.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(key_bindings());
}

fn key_bindings() -> Vec<KeyBinding> {
    #[cfg(target_os = "macos")]
    let m = "cmd";
    #[cfg(not(target_os = "macos"))]
    let m = "ctrl-shift";
    let context = Some("Terminal");
    let mut bindings = vec![
        KeyBinding::new(&format!("{m}-c"), Copy, context),
        KeyBinding::new(&format!("{m}-v"), Paste, context),
        KeyBinding::new(&format!("{m}-="), ZoomIn, context),
        KeyBinding::new(&format!("{m}-+"), ZoomIn, context),
        KeyBinding::new(&format!("{m}--"), ZoomOut, context),
        KeyBinding::new(&format!("{m}-0"), ZoomReset, context),
    ];
    // GPUI reports Shift+symbol/digit as the shifted character without Shift,
    // so Ctrl+Shift+= / - / 0 arrive as ctrl-+ / ctrl-_ / ctrl-) (US layout).
    #[cfg(not(target_os = "macos"))]
    bindings.extend([
        KeyBinding::new("ctrl-+", ZoomIn, context),
        KeyBinding::new("ctrl-_", ZoomOut, context),
        KeyBinding::new("ctrl-)", ZoomReset, context),
    ]);
    bindings
}

/// Font size used at startup and restored by "reset zoom".
pub const DEFAULT_FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT_FACTOR: f32 = 1.3;

#[cfg(target_os = "linux")]
const FONT_FAMILY: &str = "Noto Sans Mono";
#[cfg(target_os = "macos")]
const FONT_FAMILY: &str = "Menlo";
#[cfg(target_os = "windows")]
const FONT_FAMILY: &str = "Consolas";

#[derive(Clone, Copy, Debug, PartialEq)]
struct CellMetrics {
    width: Pixels,
    height: Pixels,
}

/// One terminal pane: owns the PTY process and the terminal state fed by its output.
pub struct TerminalView {
    focus_handle: FocusHandle,
    term: TermState,
    pty: PtyProcess,
    font: Font,
    font_size: Pixels,
    grid_origin: Point<Pixels>,
    grid_size: (u16, u16),
    metrics: CellMetrics,
    scroll_accum: f32,
    selecting: bool,
    mouse_position: Option<Point<Pixels>>,
    hovered_link: Option<(u16, Range<usize>)>,
    exited: bool,
    _reader: Task<()>,
}

impl TerminalView {
    /// Wrap a spawned PTY process. `cols`/`rows` must match the size the process was spawned with.
    pub fn new(
        pty: PtyProcess,
        reader: Box<dyn Read + Send>,
        cols: u16,
        rows: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::unbounded::<Vec<u8>>();
        std::thread::spawn(move || read_loop(reader, tx));
        let reader_task = cx.spawn(async move |this, cx| {
            while let Ok(mut bytes) = rx.recv().await {
                while let Ok(more) = rx.try_recv() {
                    bytes.extend_from_slice(&more);
                }
                if this.update(cx, |view, cx| view.on_output(&bytes, cx)).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |view, cx| {
                view.exited = true;
                cx.notify();
            });
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        TerminalView {
            focus_handle,
            term: TermState::new(cols, rows),
            pty,
            font: font(FONT_FAMILY),
            font_size: px(DEFAULT_FONT_SIZE),
            grid_origin: Point::default(),
            grid_size: (cols, rows),
            metrics: CellMetrics { width: px(8.), height: px(18.) },
            scroll_accum: 0.0,
            selecting: false,
            mouse_position: None,
            hovered_link: None,
            exited: false,
            _reader: reader_task,
        }
    }

    fn on_output(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.term.feed(bytes);
        let replies = self.term.take_pty_writes();
        if !replies.is_empty() {
            self.write_pty(&replies);
        }
        cx.notify();
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if let Err(err) = self.pty.write(bytes) {
            tracing::warn!("writing to pty failed: {err}");
        }
    }

    fn cell_metrics(&self, window: &Window) -> CellMetrics {
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&self.font);
        let width = text_system
            .advance(font_id, self.font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(self.font_size * 0.6);
        CellMetrics { width, height: (self.font_size * LINE_HEIGHT_FACTOR).round() }
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, metrics: CellMetrics, cx: &mut Context<Self>) {
        self.grid_origin = bounds.origin;
        self.metrics = metrics;
        let dims = grid_dims(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            f32::from(metrics.width),
            f32::from(metrics.height),
        );
        if dims != self.grid_size {
            self.grid_size = dims;
            self.term.resize(dims.0, dims.1);
            if let Err(err) = self.pty.resize(dims.0, dims.1) {
                tracing::warn!("pty resize failed: {err:#}");
            }
            cx.notify();
        }
    }

    fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.term.scroll_to_bottom();
        self.term.clear_selection();
        self.write_pty(bytes);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.exited {
            cx.quit();
            return;
        }
        let keystroke = &event.keystroke;
        if event.prefer_character_input && keystroke.key_char.is_some() {
            return;
        }
        let input = KeyInput {
            key: keystroke.key.clone(),
            key_char: keystroke.key_char.clone(),
            ctrl: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            shift: keystroke.modifiers.shift,
            platform: keystroke.modifiers.platform,
        };
        if let Some(bytes) = encode_key(&input, self.term.key_modes()) {
            tracing::debug!(key = %keystroke.key, kitty = self.term.key_modes().kitty_disambiguate, "encoded key");
            self.send_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.term.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let bytes = encode_paste(&text, self.term.bracketed_paste());
            self.send_input(&bytes, cx);
        }
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.font_size = px(size.clamp(8.0, 40.0));
        cx.notify();
    }

    fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) + 1.0, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) - 1.0, cx);
    }

    fn zoom_reset(&mut self, _: &ZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(DEFAULT_FONT_SIZE, cx);
    }

    /// Viewport cell under a window position, plus whether it is in the cell's right half.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16, bool)> {
        let x = f32::from(position.x - self.grid_origin.x);
        let y = f32::from(position.y - self.grid_origin.y);
        let (w, h) = (f32::from(self.metrics.width), f32::from(self.metrics.height));
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (col, row) = ((x / w).floor() as u16, (y / h).floor() as u16);
        if col >= self.grid_size.0 || row >= self.grid_size.1 {
            return None;
        }
        Some((col, row, x - col as f32 * w > w / 2.0))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        let Some((col, row, right_half)) = self.cell_at(event.position) else { return };
        if link_modifier(event.modifiers)
            && let Some(link) = link_at(&self.term.row_text(row), col as usize)
        {
            cx.open_url(&link.url);
            return;
        }
        self.term.start_selection(col, row, right_half);
        self.selecting = true;
        cx.notify();
    }

    /// Underline the link under the mouse while the link modifier (Ctrl, or Cmd on macOS) is held.
    fn update_hovered_link(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        let hovered = self
            .mouse_position
            .filter(|_| link_modifier(modifiers))
            .and_then(|position| self.cell_at(position))
            .and_then(|(col, row, _)| {
                link_at(&self.term.row_text(row), col as usize).map(|link| (row, link.cols))
            });
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            cx.notify();
        }
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.update_hovered_link(event.modifiers, cx);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.mouse_position = Some(event.position);
        self.update_hovered_link(event.modifiers, cx);
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some((col, row, right_half)) = self.cell_at(event.position) {
            self.term.update_selection(col, row, right_half);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.term.selection_text().is_none() {
            self.term.clear_selection();
        }
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.metrics.height);
        self.scroll_accum += f32::from(delta.y) / f32::from(self.metrics.height);
        let lines = self.scroll_accum.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_accum -= lines as f32;
        let (col, row) = self.cell_at(event.position).map_or((0, 0), |(col, row, _)| (col, row));
        match scroll_action(self.term.scroll_modes(), lines, col, row) {
            ScrollAction::Viewport(n) => self.term.scroll_viewport(n),
            ScrollAction::Send(bytes) => self.write_pty(&bytes),
        }
        cx.notify();
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.exited {
            cx.quit();
            return;
        }
        self.send_input(text.as_bytes(), cx);
    }

    // IME composition (pre-edit) is not displayed in M1; only committed text is sent.
    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (col, row) = self.term.cursor_cell()?;
        Some(Bounds::new(
            point(
                element_bounds.origin.x + self.metrics.width * col as f32,
                element_bounds.origin.y + self.metrics.height * row as f32,
            ),
            size(self.metrics.width, self.metrics.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self.cell_metrics(window);
        let mut snapshot = self.term.snapshot();
        if let Some((row, cols)) = &self.hovered_link
            && let Some(cells) = snapshot.rows.get_mut(*row as usize)
        {
            for cell in cells.iter_mut().take(cols.end).skip(cols.start) {
                cell.style.underline = true;
            }
        }
        let hovering_link = self.hovered_link.is_some();
        let focused = self.focus_handle.is_focused(window);
        let font = self.font.clone();
        let font_size = self.font_size;
        let background = to_hsla(snapshot.background);
        let banner_color = to_hsla(snapshot.cursor_color);
        let layout_view = cx.entity();
        let input_view = cx.entity();
        let focus = self.focus_handle.clone();
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .when(hovering_link, |this| this.cursor_pointer())
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        layout_view.update(cx, |view, cx| view.layout(bounds, metrics, cx));
                    },
                    move |bounds, (), window, cx| {
                        paint_grid(&snapshot, bounds.origin, metrics, &font, font_size, focused, window, cx);
                        window.handle_input(&focus, ElementInputHandler::new(bounds, input_view), cx);
                    },
                )
                .flex_1()
                .w_full(),
            )
            .when(self.exited, |this| {
                this.child(
                    div().px_2().py_1().text_color(banner_color).child("Process exited — press any key to close"),
                )
            })
    }
}

fn link_modifier(modifiers: Modifiers) -> bool {
    modifiers.control || modifiers.platform
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: async_channel::Sender<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send_blocking(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    snapshot: &ScreenSnapshot,
    origin: Point<Pixels>,
    m: CellMetrics,
    base_font: &Font,
    font_size: Pixels,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    for (row_index, row) in snapshot.rows.iter().enumerate() {
        let y = origin.y + m.height * row_index as f32;
        for span in row_spans(row) {
            let x = origin.x + m.width * span.start_col as f32;
            if span.style.bg != snapshot.background {
                let bounds = Bounds::new(point(x, y), size(m.width * span.width_cols as f32, m.height));
                window.paint_quad(fill(bounds, to_hsla(span.style.bg)));
            }
            if span.text.trim().is_empty() {
                continue;
            }
            let force_width = (!span.wide).then_some(m.width);
            paint_text(&span.text, &span.style, point(x, y), force_width, m, base_font, font_size, window, cx);
        }
    }
    if let Some(cursor) = &snapshot.cursor {
        let cells = if cursor.wide { 2.0 } else { 1.0 };
        let bounds = Bounds::new(
            point(origin.x + m.width * cursor.col as f32, origin.y + m.height * cursor.row as f32),
            size(m.width * cells, m.height),
        );
        let color = to_hsla(snapshot.cursor_color);
        if focused {
            window.paint_quad(fill(bounds, color));
            if cursor.ch != ' ' {
                let style = CellStyle { fg: snapshot.background, ..cursor.style };
                let force_width = (!cursor.wide).then_some(m.width);
                paint_text(&cursor.ch.to_string(), &style, bounds.origin, force_width, m, base_font, font_size, window, cx);
            }
        } else {
            window.paint_quad(outline(bounds, color, BorderStyle::Solid));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: &str,
    style: &CellStyle,
    origin: Point<Pixels>,
    force_width: Option<Pixels>,
    m: CellMetrics,
    base_font: &Font,
    font_size: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let color = to_hsla(style.fg);
    let mut run_font = base_font.clone();
    if style.bold {
        run_font.weight = FontWeight::BOLD;
    }
    if style.italic {
        run_font.style = FontStyle::Italic;
    }
    let run = TextRun {
        len: text.len(),
        font: run_font,
        color,
        background_color: None,
        underline: style.underline.then_some(UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false }),
        strikethrough: style.strikethrough.then_some(StrikethroughStyle { thickness: px(1.), color: Some(color) }),
    };
    let line = window.text_system().shape_line(text.to_string().into(), font_size, &[run], force_width);
    if let Err(err) = line.paint(origin, m.height, TextAlign::Left, None, window, cx) {
        tracing::warn!("painting text failed: {err:#}");
    }
}

fn to_hsla(color: Rgb) -> Hsla {
    rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32).into()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn action_for(keystroke: Keystroke) -> Option<&'static str> {
        key_bindings()
            .into_iter()
            .find(|binding| binding.match_keystrokes(std::slice::from_ref(&keystroke)) == Some(false))
            .map(|binding| binding.action().name())
    }

    /// Shape of the keystrokes GPUI's Linux backend emits: Shift is dropped for symbols and digits
    /// and the shifted character is reported instead (Ctrl+Shift+= arrives as `ctrl-+`).
    fn keystroke(key: &str, shift: bool, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers: Modifiers { control: true, shift, ..Default::default() },
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn zoom_shortcuts_match_linux_shifted_symbols() {
        assert_eq!(action_for(keystroke("+", false, Some("+"))), Some("terminal::ZoomIn"));
        assert_eq!(action_for(keystroke("_", false, Some("_"))), Some("terminal::ZoomOut"));
        assert_eq!(action_for(keystroke(")", false, Some(")"))), Some("terminal::ZoomReset"));
    }

    #[test]
    fn clipboard_shortcuts_keep_shift_for_letters() {
        assert_eq!(action_for(keystroke("c", true, None)), Some("terminal::Copy"));
        assert_eq!(action_for(keystroke("v", true, None)), Some("terminal::Paste"));
    }
}
