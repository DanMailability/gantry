//! GPUI view that renders one terminal pane and exchanges bytes with its PTY.

use std::io::Read;

use app_core::layout::grid_dims;
use app_core::snapshot::{CellStyle, Rgb, ScreenSnapshot, row_spans};
use app_core::term_state::TermState;
use daemon::pty::PtyProcess;
use gpui::{
    App, BorderStyle, Bounds, Context, FocusHandle, Font, FontStyle, FontWeight, Hsla, Pixels,
    Point, StrikethroughStyle, Task, TextAlign, TextRun, UnderlineStyle, Window, canvas, div,
    fill, font, outline, point, prelude::*, px, rgb, size,
};

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
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self.cell_metrics(window);
        let snapshot = self.term.snapshot();
        let focused = self.focus_handle.is_focused(window);
        let font = self.font.clone();
        let font_size = self.font_size;
        let background = to_hsla(snapshot.background);
        let banner_color = to_hsla(snapshot.cursor_color);
        let layout_view = cx.entity();
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
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
