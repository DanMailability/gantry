//! The Shortcuts section: every rebindable action with its keystroke, a recorder, and the "Replace?" card.

use app_core::settings::Settings;
use app_core::shortcuts::{self, ACTIONS, Action, Check, GO_TO_TAB, Group, Platform};
use gpui::{Context, Div, FontWeight, Keystroke, Window, div, px, rgb, rgba};

use super::*;
use crate::settings_store::SettingsStore;
use crate::theme::{RADIUS, RADIUS_SMALL, Theme};

/// Width (before scaling) one column of groups needs; two columns plus the gap are shown from twice this.
const COLUMN_WIDTH: f32 = 330.;
const COLUMN_GAP: f32 = 28.;
const GROUPS: [Group; 5] = [Group::Tabs, Group::Panes, Group::Focus, Group::Overlays, Group::Terminal];

/// A keystroke another action has, waiting for the user to say whether it may be taken over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Replace {
    id: &'static str,
    keystroke: String,
    other: &'static str,
}

/// What a keystroke pressed while recording means.
#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    /// A bare modifier press: keep waiting for the full chord.
    Ignore,
    /// Esc.
    Cancel,
    /// Backspace: the action gets no shortcut.
    Unset,
    /// A keystroke (for Go to tab: a modifier set) to check.
    Choose(String),
}

/// The keystroke in settings syntax (`ctrl-alt-shift-cmd-key`); for `modifier_set` only its modifiers.
fn keystroke_text(keystroke: &Keystroke, modifier_set: bool) -> Option<String> {
    let modifiers = &keystroke.modifiers;
    let mut parts: Vec<&str> = [(modifiers.control, "ctrl"), (modifiers.alt, "alt"), (modifiers.shift, "shift"), (modifiers.platform, "cmd")]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
    if modifier_set {
        return (!parts.is_empty()).then(|| parts.join("-"));
    }
    if keystroke.key.is_empty() {
        return None;
    }
    parts.push(&keystroke.key);
    Some(parts.join("-"))
}

fn recorded(keystroke: &Keystroke, action: &str) -> Recorded {
    let modifiers = &keystroke.modifiers;
    if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function") {
        return Recorded::Ignore;
    }
    if !(modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform) {
        return match keystroke.key.as_str() {
            "escape" => Recorded::Cancel,
            "backspace" => Recorded::Unset,
            // A plain key is checked too, so the user is told why it cannot be used.
            key if !key.is_empty() => Recorded::Choose(key.to_string()),
            _ => Recorded::Ignore,
        };
    }
    keystroke_text(keystroke, action == GO_TO_TAB).map_or(Recorded::Ignore, Recorded::Choose)
}

/// Store `value` for `action`: nothing when it is the default (only changes are written), `null` for no shortcut.
fn store_shortcut(settings: &mut Settings, action: &str, value: Option<String>, platform: Platform) {
    match value {
        Some(keystroke) if shortcuts::is_default(action, &keystroke, platform) => {
            settings.shortcuts.remove(action);
        }
        value => {
            settings.shortcuts.insert(action.to_string(), value);
        }
    }
}

/// What the key button of `action` shows.
fn shown(settings: &Settings, action: &Action, platform: Platform) -> String {
    let current = shortcuts::current(action.id, &settings.shortcuts, platform);
    match (action.id == GO_TO_TAB, current) {
        (true, Some(modifiers)) => {
            format!("{} … {}", shortcuts::label(Some(&format!("{modifiers}-1")), platform), shortcuts::label(Some(&format!("{modifiers}-9")), platform))
        }
        (_, current) => shortcuts::label(current.as_deref(), platform),
    }
}

fn label_of(id: &str) -> &'static str {
    ACTIONS.iter().find(|action| action.id == id).map_or("", |action| action.label)
}

impl SettingsView {
    /// Recording ends when its key button loses focus (a click elsewhere, another tab).
    pub(super) fn end_recording_without_focus(&mut self, window: &Window) {
        let Some(id) = self.recording else { return };
        let index = ACTIONS.iter().position(|action| action.id == id).unwrap_or_default();
        if !self.stops.get(&("shortcut", index)).is_some_and(|handle| handle.is_focused(window)) {
            self.recording = None;
        }
    }

    /// Handle a keystroke while a shortcut is being recorded; false (and not consumed) when none is.
    pub(super) fn record(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.end_recording_without_focus(window);
        let Some(id) = self.recording else { return false };
        match recorded(keystroke, id) {
            Recorded::Ignore => {}
            Recorded::Cancel => self.recording = None,
            Recorded::Unset => {
                self.recording = None;
                SettingsStore::update(cx, |settings| store_shortcut(settings, id, None, Platform::current()));
            }
            Recorded::Choose(text) => self.choose(id, text, cx),
        }
        if self.recording.is_none() {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
        true
    }

    fn choose(&mut self, id: &'static str, text: String, cx: &mut Context<Self>) {
        let platform = Platform::current();
        let current = shortcuts::bindings(&cx.global::<SettingsStore>().settings().shortcuts, platform);
        match shortcuts::check(&text, id, &current) {
            Check::Ok => {
                self.recording = None;
                SettingsStore::update(cx, |settings| store_shortcut(settings, id, Some(text), platform));
            }
            Check::Reserved(reason) => {
                let shown = shortcuts::label(Some(&text), platform);
                self.shortcut_message = Some((id, format!("{shown} {reason}. Press other keys, or Esc.")));
            }
            Check::Conflict(other) => {
                self.recording = None;
                self.pending_replace = Some(Replace { id, keystroke: text, other });
            }
            Check::Invalid => self.shortcut_message = Some((id, "Not a usable shortcut. Press other keys, or Esc.".into())),
        }
    }

    pub(super) fn shortcuts(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let platform = Platform::current();
        let two_columns_at = theme.ui(2. * COLUMN_WIDTH + COLUMN_GAP);
        let two_columns = has_room(self.grid_bounds.get().size.width, two_columns_at);
        let filter = self.shortcut_filter.text().trim().to_lowercase();

        let mut groups: Vec<Div> = Vec::new();
        for group in GROUPS {
            let rows: Vec<Div> = ACTIONS
                .iter()
                .enumerate()
                .filter(|(_, action)| action.group == group)
                .filter(|(_, action)| {
                    filter.is_empty()
                        || action.label.to_lowercase().contains(&filter)
                        || shown(settings, action, platform).to_lowercase().contains(&filter)
                })
                .map(|(index, action)| self.shortcut_row(index, action, theme, settings, platform, window, cx))
                .collect();
            if rows.is_empty() {
                continue;
            }
            groups.push(
                div()
                    .flex()
                    .flex_col()
                    .child(div().mb(theme.ui(6.)).text_size(theme.ui(13.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(theme.tokens.muted)).child(group.title()))
                    .children(rows),
            );
        }
        let column = |groups: Vec<Div>| div().flex_1().min_w_0().flex().flex_col().gap(theme.ui(18.)).children(groups);
        let body = if groups.is_empty() {
            muted(theme, "No shortcut matches.")
        } else if two_columns {
            let split = groups.len().min(2).max(groups.len() / 2).min(groups.len());
            let right = groups.split_off(split);
            div().flex().items_start().gap(theme.ui(COLUMN_GAP)).child(column(groups)).child(column(right))
        } else {
            div().flex().flex_col().child(column(groups))
        };


        let focused = self.filter_focus.is_focused(window);
        let filter_box = div()
            .id("shortcut-filter")
            .track_focus(&self.filter_focus)
            .tab_stop(true)
            .w(theme.ui(260.))
            .max_w_full()
            .h(theme.ui(34.))
            .px(theme.ui(12.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .border_1()
            .border_color(rgb(if focused { theme.tokens.accent } else { theme.tokens.line }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if field::edit(&mut this.shortcut_filter, &event.keystroke) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.filter_focus.focus(window, cx)))
            .child(field::render(&self.shortcut_filter, "Filter shortcuts", focused, theme));

        div().relative().flex().flex_col().gap(theme.ui(20.)).child(filter_box).child(body)
    }

    #[allow(clippy::too_many_arguments)]
    fn shortcut_row(
        &mut self,
        index: usize,
        action: &'static Action,
        theme: &Theme,
        settings: &Settings,
        platform: Platform,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = action.id;
        let recording = self.recording == Some(id);
        let unset = shortcuts::current(id, &settings.shortcuts, platform).is_none();
        let changed = settings.shortcuts.contains_key(id);

        let record = handler(move |this, _, cx| {
            this.recording = Some(id);
            this.shortcut_message = None;
            this.pending_replace = None;
            cx.notify();
        });
        let key_text = if recording { "Press keys…".to_string() } else { shown(settings, action, platform) };
        let key_ink = if recording { theme.tokens.accent } else if unset { theme.tokens.muted } else { theme.tokens.text };
        let key = self
            .pressable("shortcut", index, Some(if recording { theme.tokens.accent } else { theme.tokens.line }), window, cx, record)
            .h(theme.ui(28.))
            .min_w(theme.ui(120.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(RADIUS_SMALL))
            .bg(if recording { rgba((theme.tokens.accent << 8) | 0x33) } else { rgba((theme.tokens.raised << 8) | 0xff) })
            .text_size(theme.ui(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(key_ink))
            .whitespace_nowrap()
            .child(key_text);
        let reset = self
            .pressable("shortcut-reset", index, None, window, cx, change(move |settings| {
                settings.shortcuts.remove(id);
            }))
            .w(theme.ui(52.))
            .h(theme.ui(28.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(theme.ui(12.))
            .text_color(rgb(if changed { theme.tokens.text } else { theme.tokens.line }))
            .child("Reset");

        let row = div()
            .min_h(theme.ui(36.))
            .py(theme.ui(4.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(theme.ui(10.))
            .gap_y(theme.ui(4.))
            .border_b_1()
            .border_color(rgb(theme.tokens.line))
            .child(div().flex_1().min_w(theme.ui(150.)).child(action.label))
            .child(div().flex_none().flex().items_center().gap(theme.ui(10.)).child(key).child(reset));

        let message = self.shortcut_message.as_ref().filter(|(message_id, _)| *message_id == id).map(|(_, text)| {
            let colors = theme.tone(Tone::NeedsInput);
            div()
                .mt(theme.ui(4.))
                .px(theme.ui(10.))
                .py(theme.ui(6.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(colors.fill.unwrap_or(theme.tokens.raised)))
                .text_color(rgb(colors.ink))
                .text_sm()
                .child(text.clone())
        });
        let replace = self.pending_replace.clone().filter(|pending| pending.id == id).map(|pending| {
            let shown = shortcuts::label(Some(&pending.keystroke), platform);
            let other = label_of(pending.other);
            let cancel = handler(|this, _, cx| {
                this.pending_replace = None;
                cx.notify();
            });
            let confirm = handler(move |this, _, cx| {
                let Some(pending) = this.pending_replace.take() else { return };
                SettingsStore::update(cx, |settings| {
                    store_shortcut(settings, pending.other, None, platform);
                    store_shortcut(settings, pending.id, Some(pending.keystroke.clone()), platform);
                });
                cx.notify();
            });
            let cancel = self.button("shortcut-cancel", index, "Cancel", window, cx, cancel);
            let confirm = self
                .pressable("shortcut-replace", index, None, window, cx, confirm)
                .h(theme.ui(34.))
                .px(theme.ui(14.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(theme.tokens.accent))
                .text_color(rgb(theme.tokens.on_accent))
                .font_weight(FontWeight::SEMIBOLD)
                .text_sm()
                .child("Replace");
            div()
                .my(theme.ui(6.))
                .p(theme.ui(14.))
                .flex()
                .flex_col()
                .gap(theme.ui(10.))
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{shown} is used by “{other}”.")))
                .child(div().text_sm().text_color(rgb(theme.tokens.muted)).child(format!("Replace it? “{other}” will have no shortcut until you set one.")))
                .child(div().flex().flex_wrap().justify_end().gap(theme.ui(8.)).child(cancel).child(confirm))
        });
        div().flex().flex_col().child(row).children(message).children(replace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: None }
    }

    fn ctrl_shift() -> Modifiers {
        Modifiers { control: true, shift: true, ..Default::default() }
    }

    #[test]
    fn a_pressed_chord_becomes_settings_syntax() {
        assert_eq!(keystroke_text(&key("d", ctrl_shift()), false).as_deref(), Some("ctrl-shift-d"));
        let all = Modifiers { control: true, alt: true, shift: true, platform: true, ..Default::default() };
        assert_eq!(keystroke_text(&key("pageup", all), false).as_deref(), Some("ctrl-alt-shift-cmd-pageup"));
        assert_eq!(keystroke_text(&key("5", Modifiers { alt: true, ..Default::default() }), true).as_deref(), Some("alt"), "Go to tab keeps only the modifiers");
        assert_eq!(keystroke_text(&key("5", Modifiers::default()), true), None);
    }

    #[test]
    fn plain_keys_are_checked_except_escape_and_backspace() {
        let none = Modifiers::default();
        assert_eq!(recorded(&key("escape", none), "closePane"), Recorded::Cancel);
        assert_eq!(recorded(&key("backspace", none), "closePane"), Recorded::Unset);
        assert_eq!(recorded(&key("enter", none), "closePane"), Recorded::Choose("enter".into()));
        assert_eq!(recorded(&key("a", none), "closePane"), Recorded::Choose("a".into()));
        assert_eq!(recorded(&key("shift", Modifiers { shift: true, ..Default::default() }), "closePane"), Recorded::Ignore);
        assert_eq!(recorded(&key("control", Modifiers { control: true, ..Default::default() }), "closePane"), Recorded::Ignore);
        assert_eq!(recorded(&key("d", ctrl_shift()), "closePane"), Recorded::Choose("ctrl-shift-d".into()));
        assert_eq!(recorded(&key("escape", ctrl_shift()), "closePane"), Recorded::Choose("ctrl-shift-escape".into()));
    }

    #[test]
    fn storing_the_default_removes_the_override_and_unset_writes_null() {
        let mut settings = Settings::default();
        store_shortcut(&mut settings, "newShellTab", Some("ctrl-alt-shift-t".into()), Platform::Linux);
        assert!(settings.shortcuts.is_empty(), "it is the default");
        store_shortcut(&mut settings, "toggleSidebar", Some("ctrl-alt-s".into()), Platform::Linux);
        assert_eq!(settings.shortcuts["toggleSidebar"].as_deref(), Some("ctrl-alt-s"));
        store_shortcut(&mut settings, "toggleSidebar", Some("ctrl-shift-b".into()), Platform::Linux);
        assert!(settings.shortcuts.is_empty(), "back to the default");
        store_shortcut(&mut settings, "closePane", None, Platform::Linux);
        assert_eq!(settings.shortcuts["closePane"], None);
    }

    #[test]
    fn the_key_button_shows_the_label_and_go_to_tab_shows_its_range() {
        let settings = Settings::default();
        let by_id = |id: &str| ACTIONS.iter().find(|action| action.id == id).unwrap();
        assert_eq!(shown(&settings, by_id("newClaudeTab"), Platform::Linux), "Ctrl+Shift+T");
        assert_eq!(shown(&settings, by_id(GO_TO_TAB), Platform::Linux), "Alt+1 … Alt+9");
        let mut settings = Settings::default();
        settings.shortcuts.insert("closePane".into(), None);
        assert_eq!(shown(&settings, by_id("closePane"), Platform::Linux), "Not set");
    }
}
