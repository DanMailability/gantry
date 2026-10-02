//! The Settings tab's page: a left menu of sections (Appearance, Alerts, Shortcuts, Sessions, Window) and the section's controls.
//!
//! Every control is focusable (Tab / Shift+Tab, Space or Enter to press); changes go through
//! [`SettingsStore::update`], which saves and applies them at once.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use app_core::session_tree::{Tone, display_path};
use app_core::settings::{Problem, Settings, hex};
use app_core::snapshot::Rgb;
use app_core::text_field::TextField;
use gpui::{
    App, Bounds, Context, DispatchPhase, Div, ElementId, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle, SharedString, Stateful, Subscription, Window, canvas, deferred, div, point, prelude::*, px,
    relative, rgb, rgba,
};

use crate::field;
use crate::settings_store::SettingsStore;
use crate::theme::{RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, theme};

mod alerts;
mod appearance;
mod shortcuts;

use alerts::Event;
use appearance::{Status, installed_monospace};

/// A group of settings, chosen in the left menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// Theme, status colours and fonts.
    Appearance,
    /// What raises an alert, and how.
    Alerts,
    /// Rebindable keyboard shortcuts.
    Shortcuts,
    /// Claude config dirs, shell, recent sessions and favorites.
    Sessions,
    /// Start-up behaviour and text display.
    Window,
}

impl Section {
    const ALL: [Section; 5] = [Section::Appearance, Section::Alerts, Section::Shortcuts, Section::Sessions, Section::Window];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Alerts => "Alerts",
            Section::Shortcuts => "Shortcuts",
            Section::Sessions => "Sessions",
            Section::Window => "Window",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Section::Appearance => "Changes apply right away.",
            Section::Alerts => "For sessions running in Gantry's panes.",
            Section::Shortcuts => "Click a shortcut, then press the new keys. Esc cancels.",
            Section::Sessions => "How new Claude and shell panes start.",
            Section::Window => "How Gantry starts and shows text.",
        }
    }
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

/// `value` moved by `delta`, kept within `min..=max` and rounded to one decimal.
fn step(value: f32, delta: f32, min: f32, max: f32) -> f32 {
    ((value + delta).clamp(min, max) * 10.0).round() / 10.0
}

/// A number as the steppers show it: no decimals for whole numbers.
fn format_number(value: f32) -> String {
    if value.fract() == 0.0 { format!("{value:.0}") } else { format!("{value:.1}") }
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
    /// The Shortcuts page: the action whose key is being recorded.
    recording: Option<&'static str>,
    /// Why the last recorded key was refused, under that action's row.
    shortcut_message: Option<(&'static str, String)>,
    /// A key another action has, waiting for "Replace".
    pending_replace: Option<shortcuts::Replace>,
    /// The Shortcuts page's filter field.
    shortcut_filter: TextField,
    filter_focus: FocusHandle,
    _keystrokes: Subscription,
}

impl SettingsView {
    /// A page on the Appearance section.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Recording a shortcut must see keys before the key bindings do, or the chord would run its action.
        let view = cx.weak_entity();
        let keystrokes = cx.intercept_keystrokes(move |event, window, cx| {
            let consumed = view.update(cx, |this, cx| this.record(&event.keystroke, window, cx));
            if consumed.unwrap_or(false) {
                cx.stop_propagation();
            }
        });
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
            recording: None,
            shortcut_message: None,
            pending_replace: None,
            shortcut_filter: TextField::default(),
            filter_focus: cx.focus_handle(),
            _keystrokes: keystrokes,
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
        self.end_recording_without_focus(window);

        let menu = div().flex_none().w(theme.ui(200.)).py(theme.ui(24.)).px(theme.ui(12.)).flex().flex_col().gap(theme.ui(4.)).border_r_1().border_color(rgb(theme.tokens.line)).children(
            Section::ALL.map(|item| {
                let active = item == section;
                let choose = handler(move |this, _, cx| {
                    this.section = item;
                    this.popover = None;
                    this.recording = None;
                    this.shortcut_message = None;
                    this.pending_replace = None;
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
            Section::Shortcuts => self.shortcuts(&theme, &settings, window, cx),
            Section::Sessions | Section::Window => div(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn problem_lines_name_the_key_when_there_is_one() {
        let keyed = Problem { key: "alerts.volume".into(), message: "expected a number".into() };
        let whole = Problem { key: String::new(), message: "not valid JSON".into() };
        assert_eq!(problem_line(&keyed), "alerts.volume: expected a number");
        assert_eq!(problem_line(&whole), "not valid JSON");
    }
}
