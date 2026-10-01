//! GPUI view that renders one terminal pane and exchanges bytes with its PTY.

use std::ops::Range;

use app_core::bidi::{RowOrder, logical_hit, to_display};
use app_core::input::{KeyInput, encode_key};
use app_core::layout::grid_dims;
use app_core::paste::encode_paste;
use app_core::scroll::{ScrollAction, scroll_action};
use app_core::url::link_at;
use app_core::snapshot::{CellStyle, Rgb, ScreenSnapshot, row_spans};
use app_core::term_state::TermState;
use app_core::workspace::PaneLaunch;
use daemon::client::{ClientEvent, DaemonClient};
use protocol::message::PaneId;
use gpui::{
    App, BorderStyle, Bounds, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Font, FontStyle,
    FontWeight, Hsla, Pixels, Point, StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window, canvas,
    div, fill, font, outline, point, prelude::*, px, rgb, size,
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

/// Tried before cosmic-text's own fallback list, which misses some installed symbol fonts (it asks for "Noto Sans
/// Symbols2"; `⏺` then lands on a COLRv1 emoji font that cannot be drawn). GPUI drops — and unloads — any fallback
/// font without an `m` glyph, so pure symbol or Hebrew fonts must not be listed here.
#[cfg(target_os = "linux")]
const FALLBACK_FAMILIES: &[&str] = &["Adwaita Mono", "Symbola"];

/// The pane font, with its fallback chain on Linux.
fn pane_font() -> Font {
    #[allow(unused_mut)]
    let mut pane_font = font(FONT_FAMILY);
    #[cfg(target_os = "linux")]
    {
        pane_font.fallbacks =
            Some(gpui::FontFallbacks::from_fonts(FALLBACK_FAMILIES.iter().map(|family| family.to_string()).collect()));
    }
    pane_font
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CellMetrics {
    width: Pixels,
    height: Pixels,
}

/// What the pane is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneState {
    /// Waiting for the daemon to start the process.
    Starting,
    /// Showing a live process.
    Live,
    /// The process ended (`None`: it ended while ccm was closed).
    Exited(Option<u32>),
    /// The process could not be started.
    Failed(String),
    /// The daemon connection is gone.
    Disconnected,
}

/// Something the workspace should react to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// The user clicked into the pane.
    Focused,
    /// The user asked to run the pane again (Continue / Restart).
    Rerun,
    /// The user asked to close the pane.
    Close,
    /// The program changed the title.
    TitleChanged,
}

/// One terminal pane, showing a daemon-owned session.
pub struct TerminalView {
    focus_handle: FocusHandle,
    term: TermState,
    client: DaemonClient,
    pane: Option<PaneId>,
    launch: PaneLaunch,
    state: PaneState,
    title: Option<String>,
    cell: (u16, u16),
    font: Font,
    font_size: Pixels,
    grid_origin: Point<Pixels>,
    grid_size: (u16, u16),
    metrics: CellMetrics,
    scroll_accum: f32,
    selecting: bool,
    mouse_position: Option<Point<Pixels>>,
    hovered_link: Option<(u16, Range<usize>)>,
    /// Display order of each viewport row in the last frame (`None`: drawn as stored).
    row_orders: Vec<Option<RowOrder>>,
}

impl EventEmitter<PaneEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    /// A pane that will show `launch` once `attach` is called.
    pub fn new(client: DaemonClient, launch: PaneLaunch, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        TerminalView {
            focus_handle: cx.focus_handle(),
            term: TermState::new(80, 24),
            client,
            pane: None,
            launch,
            state: PaneState::Starting,
            title: None,
            cell: (0, 0),
            font: pane_font(),
            font_size: px(DEFAULT_FONT_SIZE),
            grid_origin: Point::default(),
            grid_size: (80, 24),
            metrics: CellMetrics { width: px(8.), height: px(18.) },
            scroll_accum: 0.0,
            selecting: false,
            mouse_position: None,
            hovered_link: None,
            row_orders: Vec::new(),
        }
    }

    /// Show daemon pane `pane`, running `launch` (a fresh terminal; its output or replay follows).
    pub fn attach(&mut self, pane: PaneId, launch: PaneLaunch, cx: &mut Context<Self>) {
        self.pane = Some(pane);
        self.launch = launch;
        self.state = PaneState::Live;
        self.term = TermState::new(self.grid_size.0, self.grid_size.1);
        self.title = None;
        if let Some((cols, rows, width, height)) = resize_on_attach(self.grid_size, self.cell) {
            self.client.resize(pane, cols, rows, width, height);
        }
        cx.notify();
    }

    /// Output or resync for this pane.
    pub fn on_event(&mut self, event: &ClientEvent, cx: &mut Context<Self>) {
        match event {
            ClientEvent::Output { bytes, .. } => {
                self.term.feed(bytes);
                // The daemon answers terminal queries; replies from this copy would be duplicates.
                self.term.take_pty_writes();
                let title = self.term.title();
                if title != self.title {
                    self.title = title;
                    cx.emit(PaneEvent::TitleChanged);
                }
            }
            ClientEvent::Resync { cols, rows, .. } => {
                self.term = TermState::new(*cols, *rows);
                // The next layout pass resizes to the window if it differs.
                self.grid_size = (*cols, *rows);
            }
            _ => return,
        }
        cx.notify();
    }

    /// The process ended.
    pub fn exited(&mut self, code: Option<u32>, cx: &mut Context<Self>) {
        self.pane = None;
        self.state = PaneState::Exited(code);
        cx.notify();
    }

    /// The process could not be started.
    pub fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.state = PaneState::Failed(error);
        cx.notify();
    }

    /// The daemon connection is gone.
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.state = PaneState::Disconnected;
        cx.notify();
    }

    /// About to start `launch` in this pane again.
    pub fn restarting(&mut self, launch: PaneLaunch, cx: &mut Context<Self>) {
        self.launch = launch;
        self.state = PaneState::Starting;
        cx.notify();
    }

    /// Whether a live process is running here.
    pub fn is_running(&self) -> bool {
        self.state == PaneState::Live
    }

    /// Program title, else `program · dir`.
    pub fn title(&self) -> String {
        self.title.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| self.launch.label())
    }

    /// Current grid size (columns, rows).
    pub fn grid_size(&self) -> (u16, u16) {
        self.grid_size
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if let (Some(pane), PaneState::Live) = (self.pane, &self.state) {
            self.client.input(pane, bytes);
        }
    }

    /// Banner text and whether Continue/Restart + Close apply.
    fn banner(&self) -> Option<(String, bool)> {
        match &self.state {
            PaneState::Starting | PaneState::Live => None,
            PaneState::Exited(Some(code)) => Some((format!("Process exited (code {code})"), true)),
            PaneState::Exited(None) => Some(("Session ended while ccm was closed".into(), true)),
            PaneState::Failed(error) => Some((format!("Could not start: {error}"), true)),
            PaneState::Disconnected => Some(("Lost connection to the ccm daemon — restart ccm".into(), false)),
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
        let cell = (f32::from(metrics.width).round() as u16, f32::from(metrics.height).round() as u16);
        self.term.set_cell_size(cell.0, cell.1);
        self.cell = cell;
        let dims = grid_dims(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            f32::from(metrics.width),
            f32::from(metrics.height),
        );
        if dims != self.grid_size {
            self.grid_size = dims;
            self.term.resize(dims.0, dims.1);
            if let Some(pane) = self.pane {
                self.client.resize(pane, dims.0, dims.1, cell.0, cell.1);
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
        if !self.is_running() {
            if event.keystroke.key == "enter" && matches!(self.state, PaneState::Exited(_) | PaneState::Failed(_)) {
                cx.emit(PaneEvent::Rerun);
            }
            cx.stop_propagation();
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

    /// The logical cell under a window position: `cell_at` mapped through the last frame's display order.
    fn logical_cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16, bool)> {
        let (col, row, right_half) = self.cell_at(position)?;
        let (col, right_half) = logical_hit(&self.row_orders, col as usize, row as usize, right_half);
        Some((col as u16, row, right_half))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        cx.emit(PaneEvent::Focused);
        let Some((col, row, right_half)) = self.logical_cell_at(event.position) else { return };
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
            .and_then(|position| self.logical_cell_at(position))
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
        if let Some((col, row, right_half)) = self.logical_cell_at(event.position) {
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
        let (col, row) = self.logical_cell_at(event.position).map_or((0, 0), |(col, row, _)| (col, row));
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
        if !self.is_running() {
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
        let col = self.row_orders.get(row).and_then(Option::as_ref).map_or(col, |order| order.visual(col));
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
        self.row_orders = to_display(&mut snapshot);
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
            .when_some(self.banner(), |this, (text, actions)| {
                let rerun = if self.launch.is_claude() { "Continue" } else { "Restart" };
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .items_center()
                        .text_color(banner_color)
                        .child(text)
                        .when(actions, |row| {
                            row.child(banner_button("rerun", rerun, cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Rerun))))
                                .child(banner_button("close", "Close", cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Close))))
                        }),
                )
            })
    }
}

/// The resize to send when attaching: the laid-out size, since a spawn request may have used a size
/// from before the first layout. Before any layout (`cell` unknown) the daemon's size stands.
fn resize_on_attach(grid: (u16, u16), cell: (u16, u16)) -> Option<(u16, u16, u16, u16)> {
    (cell != (0, 0)).then_some((grid.0, grid.1, cell.0, cell.1))
}

fn banner_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div().id(id).px_2().rounded_sm().border_1().border_color(rgb(0x6cb6ff)).cursor_pointer().child(label).on_click(on_click)
}

fn link_modifier(modifiers: Modifiers) -> bool {
    modifiers.control || modifiers.platform
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
            let single = span.width_cols == 1 && !span.text.is_ascii();
            let force_width = (!span.wide && !single).then_some(m.width);
            paint_text(&span.text, &span.style, point(x, y), force_width, single, m, base_font, font_size, window, cx);
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
                let single = !cursor.wide && !cursor.ch.is_ascii();
                let force_width = (!cursor.wide && !single).then_some(m.width);
                paint_text(&cursor.ch.to_string(), &style, bounds.origin, force_width, single, m, base_font, font_size, window, cx);
            }
        } else {
            window.paint_quad(outline(bounds, color, BorderStyle::Solid));
        }
    }
}

/// `center`: draw a glyph narrower than one cell in the middle of that cell.
#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: &str,
    style: &CellStyle,
    origin: Point<Pixels>,
    force_width: Option<Pixels>,
    center: bool,
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
    let inset = if center { ((m.width - line.width) / 2.).max(px(0.)) } else { px(0.) };
    if let Err(err) = line.paint(point(origin.x + inset, origin.y), m.height, TextAlign::Left, None, window, cx) {
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
    fn the_pane_font_falls_back_to_fonts_gpui_keeps() {
        let fallbacks = pane_font().fallbacks.expect("Linux panes have a fallback chain");
        assert_eq!(fallbacks.fallback_list(), ["Adwaita Mono", "Symbola"]);
    }

    #[test]
    fn attaching_before_the_first_layout_keeps_the_daemon_size() {
        assert_eq!(resize_on_attach((80, 24), (0, 0)), None);
        assert_eq!(resize_on_attach((120, 40), (8, 18)), Some((120, 40, 8, 18)));
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
