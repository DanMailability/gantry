//! The Settings tab's page: a left menu of sections (Appearance, Alerts) and the section's controls.
//!
//! Every control is focusable (Tab / Shift+Tab, Space or Enter to press); changes go through
//! [`SettingsStore::update`], which saves and applies them at once.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use app_core::session_tree::{Tone, display_path};
use app_core::settings::{AlertSettings, AlertTone, EventAlerts, Problem, Settings, StatusFills, ThemeChoice, hex, parse_hex};
use app_core::snapshot::Rgb;
use app_core::text_field::TextField;
use app_core::theme_colors::{Preset, Tokens, contrast, derive, readable_fill, terminal_palette, tokens as preset_tokens, tone_colors};
use gpui::{
    App, Bounds, Context, DispatchPhase, Div, ElementId, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle, SharedString, Stateful, Window, canvas, deferred, div, point, prelude::*, px,
    relative, rgb, rgba,
};

use crate::alerts;
use crate::field;
use crate::settings_store::SettingsStore;
use crate::terminal_view::FONT_FAMILY;
use crate::theme::{RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, theme};

/// A group of settings, chosen in the left menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// Theme, status colours and fonts.
    Appearance,
    /// What raises an alert, and how.
    Alerts,
}

impl Section {
    const ALL: [Section; 2] = [Section::Appearance, Section::Alerts];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Alerts => "Alerts",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Section::Appearance => "Changes apply right away.",
            Section::Alerts => "For sessions running in Gantry's panes.",
        }
    }
}

/// A session status whose sidebar fill can be chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    NeedsInput,
    Working,
    Done,
    Idle,
}

impl Status {
    const ALL: [Status; 4] = [Status::NeedsInput, Status::Working, Status::Done, Status::Idle];

    fn tone(self) -> Tone {
        match self {
            Status::NeedsInput => Tone::NeedsInput,
            Status::Working => Tone::Working,
            Status::Done => Tone::Done,
            Status::Idle => Tone::Idle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::NeedsInput => "Needs input",
            Status::Working => "Working",
            Status::Done => "Done",
            Status::Idle => "Idle",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Status::NeedsInput => "Claude is waiting for you",
            Status::Working => "Claude is busy",
            Status::Done => "Finished; you have not looked yet",
            Status::Idle => "Waiting quietly; a muted fill",
        }
    }

    fn sample(self) -> &'static str {
        match self {
            Status::NeedsInput => "OPS work log triage",
            Status::Working => "Tab drag to reorder",
            Status::Done => "Right-click menus",
            Status::Idle => "Plan mode bridge hook",
        }
    }

    fn fill(self, fills: &StatusFills) -> Option<u32> {
        match self {
            Status::NeedsInput => fills.needs_input,
            Status::Working => fills.working,
            Status::Done => fills.done,
            Status::Idle => fills.idle,
        }
    }

    fn set(self, fills: &mut StatusFills, fill: Option<u32>) {
        match self {
            Status::NeedsInput => fills.needs_input = fill,
            Status::Working => fills.working = fill,
            Status::Done => fills.done = fill,
            Status::Idle => fills.idle = fill,
        }
    }

    /// The fill the preset draws when nothing is chosen.
    fn default_fill(self, preset: Preset) -> u32 {
        tone_colors(preset, &StatusFills::default(), self.tone()).fill.unwrap_or(preset_tokens(preset).surface)
    }
}

/// An event that can alert, a row of the alerts grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Event {
    NeedsInput,
    Finished,
}

impl Event {
    const ALL: [Event; 2] = [Event::NeedsInput, Event::Finished];

    fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Event::NeedsInput => "Needs input",
            Event::Finished => "Finished",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Event::NeedsInput => "Claude asks for permission or a reply",
            Event::Finished => "Busy, then idle",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Event::NeedsInput => Tone::NeedsInput,
            Event::Finished => Tone::Done,
        }
    }

    fn alerts(self, alerts: &mut AlertSettings) -> &mut EventAlerts {
        match self {
            Event::NeedsInput => &mut alerts.needs_input,
            Event::Finished => &mut alerts.finished,
        }
    }
}

const TONES: [(AlertTone, &str); 2] = [(AlertTone::Bell, "Bell (low, ringing)"), (AlertTone::Tick, "Tick (short, high)")];

fn tone_label(tone: AlertTone) -> &'static str {
    TONES.iter().find(|(candidate, _)| *candidate == tone).map_or("", |(_, label)| label)
}

/// A drop-down or pop-over that is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Popover {
    Fill(Status),
    Font,
    Tone(Event),
}

/// A number setting edited with − / + buttons or the arrow keys.
struct Stepped {
    name: &'static str,
    get: fn(&Settings) -> f32,
    set: fn(&mut Settings, f32),
    delta: f32,
    min: f32,
    max: f32,
}

const TERMINAL_SIZE: Stepped = Stepped {
    name: "terminal-size",
    get: |s| s.terminal_font.size,
    set: |s, value| s.terminal_font.size = value,
    delta: 1.0,
    min: 8.0,
    max: 40.0,
};
const LINE_HEIGHT: Stepped = Stepped {
    name: "line-height",
    get: |s| s.terminal_font.line_height,
    set: |s, value| s.terminal_font.line_height = value,
    delta: 0.1,
    min: 1.0,
    max: 2.0,
};
const INTERFACE_SIZE: Stepped = Stepped {
    name: "interface-size",
    get: |s| s.ui_font_size,
    set: |s, value| s.ui_font_size = value,
    delta: 1.0,
    min: 11.0,
    max: 20.0,
};
const FINISHED_AFTER: Stepped = Stepped {
    name: "finished-after",
    get: |s| s.alerts.finished_after_seconds as f32,
    set: |s, value| s.alerts.finished_after_seconds = value as u32,
    delta: 1.0,
    min: 0.0,
    max: 600.0,
};

/// A drop-down list shows this many rows (each this tall, before scaling) before it scrolls.
const DROPDOWN_ROWS: f32 = 8.;
const DROPDOWN_ROW: f32 = 30.;

/// Widths (before scaling) of the alerts grid's fixed columns; the Event column takes the rest. The tone drop-down and Play button
/// follow the Sound switch, or wrap under the row when there is no room.
/// Width of the tone drop-down and Play button; the grid shows them on the row's first line from this much width on.
const TRAILING_WIDTH: f32 = 255.;
const GRID_WIDE: f32 = 36. + 120. + BADGE_COLUMN + NOTIFICATION_COLUMN + SOUND_COLUMN + 16. * 4. + TRAILING_WIDTH;
const BADGE_COLUMN: f32 = 64.;
const NOTIFICATION_COLUMN: f32 = 84.;
const SOUND_COLUMN: f32 = 64.;

const SUGGESTED_FILLS: [u32; 12] =
    [0xc1e5cd, 0xb7e4dd, 0xcfe8b4, 0xe6efb0, 0xa8dcc0, 0xd5ecd9, 0xf7c9a3, 0xf6b8b8, 0xe9c6ef, 0xc9cdf6, 0xb9d5f6, 0xf1e1a6];

const THEME_CARDS: [(ThemeChoice, &str, &str); 3] = [
    (ThemeChoice::Mist, "Mist", "Dark slate, light-blue accent"),
    (ThemeChoice::MistLight, "Mist light", "The same look on a light ground"),
    (ThemeChoice::System, "Follow system", "Dark or light from the desktop"),
];

/// `value` moved by `delta`, kept within `min..=max` and rounded to one decimal.
fn step(value: f32, delta: f32, min: f32, max: f32) -> f32 {
    ((value + delta).clamp(min, max) * 10.0).round() / 10.0
}

/// A number as the steppers show it: no decimals for whole numbers.
fn format_number(value: f32) -> String {
    if value.fract() == 0.0 { format!("{value:.0}") } else { format!("{value:.1}") }
}

/// The contrast line for a picked `fill`, and whether the fill may be used.
fn contrast_label(fill: u32, tokens: &Tokens) -> (String, bool) {
    if !readable_fill(fill, tokens) {
        return ("Too low contrast; choose a lighter or darker fill".into(), false);
    }
    let ratio = contrast(derive(fill, tokens).ink, fill);
    let text = if ratio < 10.0 { format!("{ratio:.1}") } else { format!("{ratio:.0}") };
    (format!("Text contrast {text} : 1"), true)
}

/// What to store for a picked `fill`: nothing when it is the theme's own.
fn fill_to_store(fill: u32, default: u32) -> Option<u32> {
    (fill != default).then_some(fill)
}

/// `#rrggbb`, with or without the `#`.
fn parse_fill(text: &str) -> Option<u32> {
    let text = text.trim();
    parse_hex(&if text.starts_with('#') { text.to_string() } else { format!("#{text}") })
}

/// The families whose `m` and `i` advance the same: `(family, advance of m, advance of i)` in, sorted names out.
fn monospace_families(measured: impl IntoIterator<Item = (String, f32, f32)>) -> Vec<String> {
    let mut names: Vec<String> =
        measured.into_iter().filter(|(_, m, i)| (m - i).abs() < 0.01).map(|(name, _, _)| name).collect();
    names.sort();
    names.dedup();
    names
}

fn installed_monospace(cx: &App) -> Vec<String> {
    let text_system = cx.text_system();
    let measured = text_system.all_font_names().into_iter().filter_map(|name| {
        let id = text_system.resolve_font(&gpui::font(name.clone()));
        let m = text_system.advance(id, px(14.), 'm').ok()?.width;
        let i = text_system.advance(id, px(14.), 'i').ok()?.width;
        Some((name, f32::from(m), f32::from(i)))
    });
    monospace_families(measured)
}

/// The volume (0 to 1, whole percents) under pointer position `x` on a slider spanning `left..left + width`.
fn volume_at(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 {
        return 0.0;
    }
    (((x - left) / width).clamp(0.0, 1.0) * 100.0).round() / 100.0
}

/// A problem as the banner lists it: `key: message`, or just the message for the whole file.
fn problem_line(problem: &Problem) -> String {
    if problem.key.is_empty() { problem.message.clone() } else { format!("{}: {}", problem.key, problem.message) }
}

fn color(value: Rgb) -> u32 {
    (u32::from(value.r) << 16) | (u32::from(value.g) << 8) | u32::from(value.b)
}

/// Create `path` as `{}` if it is missing, then open it with the system's editor.
fn open_settings_file(path: &Path, cx: &App) {
    if !path.exists() {
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            tracing::warn!("settings directory not created: {error}");
        }
        if let Err(error) = std::fs::write(path, "{}\n") {
            tracing::warn!("settings file not created: {error}");
            return;
        }
    }
    cx.open_with_system(path);
}

type DropdownBounds = Rc<RefCell<HashMap<(&'static str, usize), Bounds<Pixels>>>>;

type Handler = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

fn handler(f: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static) -> Handler {
    Rc::new(f)
}

/// A handler that changes the settings.
fn change(f: impl Fn(&mut Settings) + 'static) -> Handler {
    handler(move |_, _, cx| SettingsStore::update(cx, |settings| f(settings)))
}

/// The Settings page, shown in the pane area while the Settings tab is active.
pub(crate) struct SettingsView {
    section: Section,
    focus_handle: FocusHandle,
    /// One handle per control, by name and index, created on first draw.
    stops: HashMap<(&'static str, usize), FocusHandle>,
    /// Installed monospace families (without the platform default), found when the page opened.
    families: Vec<String>,
    popover: Option<Popover>,
    /// The colour picker's hex field.
    hex: TextField,
    hex_focus: FocusHandle,
    /// The volume being dragged, written when the button is released.
    volume_drag: Option<f32>,
    slider_bounds: Rc<Cell<Bounds<Pixels>>>,
    body_scroll: ScrollHandle,
    grid_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where each drop-down's button was last drawn, to open its list upward near the window's bottom.
    dropdown_bounds: DropdownBounds,
    /// The hex field's text counts as selected: the next typed character or Backspace replaces it all.
    hex_selected: bool,
}

impl SettingsView {
    /// A page on the Appearance section.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        SettingsView {
            section: Section::Appearance,
            focus_handle: cx.focus_handle(),
            stops: HashMap::new(),
            families: installed_monospace(cx),
            popover: None,
            hex: TextField::default(),
            hex_focus: cx.focus_handle(),
            volume_drag: None,
            slider_bounds: Rc::new(Cell::new(Bounds::default())),
            body_scroll: ScrollHandle::new(),
            grid_bounds: Rc::new(Cell::new(Bounds::default())),
            dropdown_bounds: Rc::new(RefCell::new(HashMap::new())),
            hex_selected: false,
        }
    }

    fn stop(&mut self, name: &'static str, index: usize, cx: &mut Context<Self>) -> FocusHandle {
        self.stops.entry((name, index)).or_insert_with(|| cx.focus_handle()).clone()
    }

    fn close_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn open_fill_picker(&mut self, status: Status, window: &mut Window, cx: &mut Context<Self>) {
        let theme = theme(cx);
        let chosen = status.fill(&cx.global::<SettingsStore>().settings().status_fills);
        let current = chosen.unwrap_or(status.default_fill(theme.preset));
        self.hex.clear();
        self.hex.insert(&hex(current));
        self.hex_selected = true;
        self.popover = Some(Popover::Fill(status));
        self.hex_focus.focus(window, cx);
        cx.notify();
    }

    fn apply_fill(&mut self, status: Status, fill: u32, cx: &mut Context<Self>) {
        let default = status.default_fill(theme(cx).preset);
        SettingsStore::update(cx, |settings| status.set(&mut settings.status_fills, fill_to_store(fill, default)));
    }

    fn hex_key_down(&mut self, status: Status, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" {
            let tokens = theme(cx).tokens;
            if let Some(fill) = parse_fill(self.hex.text()).filter(|fill| readable_fill(*fill, &tokens)) {
                self.apply_fill(status, fill, cx);
                self.close_popover(window, cx);
            }
        } else if keystroke.key == "a" && (keystroke.modifiers.control || keystroke.modifiers.platform) {
            self.hex_selected = true;
        } else {
            let replaces = matches!(keystroke.key.as_str(), "backspace" | "delete")
                || keystroke.key_char.as_ref().is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control));
            if self.hex_selected && replaces && !keystroke.modifiers.control && !keystroke.modifiers.platform {
                self.hex.clear();
            }
            if !field::edit(&mut self.hex, keystroke) {
                return;
            }
            self.hex_selected = false;
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" if self.popover.is_some() => self.close_popover(window, cx),
            "tab" if !keystroke.modifiers.control && !keystroke.modifiers.alt => {
                if keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// A focusable, clickable box: Space or Enter presses it like a click. The ring is `border` (none: invisible)
    /// and turns to the text colour while focused.
    fn pressable(
        &mut self,
        name: &'static str,
        index: usize,
        border: Option<u32>,
        window: &Window,
        cx: &mut Context<Self>,
        press: Handler,
    ) -> Stateful<Div> {
        let theme = theme(cx).clone();
        let handle = self.stop(name, index, cx);
        let ring = if handle.is_focused(window) { Some(theme.tokens.text) } else { border };
        let on_key = press.clone();
        div()
            .id(ElementId::from((name, index)))
            .track_focus(&handle)
            .tab_stop(true)
            .cursor_pointer()
            .border_1()
            .border_color(ring.map_or(rgba(0), rgb))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.stop(name, index, cx).focus(window, cx);
                press(this, window, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let modifiers = &event.keystroke.modifiers;
                if matches!(event.keystroke.key.as_str(), "enter" | "space") && !modifiers.control && !modifiers.alt && !modifiers.platform {
                    on_key(this, window, cx);
                    cx.stop_propagation();
                }
            }))
    }

    fn button(
        &mut self,
        name: &'static str,
        index: usize,
        label: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
        press: Handler,
    ) -> Stateful<Div> {
        let theme = theme(cx).clone();
        self.pressable(name, index, Some(theme.tokens.line), window, cx, press)
            .h(theme.ui(34.))
            .px(theme.ui(14.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .text_sm()
            .child(label)
    }

    fn switch(
        &mut self,
        name: &'static str,
        index: usize,
        on: bool,
        window: &Window,
        cx: &mut Context<Self>,
        toggle: Handler,
    ) -> Stateful<Div> {
        let theme = theme(cx).clone();
        let track = if on { theme.tokens.accent } else { theme.tokens.line };
        let knob = if on { theme.tokens.on_accent } else { theme.tokens.muted };
        self.pressable(name, index, None, window, cx, toggle)
            .flex_none()
            .w(theme.ui(40.))
            .h(theme.ui(24.))
            .p(px(2.))
            .flex()
            .items_center()
            .when(on, |el| el.justify_end())
            .rounded_full()
            .bg(rgb(track))
            .child(div().size(theme.ui(18.)).rounded_full().bg(rgb(knob)))
    }

    fn stepper(&mut self, stepped: &'static Stepped, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = theme(cx).clone();
        let value = (stepped.get)(settings);
        let by = |sign: f32| {
            change(move |settings| (stepped.set)(settings, step((stepped.get)(settings), sign * stepped.delta, stepped.min, stepped.max)))
        };
        let (less, more) = (by(-1.0), by(1.0));
        let (key_less, key_more) = (less.clone(), more.clone());
        let square = |this: &mut Self, index: usize, label: &'static str, press: Handler, cx: &mut Context<Self>| {
            this.pressable(stepped.name, index, Some(theme.tokens.line), window, cx, press)
                .size(theme.ui(34.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(theme.tokens.raised))
                .child(label)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme.ui(6.))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let handler = match event.keystroke.key.as_str() {
                    "left" | "down" => &key_less,
                    "right" | "up" => &key_more,
                    _ => return,
                };
                handler(this, window, cx);
                cx.stop_propagation();
            }))
            .child(square(self, 0, "−", less, cx))
            .child(div().w(theme.ui(56.)).flex().justify_center().font_weight(FontWeight::SEMIBOLD).child(format_number(value)))
            .child(square(self, 1, "+", more, cx))
    }

    /// A drop-down: a button showing `label`, and while open the `options` under it.
    #[allow(clippy::too_many_arguments)]
    fn dropdown(
        &mut self,
        name: &'static str,
        index: usize,
        popover: Popover,
        label: String,
        width: Pixels,
        options: Vec<(String, bool, Handler)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = theme(cx).clone();
        let open = handler(move |this, _, cx| {
            this.popover = Some(popover);
            cx.notify();
        });
        let button = self
            .pressable(name, index, Some(if self.popover == Some(popover) { theme.tokens.accent } else { theme.tokens.line }), window, cx, open)
            .h(theme.ui(34.))
            .w(width)
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .text_sm()
            .child(div().min_w_0().truncate().child(label))
            .child(div().flex_none().text_color(rgb(theme.tokens.muted)).child("▾"));
        let menu = (self.popover == Some(popover)).then(|| {
            let max_height = theme.ui(DROPDOWN_ROWS * DROPDOWN_ROW);
            let needed = theme.ui(options.len() as f32 * DROPDOWN_ROW + 8.).min(max_height);
            let button_bounds = self.dropdown_bounds.borrow().get(&(name, index)).copied().unwrap_or_default();
            let below = f32::from(window.viewport_size().height - button_bounds.bottom());
            let upward = below < f32::from(needed) + 8. && f32::from(button_bounds.top()) > below;
            let items: Vec<_> = options
                .into_iter()
                .enumerate()
                .map(|(position, (text, selected, pick))| {
                    let pick = handler(move |this, window, cx| {
                        this.popover = None;
                        this.focus_handle.focus(window, cx);
                        pick(this, window, cx);
                    });
                    self.pressable(name, 100 + index * 1000 + position, None, window, cx, pick)
                        .px_3()
                        .py_1()
                        .rounded(px(RADIUS_SMALL))
                        .whitespace_nowrap()
                        .hover(|style| style.bg(rgb(theme.tokens.line)))
                        .when(selected, |item| item.text_color(rgb(theme.tokens.accent)).font_weight(FontWeight::SEMIBOLD))
                        .child(text)
                })
                .collect();
            deferred(
                div()
                    .id(ElementId::from((name, 99 + index * 1000)))
                    .occlude()
                    .absolute()
                    .when(upward, |menu| menu.bottom(theme.ui(38.)))
                    .when(!upward, |menu| menu.top(theme.ui(38.)))
                    .left_0()
                    .w(width)
                    .max_h(max_height)
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .bg(rgb(theme.tokens.raised))
                    .border_1()
                    .border_color(rgb(theme.tokens.line))
                    .rounded(px(RADIUS))
                    .shadow_lg()
                    .children(items),
            )
            .with_priority(1)
        });
        let bounds = self.dropdown_bounds.clone();
        let measure = canvas(
            move |area, _, _| {
                bounds.borrow_mut().insert((name, index), area);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div().relative().child(button).child(measure).children(menu)
    }

    fn open_settings_button(&mut self, index: usize, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = handler(|_, _, cx| {
            let path = cx.global::<SettingsStore>().path().to_path_buf();
            open_settings_file(&path, cx);
        });
        self.button("open-settings-json", index, "Open settings.json", window, cx, open).flex_none()
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// One line of the alerts grid, used by the header and by each row so their columns cannot differ:
/// a flexible Event cell, then the fixed Badge, Notification and Sound columns (contents centred), then the trailing group if any.
fn grid_line(
    theme: &Theme,
    event: impl IntoElement,
    badge: impl IntoElement,
    notification: impl IntoElement,
    sound: impl IntoElement,
    trailing: Option<Div>,
) -> Div {
    let column = |width: f32, content: Div| div().flex_none().w(theme.ui(width)).flex().justify_center().child(content);
    div()
        .flex()
        .items_center()
        .gap(theme.ui(16.))
        .px(theme.ui(18.))
        .child(div().flex_1().min_w_0().child(event))
        .child(column(BADGE_COLUMN, div().child(badge)))
        .child(column(NOTIFICATION_COLUMN, div().child(notification)))
        .child(column(SOUND_COLUMN, div().child(sound)))
        .children(trailing)
}

fn heading(theme: &Theme, text: &'static str) -> Div {
    div().text_size(theme.ui(15.)).font_weight(FontWeight::SEMIBOLD).child(text)
}

fn muted(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(theme.tokens.muted)).child(text.into())
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let store = cx.global::<SettingsStore>();
        let settings = store.settings().clone();
        let problems = store.problems().to_vec();
        let path = display_path(store.path(), std::env::home_dir().as_deref());
        let section = self.section;

        let menu = div().flex_none().w(theme.ui(200.)).py(theme.ui(24.)).px(theme.ui(12.)).flex().flex_col().gap(theme.ui(4.)).border_r_1().border_color(rgb(theme.tokens.line)).children(
            Section::ALL.map(|item| {
                let active = item == section;
                let choose = handler(move |this, _, cx| {
                    this.section = item;
                    this.popover = None;
                    this.body_scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                });
                self.pressable("section", item as usize, None, window, cx, choose)
                    .h(theme.ui(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(RADIUS_SMALL))
                    .text_color(rgb(if active { theme.tokens.text } else { theme.tokens.muted }))
                    .when(active, |el| el.bg(rgb(theme.tokens.raised)).font_weight(FontWeight::SEMIBOLD))
                    .when(!active, |el| el.hover(|style| style.bg(rgb(theme.tokens.raised))))
                    .child(item.label())
            }),
        );

        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(theme.ui(16.))
            .child(
                div()
                    .flex_none()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(section.label()))
                    .child(div().mt(theme.ui(4.)).text_sm().text_color(rgb(theme.tokens.muted)).child(section.subtitle())),
            )
            .child(muted(&theme, path).flex_1().min_w_0().truncate().text_right())
            .child(self.open_settings_button(0, window, cx));

        let banner = (!problems.is_empty()).then(|| {
            let colors = theme.tone(Tone::NeedsInput);
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(theme.ui(16.))
                .p(theme.ui(12.))
                .rounded(px(RADIUS))
                .bg(rgb(colors.fill.unwrap_or(theme.tokens.raised)))
                .text_color(rgb(colors.ink))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("The settings file has problems"))
                        .children(problems.iter().map(problem_line).map(|line| div().child(line))),
                )
                .child(self.open_settings_button(1, window, cx))
        });

        let body = match section {
            Section::Appearance => self.appearance(&theme, &settings, window, cx),
            Section::Alerts => self.alerts(&theme, &settings, window, cx),
        };

        let backdrop = self.popover.is_some().then(|| {
            deferred(div().absolute().top_0().left_0().size_full().occlude().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_popover(window, cx)),
            ))
        });

        div()
            .id("settings-page")
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .overflow_hidden()
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS_LARGE))
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(menu)
            .child(
                div()
                    .id("settings-body")
                    .track_scroll(&self.body_scroll)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .px(theme.ui(40.))
                    .py(theme.ui(28.))
                    .flex()
                    .flex_col()
                    .gap(theme.ui(26.))
                    .child(header)
                    .children(banner)
                    .child(body),
            )
            .children(backdrop)
    }
}

impl SettingsView {
    fn appearance(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(26.))
            .child(self.theme_cards(theme, settings, window, cx))
            .child(self.status_colours(theme, settings, window, cx))
            .child(self.fonts(theme, settings, window, cx))
    }

    fn theme_cards(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let cards = THEME_CARDS.iter().enumerate().map(|(index, (choice, name, hint))| {
            let choice = *choice;
            let selected = settings.theme == choice;
            let preview = preset_tokens(if choice == ThemeChoice::MistLight { Preset::MistLight } else { Preset::Mist });
            let preset = if choice == ThemeChoice::MistLight { Preset::MistLight } else { Preset::Mist };
            let bar = |status: Status| div().h(theme.ui(8.)).rounded(px(4.)).bg(rgb(status.default_fill(preset)));
            let light = preset_tokens(Preset::MistLight);
            let miniature = div()
                .h(theme.ui(74.))
                .flex()
                .gap(theme.ui(6.))
                .p(theme.ui(8.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(preview.ground))
                .border_1()
                .border_color(rgb(preview.line))
                .child(
                    div()
                        .w(theme.ui(48.))
                        .flex()
                        .flex_col()
                        .gap(theme.ui(4.))
                        .p(theme.ui(5.))
                        .rounded(px(6.))
                        .bg(rgb(preview.surface))
                        .child(bar(Status::NeedsInput))
                        .child(bar(Status::Working))
                        .child(bar(Status::Done)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_end()
                        .p(theme.ui(5.))
                        .rounded(px(6.))
                        .bg(rgb(preview.terminal))
                        .child(div().w(theme.ui(18.)).h(theme.ui(6.)).rounded(px(3.)).bg(rgb(preview.accent))),
                )
                .when(choice == ThemeChoice::System, |mini| {
                    mini.child(div().flex_1().rounded(px(6.)).bg(rgb(light.terminal)).border_1().border_color(rgb(light.line)))
                });
            let radio = if selected { theme.tokens.accent } else { theme.tokens.muted };
            self.pressable("theme", index, None, window, cx, change(move |settings| settings.theme = choice))
                .w(theme.ui(210.))
                .p(theme.ui(10.))
                .flex()
                .flex_col()
                .gap(theme.ui(10.))
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_2()
                .when(selected, |card| card.border_color(rgb(theme.tokens.accent)))
                .child(miniature)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(8.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(
                            div()
                                .size(theme.ui(14.))
                                .rounded_full()
                                .border_2()
                                .border_color(rgb(radio))
                                .when(selected, |dot| dot.bg(rgb(theme.tokens.accent))),
                        )
                        .child(*name),
                )
                .child(div().mt(theme.ui(-6.)).child(muted(theme, *hint)))
        });
        div().flex().flex_col().gap(theme.ui(12.)).child(heading(theme, "Theme")).child(div().flex().flex_wrap().gap(theme.ui(14.)).children(cards))
    }

    fn status_colours(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let rows = Status::ALL.map(|status| {
            let index = status as usize;
            let chosen = status.fill(&settings.status_fills);
            let colors = theme.tone(status.tone());
            let shown = chosen.unwrap_or_else(|| status.default_fill(theme.preset));
            let open = self.popover == Some(Popover::Fill(status));
            let sample = div()
                .w(theme.ui(300.))
                .max_w_full()
                .flex_none()
                .h(theme.ui(34.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .bg(rgb(colors.fill.unwrap_or(theme.tokens.surface)))
                .text_color(rgb(colors.ink))
                .child(div().flex_none().size(theme.ui(8.)).rounded_full().bg(rgb(colors.dot)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(status == Status::NeedsInput, |el| el.font_weight(FontWeight::SEMIBOLD))
                        .child(status.sample()),
                )
                .child(div().flex_none().text_xs().text_color(rgba((colors.ink << 8) | 0xcc)).child(status.label()));
            let open_picker = handler(move |this, window, cx| this.open_fill_picker(status, window, cx));
            let picker_button = self
                .pressable("status-colour", index, Some(if open { theme.tokens.accent } else { theme.tokens.line }), window, cx, open_picker)
                .h(theme.ui(34.))
                .pl(theme.ui(6.))
                .pr(theme.ui(12.))
                .flex()
                .items_center()
                .gap(theme.ui(8.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(theme.tokens.raised))
                .child(div().size(theme.ui(22.)).rounded(px(6.)).bg(rgb(shown)).border_1().border_color(rgb(theme.tokens.line)))
                .child(hex(shown));
            let picker = open.then(|| self.fill_picker(status, theme, window, cx));
            let reset = self
                .pressable("status-reset", index, None, window, cx, change(move |settings| status.set(&mut settings.status_fills, None)))
                .h(theme.ui(34.))
                .px(theme.ui(10.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SMALL))
                .text_color(rgb(if chosen.is_some() { theme.tokens.text } else { theme.tokens.muted }))
                .child("Reset");
            div().flex().flex_wrap().items_center().gap_x(theme.ui(18.)).gap_y(theme.ui(6.)).py(theme.ui(6.)).child(sample).child(
                div()
                    .flex_1()
                    .min_w(theme.ui(300.))
                    .flex()
                    .items_center()
                    .gap(theme.ui(18.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(theme.ui(90.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(status.label()))
                            .child(muted(theme, status.description())),
                    )
                    .child(div().relative().flex_none().child(picker_button).children(picker))
                    .child(reset.flex_none()),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(theme.ui(12.))
                    .child(heading(theme, "Status colours"))
                    .child(div().text_color(rgb(theme.tokens.muted)).child("Pick a fill; text and dot colours follow it.")),
            )
            .children(rows)
    }

    fn fill_picker(&mut self, status: Status, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chosen = status.fill(&cx.global::<SettingsStore>().settings().status_fills);
        let current = chosen.unwrap_or_else(|| status.default_fill(theme.preset));
        let typed = parse_fill(self.hex.text());
        let verdict = typed.map(|fill| contrast_label(fill, &theme.tokens));
        let swatches = SUGGESTED_FILLS.iter().enumerate().map(|(index, fill)| {
            let fill = *fill;
            let pick = handler(move |this, _, cx| {
                this.apply_fill(status, fill, cx);
                this.hex.clear();
                this.hex.insert(&hex(fill));
                this.hex_selected = true;
                cx.notify();
            });
            self.pressable("swatch", index, None, window, cx, pick)
                .h(theme.ui(30.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(fill))
                .border_2()
                .when(fill == current, |swatch| swatch.border_color(rgb(theme.tokens.text)))
        }).collect::<Vec<_>>();
        let tone = theme.tone(if verdict.as_ref().is_some_and(|(_, ok)| *ok) { Tone::Done } else { Tone::NeedsInput });
        let hex_focused = self.hex_focus.is_focused(window);
        let field_box = div()
            .id("hex-field")
            .track_focus(&self.hex_focus)
            .flex_1()
            .h(theme.ui(32.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(if hex_focused { theme.tokens.accent } else { theme.tokens.line }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| this.hex_key_down(status, event, window, cx)))
            .on_click(cx.listener(|this, _, window, cx| this.hex_focus.focus(window, cx)))
            .child(field::render(&self.hex, "#rrggbb", hex_focused, theme).when(self.hex_selected, |text| {
                text.bg(rgba((theme.tokens.accent << 8) | 0x55))
            }));
        deferred(
            div()
                .occlude()
                .absolute()
                .top(theme.ui(40.))
                .right_0()
                .w(theme.ui(272.))
                .p(theme.ui(14.))
                .flex()
                .flex_col()
                .gap(theme.ui(12.))
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .shadow_lg()
                .child(div().grid().grid_cols(6).gap(theme.ui(8.)).children(swatches))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(10.))
                        .text_color(rgb(theme.tokens.muted))
                        .child("Hex")
                        .child(field_box),
                )
                .child(match verdict {
                    Some((text, _)) => div()
                        .flex()
                        .items_start()
                        .gap(theme.ui(8.))
                        .text_xs()
                        .text_color(rgb(theme.tokens.muted))
                        .child(div().flex_none().mt(theme.ui(4.)).size(theme.ui(8.)).rounded_full().bg(rgb(tone.dot)))
                        .child(div().flex_1().min_w_0().child(text)),
                    None => muted(theme, "Type a colour as #rrggbb"),
                }),
        )
        .with_priority(1)
    }

    fn fonts(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let font = &settings.terminal_font;
        let label = match &font.family {
            Some(family) => family.clone(),
            None => format!("{FONT_FAMILY} (default)"),
        };
        let mut options: Vec<(String, bool, Handler)> = vec![(
            format!("{FONT_FAMILY} (default)"),
            font.family.is_none(),
            change(|settings| settings.terminal_font.family = None),
        )];
        options.extend(self.families.iter().filter(|family| *family != FONT_FAMILY).map(|family| {
            let name = family.clone();
            (family.clone(), font.family.as_deref() == Some(family.as_str()), change(move |settings| settings.terminal_font.family = Some(name.clone())))
        }));
        let family_row = div()
            .flex()
            .items_center()
            .gap(theme.ui(12.))
            .child(div().w(theme.ui(150.)).child("Terminal font"))
            .child(self.dropdown("font-family", 0, Popover::Font, label, theme.ui(210.), options, window, cx));
        let stepper_row = |this: &mut Self, label: &'static str, stepped: &'static Stepped, hint: &'static str, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(theme.ui(12.))
                .child(div().flex_none().w(theme.ui(150.)).child(label))
                .child(this.stepper(stepped, settings, window, cx))
                .child(muted(theme, hint).flex_1().min_w_0())
        };
        let controls = div()
            .w(theme.ui(520.))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(family_row)
            .child(stepper_row(self, "Terminal size", &TERMINAL_SIZE, "Ctrl+= / Ctrl+- change this too", cx))
            .child(stepper_row(self, "Line height", &LINE_HEIGHT, "", cx))
            .child(stepper_row(self, "Interface size", &INTERFACE_SIZE, "Sidebar, tabs, menus", cx));

        let palette = terminal_palette(theme.preset);
        let ok = color(palette.ansi[2]);
        let line = |parts: Vec<(String, Option<u32>)>| {
            div().flex().flex_row().whitespace_nowrap().children(parts.into_iter().map(move |(text, ink)| {
                div().when_some(ink, |part, ink| part.text_color(rgb(ink))).child(text)
            }))
        };
        let family = font.family.clone().unwrap_or_else(|| FONT_FAMILY.to_string());
        let sample = div()
            .flex_1()
            .min_w(theme.ui(300.))
            .px(theme.ui(16.))
            .py(theme.ui(14.))
            .rounded(px(RADIUS))
            .bg(rgb(theme.tokens.terminal))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .overflow_hidden()
            .font_family(family)
            .text_size(px(font.size))
            .line_height(px(font.size * font.line_height))
            .text_color(rgb(color(palette.foreground)))
            .child(line(vec![("~/rust_terminal".into(), Some(theme.tokens.accent)), (" $ cargo test -p app-core settings".into(), None)]))
            .child(line(vec![("running 12 tests".into(), None)]))
            .child(line(vec![("test settings::missing_keys_use_defaults ... ".into(), None), ("ok".into(), Some(ok))]))
            .child(line(vec![("test settings::low_contrast_fill_is_refused ... ".into(), None), ("ok".into(), Some(ok))]))
            .child(line(vec![
                ("test result: ".into(), None),
                ("ok".into(), Some(ok)),
                (". 12 passed; 0 failed".into(), None),
            ]));
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(heading(theme, "Fonts"))
            .child(div().flex().flex_wrap().items_start().gap(theme.ui(20.)).child(controls).child(sample))
    }

    fn alerts(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let alerts = settings.alerts;
        let wide = f32::from(self.grid_bounds.get().size.width) >= f32::from(theme.ui(GRID_WIDE));
        let trailing_space = || wide.then(|| div().flex_none().w(theme.ui(TRAILING_WIDTH)));
        let head = grid_line(
            theme,
            div().child("Event"),
            div().child("Badge"),
            div().child("Notification"),
            div().child("Sound"),
            trailing_space(),
        )
        .py(theme.ui(10.))
        .bg(rgb(theme.tokens.raised))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme.tokens.muted));
        let rows: Vec<Div> = Event::ALL
            .into_iter()
            .map(|event| {
                let mut copy = alerts;
                let channels = *event.alerts(&mut copy);
                let index = event.index();
                let badge = self.switch("badge", index, channels.badge, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.badge = !e.badge;
                }));
                let notification = self.switch("notification", index, channels.notification, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.notification = !e.notification;
                }));
                let sound = self.switch("sound", index, channels.sound, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.sound = !e.sound;
                }));
                let options = TONES
                    .iter()
                    .map(|(tone, text)| {
                        let tone = *tone;
                        (text.to_string(), tone == channels.tone, change(move |s| event.alerts(&mut s.alerts).tone = tone))
                    })
                    .collect();
                let picker = self.dropdown("tone", index, Popover::Tone(event), tone_label(channels.tone).into(), theme.ui(150.), options, window, cx);
                let tone = channels.tone;
                let play = handler(move |_, _, cx| {
                    let volume = cx.global::<SettingsStore>().settings().alerts.volume;
                    alerts::play(tone, volume);
                });
                let play = self.button("play", index, "▶ Play", window, cx, play);
                let trailing = div().flex_none().flex().items_center().gap(theme.ui(10.)).child(picker).child(play);
                let event_cell = div()
                            .flex()
                            .items_center()
                            .gap(theme.ui(10.))
                            .child(div().flex_none().size(theme.ui(10.)).rounded_full().bg(rgb(theme.tone(event.tone()).dot)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(event.label()))
                                    .child(muted(theme, event.description()).truncate()),
                            );
                let (inline, below) = if wide { (Some(trailing), None) } else { (None, Some(trailing)) };
                div()
                    .py(theme.ui(14.))
                    .border_t_1()
                    .border_color(rgb(theme.tokens.line))
                    .child(grid_line(theme, event_cell, badge, notification, sound, inline))
                    .children(below.map(|group| div().flex().justify_end().px(theme.ui(18.)).pt(theme.ui(10.)).child(group)))
            })
            .collect();
        let grid_bounds = self.grid_bounds.clone();
        let wide_at = theme.ui(GRID_WIDE);
        let measure = canvas(
            move |area, window, _| {
                let was_wide = f32::from(grid_bounds.get().size.width) >= f32::from(wide_at);
                grid_bounds.set(area);
                if was_wide != (f32::from(area.size.width) >= f32::from(wide_at)) {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        ).absolute().size_full();
        let grid = div()
            .relative()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS))
            .child(measure)
            .child(head)
            .children(rows);

        let threshold = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme.ui(10.))
            .text_color(rgb(theme.tokens.muted))
            .child("Count a session as finished after it was busy for")
            .child(self.stepper(&FINISHED_AFTER, settings, window, cx))
            .child("seconds");

        let when = [
            (
                "Alert even when the pane is on screen",
                "Off: a pane you can see in the active window raises nothing.",
                alerts.when_visible,
                change(|s| s.alerts.when_visible = !s.alerts.when_visible),
            ),
            (
                "Only send notifications while Gantry is in the background",
                "Badges and sounds still follow the grid above.",
                alerts.only_when_unfocused,
                change(|s| s.alerts.only_when_unfocused = !s.alerts.only_when_unfocused),
            ),
        ];
        let when_rows: Vec<Div> = when
            .into_iter()
            .enumerate()
            .map(|(index, (title, description, on, toggle))| {
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(16.))
                    .py(theme.ui(10.))
                    .child(self.switch("when", index, on, window, cx, toggle))
                    .child(div().child(div().font_weight(FontWeight::SEMIBOLD).child(title)).child(muted(theme, description)))
            })
            .collect();

        let sounds = self.switch("play-sounds", 0, alerts.play_sounds, window, cx, change(|s| s.alerts.play_sounds = !s.alerts.play_sounds));
        let slider = self.volume_slider(theme, alerts.volume, window, cx);
        let sound_section = div()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(heading(theme, "Sound"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(28.))
                    .child(div().flex_none().flex().items_center().gap(theme.ui(12.)).child(sounds).child(div().font_weight(FontWeight::SEMIBOLD).child("Play sounds")))
                    .child(slider),
            )
            .child(
                div()
                    .max_w(theme.ui(720.))
                    .p(theme.ui(12.))
                    .rounded(px(RADIUS))
                    .bg(rgb(theme.tokens.raised))
                    .text_color(rgb(theme.tokens.muted))
                    .child("Claude Code also sends its own notifications for the same events, so you may get two. Turn them off in Claude Code's settings if you prefer Gantry's."),
            );

        div()
            .flex()
            .flex_col()
            .gap(theme.ui(26.))
            .child(div().flex().flex_col().gap(theme.ui(10.)).child(heading(theme, "How each event alerts you")).child(grid).child(threshold))
            .child(div().flex().flex_col().gap(theme.ui(8.)).child(heading(theme, "When")).children(when_rows))
            .child(sound_section)
    }

    /// "Volume", a draggable slider (written on release; arrow keys write at once) and the percentage.
    fn volume_slider(&mut self, theme: &Theme, volume: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let shown = self.volume_drag.unwrap_or(volume);
        let handle = self.stop("volume", 0, cx);
        let focused = handle.is_focused(window);
        let dragging = self.volume_drag.is_some();
        let entity = cx.entity();
        let bounds_cell = self.slider_bounds.clone();
        let knob = theme.ui(16.);
        let rail = div()
            .relative()
            .mx(knob / 2.)
            .w_full()
            .h(theme.ui(6.))
            .rounded_full()
            .bg(rgb(theme.tokens.line))
            .child(div().h_full().w(relative(shown)).rounded_full().bg(rgb(theme.tokens.accent)))
            .child(
                div()
                    .absolute()
                    .top(theme.ui(-5.))
                    .left(relative(shown))
                    .ml(-knob / 2.)
                    .size(knob)
                    .rounded_full()
                    .bg(rgb(theme.tokens.accent)),
            )
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        bounds_cell.set(bounds);
                        if !dragging {
                            return;
                        }
                        let moving = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                moving.update(cx, |this, cx| {
                                    this.volume_drag = Some(volume_at(f32::from(event.position.x), f32::from(bounds.left()), f32::from(bounds.size.width)));
                                    cx.notify();
                                });
                            }
                        });
                        let released = entity.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                released.update(cx, |this, cx| {
                                    if let Some(volume) = this.volume_drag.take() {
                                        SettingsStore::update(cx, |s| s.alerts.volume = volume);
                                    }
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        let slider = div()
            .id("volume")
            .track_focus(&handle)
            .tab_stop(true)
            .flex_1()
            .min_w(theme.ui(120.))
            .h(theme.ui(28.))
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .border_color(if focused { rgb(theme.tokens.text) } else { rgba(0) })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let bounds = this.slider_bounds.get();
                    this.volume_drag = Some(volume_at(f32::from(event.position.x), f32::from(bounds.left()), f32::from(bounds.size.width)));
                    this.stop("volume", 0, cx).focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                let delta = match event.keystroke.key.as_str() {
                    "left" | "down" => -5.0,
                    "right" | "up" => 5.0,
                    _ => return,
                };
                let percent = (cx.global::<SettingsStore>().settings().alerts.volume * 100.0).round();
                let volume = step(percent, delta, 0.0, 100.0) / 100.0;
                SettingsStore::update(cx, |s| s.alerts.volume = volume);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(rail);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(theme.ui(12.))
            .child(div().flex_none().child("Volume"))
            .child(slider)
            .child(div().flex_none().w(theme.ui(44.)).text_color(rgb(theme.tokens.muted)).child(format!("{:.0}%", shown * 100.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::theme_colors::tokens;

    #[test]
    fn steps_stay_in_range_and_free_of_float_noise() {
        assert_eq!(step(1.3, 0.1, 1.0, 2.0), 1.4);
        assert_eq!(step(2.0, 0.1, 1.0, 2.0), 2.0);
        assert_eq!(step(1.0, -0.1, 1.0, 2.0), 1.0);
        assert_eq!(step(40.0, 1.0, 8.0, 40.0), 40.0);
        assert_eq!(step(8.0, -1.0, 8.0, 40.0), 8.0);
        assert_eq!(step(14.0, 1.0, 8.0, 40.0), 15.0);
        assert_eq!(step(95.0, 5.0, 0.0, 100.0), 100.0);
    }

    #[test]
    fn numbers_show_decimals_only_when_needed() {
        assert_eq!(format_number(14.0), "14");
        assert_eq!(format_number(1.3), "1.3");
        assert_eq!(format_number(600.0), "600");
    }

    #[test]
    fn contrast_line_has_one_decimal_below_ten() {
        let t = tokens(Preset::Mist);
        let (text, ok) = contrast_label(0xf7c9a3, &t);
        assert!(ok);
        let ratio = text.strip_prefix("Text contrast ").and_then(|rest| rest.strip_suffix(" : 1")).unwrap();
        let value: f32 = ratio.parse().unwrap();
        assert!(value >= 4.5);
        if value < 10.0 {
            assert_eq!(ratio.split('.').nth(1).map(str::len), Some(1), "{text}");
        } else {
            assert!(!ratio.contains('.'), "{text}");
        }
    }

    #[test]
    fn unreadable_fill_is_refused() {
        let t = tokens(Preset::Mist);
        // Mid grey: neither light nor dark enough for derived text.
        let grey = (0x00..=0xff_u32).map(|v| v * 0x010101).find(|fill| !readable_fill(*fill, &t)).expect("some grey is unreadable");
        assert_eq!(contrast_label(grey, &t), ("Too low contrast; choose a lighter or darker fill".to_string(), false));
    }

    #[test]
    fn the_themes_own_fill_is_stored_as_nothing() {
        assert_eq!(fill_to_store(0xc1e5cd, 0xc1e5cd), None);
        assert_eq!(fill_to_store(0xb7e4dd, 0xc1e5cd), Some(0xb7e4dd));
        assert_eq!(Status::Done.default_fill(Preset::Mist), tone_colors(Preset::Mist, &StatusFills::default(), Tone::Done).fill.unwrap());
    }

    #[test]
    fn fills_are_typed_with_or_without_the_hash() {
        assert_eq!(parse_fill("#c1e5cd"), Some(0xc1e5cd));
        assert_eq!(parse_fill(" c1e5cd "), Some(0xc1e5cd));
        assert_eq!(parse_fill("#c1e5"), None);
        assert_eq!(parse_fill(""), None);
    }

    #[test]
    fn only_fixed_width_families_are_listed_once_and_sorted() {
        let measured = [
            ("Noto Sans Mono".to_string(), 8.4, 8.4),
            ("Figtree".to_string(), 12.0, 4.0),
            ("Adwaita Mono".to_string(), 8.4, 8.4),
            ("Noto Sans Mono".to_string(), 8.4, 8.4),
        ];
        assert_eq!(monospace_families(measured), ["Adwaita Mono", "Noto Sans Mono"]);
    }

    #[test]
    fn the_slider_value_follows_the_pointer_in_whole_percents() {
        assert_eq!(volume_at(150.0, 100.0, 200.0), 0.25);
        assert_eq!(volume_at(50.0, 100.0, 200.0), 0.0);
        assert_eq!(volume_at(900.0, 100.0, 200.0), 1.0);
        assert_eq!(volume_at(100.0, 100.0, 0.0), 0.0);
    }

    #[test]
    fn problem_lines_name_the_key_when_there_is_one() {
        let keyed = Problem { key: "alerts.volume".into(), message: "expected a number".into() };
        let whole = Problem { key: String::new(), message: "not valid JSON".into() };
        assert_eq!(problem_line(&keyed), "alerts.volume: expected a number");
        assert_eq!(problem_line(&whole), "not valid JSON");
    }
}
