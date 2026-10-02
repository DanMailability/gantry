//! The Sessions section: Claude config dirs, the shell, the recent sessions pop-up and favorites.

use std::path::{Path, PathBuf};

use app_core::session_tree::display_path;
use app_core::settings::{ClaudeDir, Settings};
use app_core::text_field::TextField;
use gpui::{Context, Div, KeyDownEvent, PathPromptOptions, Window, div, px, rgb};

use super::*;
use crate::field;
use crate::settings_store::SettingsStore;
use crate::terminal_view::FONT_FAMILY;
use crate::theme::{RADIUS, RADIUS_SMALL, Theme};

/// Which text field is being edited: a control name and the row it belongs to (the focus handle's key too).
pub(super) type FieldKey = (&'static str, usize);

const DIR_NAME: &str = "dir-name";
const DIR_COMMAND: &str = "dir-command";
pub(super) const SHELL_PROGRAM: &str = "shell-program";
pub(super) const SHELL_ARGS: &str = "shell-args";

const SESSIONS_PER_PROJECT: Stepped = Stepped {
    name: "recents-per-project",
    get: |s| s.recents.per_project as f32,
    set: |s, value| s.recents.per_project = value as usize,
    delta: 1.0,
    min: 1.0,
    max: 20.0,
    unit: "",
};
const PROJECT_CHIPS: Stepped = Stepped {
    name: "recents-chips",
    get: |s| s.recents.chips as f32,
    set: |s, value| s.recents.chips = value as usize,
    delta: 1.0,
    min: 0.0,
    max: 12.0,
    unit: "",
};

/// Split a command line into words: spaces separate, single quotes keep everything, double quotes keep spaces (and
/// let `\"` and `\\` through), a backslash outside quotes escapes the next character. An open quote runs to the end.
pub(super) fn split_args(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(next @ ('"' | '\\')) => word.push(next),
                            Some(next) => {
                                word.push('\\');
                                word.push(next);
                            }
                            None => word.push('\\'),
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Words as `split_args` reads them back: ones with spaces or quotes are double-quoted.
pub(super) fn join_args(words: &[String]) -> String {
    let quoted = |word: &String| {
        if !word.is_empty() && !word.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\')) {
            return word.clone();
        }
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
    };
    words.iter().map(quoted).collect::<Vec<_>>().join(" ")
}

/// The dir rows the page shows: the configured ones, or the implicit Default at `daemon_dir` (else `~/.claude`).
fn dir_rows(settings: &Settings, daemon_dir: Option<&Path>, home: Option<&Path>) -> Vec<ClaudeDir> {
    if !settings.claude_dirs.is_empty() {
        return settings.claude_dirs.clone();
    }
    let path = daemon_dir.map(|dir| display_path(dir, home)).unwrap_or_else(|| "~/.claude".into());
    vec![ClaudeDir { name: "Default".into(), path: PathBuf::from(path), command: "claude".into(), args: Vec::new() }]
}

/// Make the implicit Default a real entry, so it can be edited or sit beside others.
fn materialize(settings: &mut Settings, daemon_dir: Option<&Path>, home: Option<&Path>) {
    if settings.claude_dirs.is_empty() {
        settings.claude_dirs = dir_rows(settings, daemon_dir, home);
    }
}

/// The name of the default dir (an unknown or unset one means the first).
fn default_name(settings: &Settings, rows: &[ClaudeDir]) -> String {
    settings.default_claude_dir.clone().filter(|name| rows.iter().any(|dir| dir.name == *name)).unwrap_or_else(|| rows[0].name.clone())
}

/// Add the dir `picked` (named after its folder, made unique), written with `~` when it is under `home`.
fn add_dir(settings: &mut Settings, picked: &Path, daemon_dir: Option<&Path>, home: Option<&Path>) {
    materialize(settings, daemon_dir, home);
    let stem = picked.file_name().map(|name| name.to_string_lossy().trim_start_matches('.').to_string()).filter(|name| !name.is_empty()).unwrap_or_else(|| "Claude".into());
    let mut name = stem.clone();
    let mut number = 2;
    while settings.claude_dirs.iter().any(|dir| dir.name == name) {
        name = format!("{stem} {number}");
        number += 1;
    }
    let path = PathBuf::from(display_path(picked, home));
    if settings.claude_dirs.iter().any(|dir| dir.path == path) {
        return;
    }
    settings.claude_dirs.push(ClaudeDir { name, path, command: "claude".into(), args: Vec::new() });
}

/// Rename dir `index`; false when the name is empty or taken. The default and the folder memory follow the rename.
fn rename_dir(settings: &mut Settings, index: usize, name: &str) -> bool {
    let name = name.trim();
    let Some(old) = settings.claude_dirs.get(index).map(|dir| dir.name.clone()) else { return false };
    if name.is_empty() || settings.claude_dirs.iter().enumerate().any(|(other, dir)| other != index && dir.name == name) {
        return false;
    }
    settings.claude_dirs[index].name = name.to_string();
    if settings.default_claude_dir.as_deref() == Some(old.as_str()) {
        settings.default_claude_dir = Some(name.to_string());
    }
    for remembered in settings.folder_claude_dirs.values_mut().filter(|remembered| **remembered == old) {
        *remembered = name.to_string();
    }
    true
}

/// Make dir `index` the default (stored as nothing when it is the first).
fn set_default(settings: &mut Settings, index: usize) {
    settings.default_claude_dir = (index > 0).then(|| settings.claude_dirs[index].name.clone());
}

/// Remove dir `index`, its default status and the folders that remembered it.
fn remove_dir(settings: &mut Settings, index: usize) {
    if index >= settings.claude_dirs.len() {
        return;
    }
    let removed = settings.claude_dirs.remove(index);
    if settings.default_claude_dir.as_deref() == Some(removed.name.as_str()) {
        settings.default_claude_dir = None;
    }
    settings.folder_claude_dirs.retain(|_, name| *name != removed.name);
}

/// Move favorite `index` by `delta` places.
fn move_favorite(settings: &mut Settings, index: usize, delta: isize) {
    let Some(target) = index.checked_add_signed(delta).filter(|target| *target < settings.favorites.len()) else { return };
    settings.favorites.swap(index, target);
}

impl SettingsView {
    /// The text a field starts with when editing begins.
    fn field_text(&self, key: FieldKey, settings: &Settings) -> String {
        let rows = dir_rows(settings, self.daemon_dir.as_deref(), std::env::home_dir().as_deref());
        match key {
            (DIR_NAME, index) => rows.get(index).map(|dir| dir.name.clone()),
            (DIR_COMMAND, index) => rows.get(index).map(|dir| join_args(&[std::slice::from_ref(&dir.command), &dir.args].concat())),
            (SHELL_PROGRAM, _) => Some(settings.shell.program.clone()),
            (SHELL_ARGS, _) => Some(join_args(&settings.shell.args)),
            _ => None,
        }
        .unwrap_or_default()
    }

    /// Listen for `key`'s field gaining and losing focus (once): editing starts on focus, and is written on blur.
    fn watch_field(&mut self, key: FieldKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.field_subs.contains_key(&key) {
            return;
        }
        let handle = self.stop(key.0, key.1, cx);
        let focus_in = cx.on_focus_in(&handle, window, move |this, _, cx| {
            let text = this.field_text(key, cx.global::<SettingsStore>().settings());
            this.edit = TextField::default();
            this.edit.insert(&text);
            this.editing = Some(key);
            this.sessions_message = None;
            cx.notify();
        });
        let focus_out = cx.on_focus_out(&handle, window, move |this, _, _, cx| {
            this.commit_field(key, cx);
        });
        self.field_subs.insert(key, vec![focus_in, focus_out]);
    }

    /// Write the edited text of `key` to the settings (reverting, with a message, what is not allowed).
    fn commit_field(&mut self, key: FieldKey, cx: &mut Context<Self>) {
        if self.editing != Some(key) {
            return;
        }
        self.editing = None;
        let text = self.edit.text().to_string();
        if text == self.field_text(key, cx.global::<SettingsStore>().settings()) {
            return;
        }
        let (daemon_dir, home) = (self.daemon_dir.clone(), std::env::home_dir());
        let mut refused = None;
        SettingsStore::update(cx, |settings| match key {
            (DIR_NAME, index) => {
                materialize(settings, daemon_dir.as_deref(), home.as_deref());
                if !rename_dir(settings, index, &text) {
                    refused = Some("A config dir needs a name that no other has.");
                }
            }
            (DIR_COMMAND, index) => {
                let mut words = split_args(&text).into_iter();
                match words.next() {
                    Some(command) => {
                        materialize(settings, daemon_dir.as_deref(), home.as_deref());
                        if let Some(dir) = settings.claude_dirs.get_mut(index) {
                            dir.command = command;
                            dir.args = words.collect();
                        }
                    }
                    None => refused = Some("A config dir needs a command."),
                }
            }
            (SHELL_PROGRAM, _) => settings.shell.program = text.trim().to_string(),
            (SHELL_ARGS, _) => settings.shell.args = split_args(&text),
            _ => {}
        });
        self.sessions_message = refused.map(String::from);
        cx.notify();
    }

    /// An editable text box for `key` showing `value` (the edited text while it has focus).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn text_field(&mut self, key: FieldKey, value: &str, placeholder: &'static str, mono: bool, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        self.watch_field(key, window, cx);
        let handle = self.stop(key.0, key.1, cx);
        let focused = handle.is_focused(window);
        let editing = focused && self.editing == Some(key);
        let shown = if editing {
            self.edit.clone()
        } else {
            let mut field = TextField::default();
            field.insert(value);
            field
        };
        div()
            .id(gpui::ElementId::from(key))
            .track_focus(&handle)
            .tab_stop(true)
            .h(theme.ui(32.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .border_1()
            .border_color(rgb(if focused { theme.tokens.accent } else { theme.tokens.line }))
            .when(mono, |field| field.font_family(FONT_FAMILY).text_size(theme.ui(12.)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if this.editing != Some(key) {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "enter" => {
                        this.commit_field(key, cx);
                        this.focus_handle.focus(window, cx);
                    }
                    "escape" => {
                        this.editing = None;
                        this.focus_handle.focus(window, cx);
                    }
                    _ if field::edit(&mut this.edit, &event.keystroke) => {}
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.stop(key.0, key.1, cx).focus(window, cx)))
            .child(field::render(&shown, placeholder, focused, theme))
    }

    pub(super) fn sessions(&mut self, theme: &Theme, settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(24.))
            .child(self.claude_dirs_section(theme, settings, window, cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_x(theme.ui(40.))
                    .gap_y(theme.ui(24.))
                    .child(self.shell_and_recents(theme, settings, window, cx))
                    .child(self.favorites_section(theme, settings, window, cx)),
            )
    }

    fn claude_dirs_section(&mut self, theme: &Theme, settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let home = std::env::home_dir();
        let rows = dir_rows(settings, self.daemon_dir.as_deref(), home.as_deref());
        let default = default_name(settings, &rows);
        let only = rows.len() == 1;
        let cards: Vec<Div> = rows
            .iter()
            .enumerate()
            .map(|(index, dir)| {
                let is_default = dir.name == default;
                let name = self.text_field((DIR_NAME, index), &dir.name, "Name", false, theme, window, cx).w(theme.ui(150.)).flex_none();
                let command = join_args(&[std::slice::from_ref(&dir.command), &dir.args].concat());
                let command = self.text_field((DIR_COMMAND, index), &command, "Command and arguments", true, theme, window, cx).flex_1().min_w(theme.ui(200.));
                let (daemon_dir, home) = (self.daemon_dir.clone(), home.clone());
                let radio = self
                    .pressable("dir-default", index, None, window, cx, change(move |settings| {
                        materialize(settings, daemon_dir.as_deref(), home.as_deref());
                        set_default(settings, index);
                    }))
                    .flex()
                    .items_center()
                    .gap(theme.ui(8.))
                    .child(
                        div()
                            .flex_none()
                            .size(theme.ui(14.))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(if is_default { theme.tokens.accent } else { theme.tokens.muted }))
                            .when(is_default, |dot| dot.bg(rgb(theme.tokens.accent))),
                    )
                    .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child("Default"));
                let removable = !only && !is_default;
                let remove = self
                    .pressable("dir-remove", index, None, window, cx, change(move |settings| {
                        if removable {
                            remove_dir(settings, index);
                        }
                    }))
                    .h(theme.ui(30.))
                    .px(theme.ui(6.))
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(if removable { theme.tokens.muted } else { theme.tokens.line }))
                    .child("Remove");
                div()
                    .p(theme.ui(12.))
                    .flex()
                    .flex_col()
                    .gap(theme.ui(8.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(theme.ui(10.))
                            .child(name)
                            .child(command)
                            .child(div().flex_none().flex().items_center().gap(theme.ui(10.)).child(radio).child(remove)),
                    )
                    .child(div().font_family(FONT_FAMILY).text_size(theme.ui(12.)).text_color(rgb(theme.tokens.muted)).truncate().child(dir.path.display().to_string()))
            })
            .collect();

        let (daemon_dir, add_home) = (self.daemon_dir.clone(), home.clone());
        let add = self.button(
            "dir-add",
            0,
            "+ Add config dir…",
            window,
            cx,
            handler(move |_, window, cx| {
                let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                let (daemon_dir, home) = (daemon_dir.clone(), add_home.clone());
                cx.spawn_in(window, async move |_, cx| {
                    if let Ok(Ok(Some(paths))) = picked.await
                        && let Some(path) = paths.into_iter().next()
                    {
                        let _ = cx.update(|_, cx| SettingsStore::update(cx, |settings| add_dir(settings, &path, daemon_dir.as_deref(), home.as_deref())));
                    }
                })
                .detach();
            }),
        );
        let remembered = (!settings.folder_claude_dirs.is_empty()).then(|| {
            let list = settings.folder_claude_dirs.iter().map(|(folder, name)| format!("{} → {name}", display_path(folder, home.as_deref()))).collect::<Vec<_>>().join(" · ");
            let forget = self
                .pressable("dir-forget", 0, None, window, cx, change(|settings| settings.folder_claude_dirs.clear()))
                .text_xs()
                .text_color(rgb(theme.tokens.accent))
                .child("Forget all");
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(theme.ui(8.))
                .text_xs()
                .text_color(rgb(theme.tokens.muted))
                .child("Remembered folders:")
                .child(div().min_w_0().text_color(rgb(theme.tokens.text)).child(list))
                .child(forget)
        });
        let message = self.sessions_message.clone().map(|text| {
            let colors = theme.tone(Tone::NeedsInput);
            div().px(theme.ui(10.)).py(theme.ui(6.)).rounded(px(RADIUS_SMALL)).bg(rgb(colors.fill.unwrap_or(theme.tokens.raised))).text_color(rgb(colors.ink)).text_sm().child(text)
        });

        div()
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_x(theme.ui(12.))
                    .child(heading(theme, "Claude config dirs"))
                    .child(muted(theme, "New folders use the default. Pick another in the launcher or a right-click menu; Gantry remembers it for that folder.").flex_1().min_w(theme.ui(240.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(RADIUS))
                    .border_1()
                    .border_color(rgb(theme.tokens.line))
                    .children(cards.into_iter().enumerate().map(|(index, card)| card.when(index > 0, |card| card.border_t_1().border_color(rgb(theme.tokens.line))))),
            )
            .children(message)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme.ui(14.))
                    .child(add)
                    .child(muted(theme, "Dev containers always run plain claude; the arguments still apply. Gantry never writes inside these folders.").flex_1().min_w(theme.ui(240.))),
            )
            .children(remembered)
    }

    fn shell_and_recents(&mut self, theme: &Theme, settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let stepper_row = |this: &mut Self, label: &'static str, stepped: &'static Stepped, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(theme.ui(12.))
                .gap_y(theme.ui(4.))
                .child(div().flex_none().w(theme.ui(170.)).child(label))
                .child(this.stepper(stepped, settings, window, cx))
        };
        div()
            .flex_1()
            .min_w(theme.ui(300.))
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(heading(theme, "Recent sessions pop-up"))
            .child(stepper_row(self, "Sessions per project", &SESSIONS_PER_PROJECT, cx))
            .child(stepper_row(self, "Project chips", &PROJECT_CHIPS, cx))
    }

    fn favorites_section(&mut self, theme: &Theme, settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let home = std::env::home_dir();
        let count = settings.favorites.len();
        let rows: Vec<Div> = settings
            .favorites
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let arrow = |this: &mut Self, name: &'static str, glyph: &'static str, delta: isize, enabled: bool, cx: &mut Context<Self>| {
                    this.pressable(name, index, None, window, cx, change(move |settings| {
                        if enabled {
                            move_favorite(settings, index, delta);
                        }
                    }))
                    .size(theme.ui(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(if enabled { theme.tokens.muted } else { theme.tokens.line }))
                    .child(glyph)
                };
                let up = arrow(self, "favorite-up", "↑", -1, index > 0, cx);
                let down = arrow(self, "favorite-down", "↓", 1, index + 1 < count, cx);
                let target = path.clone();
                let unpin = self
                    .pressable("favorite-unpin", index, None, window, cx, change(move |settings| settings.favorites.retain(|favorite| *favorite != target)))
                    .h(theme.ui(28.))
                    .px(theme.ui(4.))
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(theme.tokens.muted))
                    .child("Unpin");
                div()
                    .min_h(theme.ui(40.))
                    .px(theme.ui(12.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(theme.ui(10.))
                    .rounded(px(RADIUS_SMALL))
                    .bg(rgb(theme.tokens.raised))
                    .child(div().flex_none().text_color(rgb(theme.tokens.accent)).child("★"))
                    .child(div().flex_1().min_w(theme.ui(120.)).truncate().font_family(FONT_FAMILY).text_size(theme.ui(12.)).child(display_path(path, home.as_deref())))
                    .child(div().flex_none().flex().items_center().child(up).child(down).child(unpin))
            })
            .collect();
        div()
            .flex_1()
            .min_w(theme.ui(300.))
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .child(heading(theme, "Favorites"))
            .child(muted(theme, "First in the launcher. Pin from a project's right-click menu."))
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    fn dir(name: &str, path: &str) -> ClaudeDir {
        ClaudeDir { name: name.into(), path: path.into(), command: "claude".into(), args: vec![] }
    }

    #[test]
    fn split_args_respects_quotes() {
        assert_eq!(split_args("claude --model opus"), words(&["claude", "--model", "opus"]));
        assert_eq!(split_args("  a   b "), words(&["a", "b"]));
        assert_eq!(split_args(r#"run "two words" 'single $x' plain\ space"#), words(&["run", "two words", "single $x", "plain space"]));
        assert_eq!(split_args(r#"say "a \"quoted\" \\ word""#), words(&["say", r#"a "quoted" \ word"#]));
        assert_eq!(split_args(r#"empty "" end"#), words(&["empty", "", "end"]));
        assert_eq!(split_args(r#"open "unterminated rest"#), words(&["open", "unterminated rest"]));
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn join_args_reads_back_the_same() {
        for list in [words(&["claude", "--model", "opus"]), words(&["a b", "c\"d", "", "e\\f", "it's"])] {
            assert_eq!(split_args(&join_args(&list)), list);
        }
        assert_eq!(join_args(&words(&["claude", "--flag"])), "claude --flag");
    }

    #[test]
    fn the_implicit_default_row_is_shown_and_materialised_on_edit() {
        let mut settings = Settings::default();
        let rows = dir_rows(&settings, Some(Path::new("/home/u/.claude")), Some(Path::new("/home/u")));
        assert_eq!((rows[0].name.as_str(), rows[0].path.as_path(), rows[0].command.as_str()), ("Default", Path::new("~/.claude"), "claude"));
        assert_eq!(dir_rows(&settings, None, None)[0].path, Path::new("~/.claude"));
        materialize(&mut settings, Some(Path::new("/home/u/.claude")), Some(Path::new("/home/u")));
        assert_eq!(settings.claude_dirs, rows);
    }

    #[test]
    fn adding_a_dir_names_it_after_its_folder_and_keeps_the_default() {
        let (daemon, home) = (Some(Path::new("/home/u/.claude")), Some(Path::new("/home/u")));
        let mut settings = Settings::default();
        add_dir(&mut settings, Path::new("/home/u/.claude-work"), daemon, home);
        assert_eq!(settings.claude_dirs.iter().map(|dir| dir.name.as_str()).collect::<Vec<_>>(), ["Default", "claude-work"]);
        assert_eq!(settings.claude_dirs[1].path, Path::new("~/.claude-work"));
        add_dir(&mut settings, Path::new("/srv/claude-work"), daemon, home);
        assert_eq!(settings.claude_dirs[2].name, "claude-work 2");
        add_dir(&mut settings, Path::new("/home/u/.claude-work"), daemon, home);
        assert_eq!(settings.claude_dirs.len(), 3, "the same folder is not added twice");
    }

    #[test]
    fn renaming_follows_the_default_and_the_memory_and_refuses_duplicates() {
        let mut settings = Settings { claude_dirs: vec![dir("A", "/a"), dir("B", "/b")], default_claude_dir: Some("B".into()), ..Settings::default() };
        settings.folder_claude_dirs.insert("/r".into(), "B".into());
        assert!(!rename_dir(&mut settings, 1, "A"));
        assert!(!rename_dir(&mut settings, 1, "  "));
        assert!(rename_dir(&mut settings, 1, " Work "));
        assert_eq!(settings.default_claude_dir.as_deref(), Some("Work"));
        assert_eq!(settings.folder_claude_dirs[Path::new("/r")], "Work");
        assert!(rename_dir(&mut settings, 1, "Work"), "the same name is fine");
    }

    #[test]
    fn default_and_remove_keep_the_settings_consistent() {
        let mut settings = Settings { claude_dirs: vec![dir("A", "/a"), dir("B", "/b"), dir("C", "/c")], ..Settings::default() };
        set_default(&mut settings, 2);
        assert_eq!(settings.default_claude_dir.as_deref(), Some("C"));
        set_default(&mut settings, 0);
        assert_eq!(settings.default_claude_dir, None, "the first is the default without saying so");
        settings.folder_claude_dirs.insert("/r".into(), "B".into());
        settings.folder_claude_dirs.insert("/s".into(), "A".into());
        remove_dir(&mut settings, 1);
        assert_eq!(settings.claude_dirs.iter().map(|dir| dir.name.as_str()).collect::<Vec<_>>(), ["A", "C"]);
        assert_eq!(settings.folder_claude_dirs.len(), 1, "memory of the removed dir is dropped");
    }

    #[test]
    fn favorites_move_within_the_list() {
        let mut settings = Settings { favorites: vec!["/a".into(), "/b".into(), "/c".into()], ..Settings::default() };
        move_favorite(&mut settings, 2, -1);
        assert_eq!(settings.favorites, [PathBuf::from("/a"), "/c".into(), "/b".into()]);
        move_favorite(&mut settings, 0, -1);
        move_favorite(&mut settings, 2, 1);
        assert_eq!(settings.favorites, [PathBuf::from("/a"), "/c".into(), "/b".into()], "the ends stay put");
    }
}
