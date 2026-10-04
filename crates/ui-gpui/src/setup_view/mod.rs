//! The first-run setup wizard (an overlay): finds Claude config dirs and imports them as profiles. It opens by itself
//! on the first start, and from Settings > Sessions > "Detect Claude dirs…". It writes the settings itself, then asks
//! the workspace to close it.

mod import;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use app_core::session_tree::{age, display_path};
use app_core::text_field::TextField;
use app_core::theme_colors::ProjectColor;
use app_core::workspace::DirBadge;
use claude_index::discovery::{self, Candidate, Source};
use gpui::{
    App, ClickEvent, ClipboardItem, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyDownEvent, PathPromptOptions, Render, SharedString, Stateful, Window, div, prelude::*, px, rgb,
};

use crate::field;
use crate::settings_store::SettingsStore;
use crate::terminal_view::FONT_FAMILY;
use crate::theme::{RADIUS_LARGE, RADIUS_SMALL, Theme, dir_badge, theme};

pub(crate) use import::{Mode, opens_by_itself};

const INSTALL_COMMAND: &str = if cfg!(windows) {
    "irm https://claude.ai/install.ps1 | iex"
} else {
    "curl -fsSL https://claude.ai/install.sh | bash"
};

/// What the wizard asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupEvent {
    /// Take the wizard down (Import, Skip, Esc, or a click outside).
    Close,
}

/// One candidate with the user's choices.
struct Row {
    candidate: Candidate,
    ticked: bool,
    default: bool,
    name: TextField,
    command: TextField,
}

/// The text field with the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Active {
    Name(usize),
    Command(usize),
}

/// The wizard overlay.
pub(crate) struct SetupView {
    focus_handle: FocusHandle,
    mode: Mode,
    rows: Vec<Row>,
    active: Option<Active>,
    /// A refusal or warning shown above the buttons.
    message: Option<String>,
    daemon_dir: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Dirs already in use (a re-run), never offered again.
    listed: Vec<PathBuf>,
    /// The places looked in, for the "nothing found" card.
    checked: [String; 3],
    /// Show the install hint: none of the dirs in use exists.
    show_install: bool,
    now_ms: u64,
}

impl EventEmitter<SetupEvent> for SetupView {}

impl Focusable for SetupView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn text(content: &str) -> TextField {
    let mut field = TextField::default();
    field.insert(content);
    field
}

/// "142 sessions · 9 projects · last used 2h ago".
fn describe(candidate: &Candidate, now_ms: u64) -> String {
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let used = match candidate.last_used_ms {
        None => "never used".to_string(),
        Some(then) => match age(now_ms, then).as_str() {
            "now" => "last used just now".to_string(),
            ago => format!("last used {ago} ago"),
        },
    };
    format!(
        "{} · {} · {used}",
        count(candidate.sessions, "session", "sessions"),
        count(candidate.projects, "project", "projects")
    )
}

fn lede(count: usize) -> String {
    let (folders, verb, dirs) =
        if count == 1 { ("folder", "looks", "a Claude config dir") } else { ("folders", "look", "Claude config dirs") };
    format!("Gantry found {count} {folders} that {verb} like {dirs}. Pick the ones to add as profiles.")
}

/// The three places looked in, as lines for the "nothing found" card.
fn checked(env: &[(String, String)], home: Option<&Path>) -> [String; 3] {
    let var = env.iter().find(|(k, v)| k == "CLAUDE_CONFIG_DIR" && !v.is_empty()).map(|(_, v)| v.as_str());
    let default_exists = home.is_some_and(|home| home.join(".claude").is_dir());
    [
        match var {
            Some(value) => format!("$CLAUDE_CONFIG_DIR · {value} · no Claude files there"),
            None => "$CLAUDE_CONFIG_DIR · not set".into(),
        },
        if default_exists { "~/.claude · no Claude files there".into() } else { "~/.claude · missing".into() },
        "~/.claude-* and ~/.config/claude* · nothing found".into(),
    ]
}

impl SetupView {
    /// A wizard that has just looked for config dirs.
    pub(crate) fn new(mode: Mode, daemon_dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut view = SetupView {
            focus_handle: cx.focus_handle(),
            mode,
            rows: Vec::new(),
            active: None,
            message: None,
            daemon_dir,
            home: std::env::home_dir(),
            listed: Vec::new(),
            checked: Default::default(),
            show_install: true,
            now_ms: 0,
        };
        view.scan(cx);
        view
    }

    /// Look again; the list is rebuilt from scratch.
    fn scan(&mut self, cx: &App) {
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        // The daemon's dir comes from the login shell, which the GUI's own environment may not have.
        let home_default = self.home.as_ref().map(|home| home.join(".claude"));
        if let Some(dir) = self.daemon_dir.as_ref().filter(|dir| Some(*dir) != home_default.as_ref()) {
            env.retain(|(key, _)| key != "CLAUDE_CONFIG_DIR");
            env.push(("CLAUDE_CONFIG_DIR".into(), dir.to_string_lossy().into_owned()));
        }
        self.now_ms = now_ms();
        let settings = cx.global::<SettingsStore>().settings();
        let mut taken: Vec<String> = settings.claude_dirs.iter().map(|dir| dir.name.clone()).collect();
        if taken.is_empty() && self.mode == Mode::Rerun {
            taken.push("Default".into());
        }
        self.listed = import::listed_paths(settings, self.mode, self.daemon_dir.as_deref(), self.home.as_deref());
        let found: Vec<Candidate> = discovery::discover(&env, self.now_ms)
            .into_iter()
            .filter(|candidate| !import::is_listed(&self.listed, &candidate.path))
            .collect();
        let default = if self.mode == Mode::FirstRun { discovery::default_pick(&found) } else { None };
        self.rows = found
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| Row {
                ticked: !candidate.stale,
                default: default == Some(index),
                name: text(&discovery::unique_name(&candidate.name, &|name| taken.iter().any(|taken| taken == name))),
                command: text("claude"),
                candidate,
            })
            .collect();
        self.fix_default();
        self.checked = checked(&env, self.home.as_deref());
        self.show_install = self.listed.iter().all(|path| !path.exists());
        self.active = None;
        self.message = None;
    }

    fn ticked(&self) -> usize {
        self.rows.iter().filter(|row| row.ticked).count()
    }

    /// First run: the default is always a ticked row (the first one when the chosen row was unticked).
    fn fix_default(&mut self) {
        if self.mode != Mode::FirstRun || self.rows.iter().any(|row| row.ticked && row.default) {
            return;
        }
        for row in &mut self.rows {
            row.default = false;
        }
        if let Some(first) = self.rows.iter_mut().find(|row| row.ticked) {
            first.default = true;
        }
    }

    fn toggle(&mut self, index: usize) {
        self.rows[index].ticked = !self.rows[index].ticked;
        if !self.rows[index].ticked && matches!(self.active, Some(Active::Name(i) | Active::Command(i)) if i == index) {
            self.active = None;
        }
        self.message = None;
        self.fix_default();
    }

    fn set_default(&mut self, index: usize) {
        for row in &mut self.rows {
            row.default = false;
        }
        self.rows[index].default = true;
        self.rows[index].ticked = true;
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        if self.ticked() == 0 {
            return;
        }
        for (index, row) in self.rows.iter().enumerate().filter(|(_, row)| row.ticked) {
            if row.name.text().trim().is_empty() {
                self.active = Some(Active::Name(index));
                self.message = Some("Every dir needs a name.".into());
                return;
            }
            if row.command.text().trim().is_empty() {
                self.active = Some(Active::Command(index));
                self.message = Some("Every dir needs a command (usually claude).".into());
                return;
            }
        }
        if cx.global::<SettingsStore>().broken() {
            self.message = Some("Not saved: fix settings.json first.".into());
            return;
        }
        let rows: Vec<import::Row> = self
            .rows
            .iter()
            .filter(|row| row.ticked)
            .map(|row| import::Row {
                path: row.candidate.path.clone(),
                name: row.name.text().to_string(),
                command: row.command.text().to_string(),
                default: row.default,
            })
            .collect();
        let (mode, daemon_dir, home) = (self.mode, self.daemon_dir.clone(), self.home.clone());
        SettingsStore::update(cx, |settings| {
            import::apply_import(settings, &rows, mode, daemon_dir.as_deref(), home.as_deref())
        });
        cx.emit(SetupEvent::Close);
    }

    /// Skip (first run: remembered), or just close (re-run).
    fn skip(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::FirstRun && !cx.global::<SettingsStore>().broken() {
            SettingsStore::update(cx, |settings| settings.setup_done = true);
        }
        cx.emit(SetupEvent::Close);
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.add_folder(&path);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// A folder picked by hand: listed and ticked when it looks like a Claude dir, else a message.
    fn add_folder(&mut self, path: &Path) {
        if import::is_listed(&self.listed, path) {
            self.message = Some("That folder is already in your config dirs.".into());
            return;
        }
        let Ok(candidate) = discovery::inspect(path, self.now_ms) else {
            let shown = display_path(path, self.home.as_deref());
            self.message =
                Some(format!("No projects/ or sessions/ in {shown}, so it doesn't look like a Claude config dir."));
            return;
        };
        self.message = None;
        if let Some(row) =
            self.rows.iter_mut().find(|row| import::is_listed(std::slice::from_ref(&row.candidate.path), path))
        {
            row.ticked = true;
        } else {
            let name =
                discovery::unique_name(&candidate.name, &|name| self.rows.iter().any(|row| row.name.text() == name));
            self.rows.push(Row { ticked: true, default: false, name: text(&name), command: text("claude"), candidate });
        }
        self.fix_default();
    }

    fn active_field(&mut self) -> Option<&mut TextField> {
        match self.active? {
            Active::Name(index) => self.rows.get_mut(index).map(|row| &mut row.name),
            Active::Command(index) => self.rows.get_mut(index).map(|row| &mut row.command),
        }
    }

    /// Tab: the next (or previous) field of a ticked row.
    fn cycle_field(&mut self, backwards: bool) {
        let fields: Vec<Active> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.ticked)
            .flat_map(|(index, _)| [Active::Name(index), Active::Command(index)])
            .collect();
        if fields.is_empty() {
            return;
        }
        let last = fields.len() - 1;
        let at = self.active.and_then(|active| fields.iter().position(|field| *field == active));
        let next = match (at, backwards) {
            (None, false) => 0,
            (None, true) => last,
            (Some(at), false) => (at + 1) % fields.len(),
            (Some(at), true) => (at + last) % fields.len(),
        };
        self.active = Some(fields[next]);
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => self.skip(cx),
            "enter" if !self.rows.is_empty() => self.import(cx),
            "tab" => self.cycle_field(keystroke.modifiers.shift),
            _ => {
                let Some(field) = self.active_field() else { return };
                if !field::edit(field, keystroke, cx) {
                    return;
                }
                self.message = None;
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn check_box(theme: &Theme, on: bool) -> Div {
    div()
        .flex_none()
        .size(theme.ui(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SMALL / 2.))
        .border_1()
        .border_color(rgb(if on { theme.tokens.accent } else { theme.tokens.line }))
        .text_size(theme.ui(11.))
        .when(on, |b| b.bg(rgb(theme.tokens.accent)).text_color(rgb(theme.tokens.on_accent)))
        .child(if on { "✓" } else { "" })
}

fn radio(theme: &Theme, on: bool) -> Div {
    div()
        .flex_none()
        .size(theme.ui(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(if on { theme.tokens.accent } else { theme.tokens.line }))
        .child(div().size(theme.ui(8.)).rounded_full().when(on, |dot| dot.bg(rgb(theme.tokens.accent))))
}

/// A button; attach `on_click` for the ones that do something. `enabled = false` draws it faded.
fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(theme.ui(32.))
        .px(theme.ui(14.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SMALL))
        .border_1()
        .border_color(rgb(if primary { theme.tokens.accent } else { theme.tokens.line }))
        .when(primary, |b| {
            b.bg(rgb(theme.tokens.accent)).text_color(rgb(theme.tokens.on_accent)).font_weight(FontWeight::SEMIBOLD)
        })
        .when(!primary, |b| b.bg(rgb(theme.tokens.raised)))
        .when(enabled, |b| b.cursor_pointer().hover(|style| style.border_color(rgb(theme.tokens.accent))))
        .when(!enabled, |b| b.opacity(0.45))
        .child(label.into())
}

fn text_box(theme: &Theme, width: f32, content: Div) -> Div {
    div()
        .w(theme.ui(width))
        .h(theme.ui(26.))
        .px(theme.ui(8.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SMALL))
        .border_1()
        .border_color(rgb(theme.tokens.line))
        .bg(rgb(theme.tokens.ground))
        .child(content)
}

impl SetupView {
    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> Div {
        let theme = theme(cx).clone();
        let row = &self.rows[index];
        let letter = row.name.text().chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
        let badge =
            DirBadge { name: row.name.text().to_string(), letter, color: ProjectColor::auto(&row.candidate.path) };
        let source = match row.candidate.source {
            Source::ConfigDirEnv => Some("$CLAUDE_CONFIG_DIR"),
            Source::HomeDefault => Some("~/.claude"),
            Source::Sibling => None,
        };
        let name = field::render(
            &row.name,
            "Name",
            self.active == Some(Active::Name(index)),
            &theme,
            field::on_press(cx, move |this, offset, clicks, _, cx| {
                this.active = Some(Active::Name(index));
                this.rows[index].name.press(offset, clicks);
                cx.notify();
            }),
        );
        let command = field::render(
            &row.command,
            "claude",
            self.active == Some(Active::Command(index)),
            &theme,
            field::on_press(cx, move |this, offset, clicks, _, cx| {
                this.active = Some(Active::Command(index));
                this.rows[index].command.press(offset, clicks);
                cx.notify();
            }),
        );
        let tick = div().id(("setup-tick", index)).cursor_pointer().child(check_box(&theme, row.ticked)).on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle(index);
                cx.notify();
            }),
        );
        let default = (self.mode == Mode::FirstRun).then(|| {
            div()
                .id(("setup-default", index))
                .flex()
                .items_center()
                .gap(theme.ui(6.))
                .text_xs()
                .text_color(rgb(theme.tokens.muted))
                .cursor_pointer()
                .child(radio(&theme, row.default))
                .child("Default")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_default(index);
                    cx.notify();
                }))
        });
        div()
            .flex()
            .items_center()
            .gap(theme.ui(12.))
            .p(theme.ui(10.))
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .bg(rgb(theme.tokens.raised))
            .when(!row.ticked, |r| r.opacity(0.55))
            .child(tick)
            .child(dir_badge(&badge, &theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(3.))
                    .child(div().flex().items_center().gap(theme.ui(8.)).child(text_box(&theme, 130., name)).children(
                        source.map(|source| div().text_xs().text_color(rgb(theme.tokens.muted)).child(source)),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.tokens.muted))
                            .child(display_path(&row.candidate.path, self.home.as_deref())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.tokens.muted))
                            .child(describe(&row.candidate, self.now_ms)),
                    ),
            )
            .child(
                div().flex_none().flex().flex_col().items_end().gap(theme.ui(6.)).children(default).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(6.))
                        .text_xs()
                        .text_color(rgb(theme.tokens.muted))
                        .child("command")
                        .child(text_box(&theme, 96., command)),
                ),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> Div {
        let theme = theme(cx).clone();
        let card = |title: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(theme.ui(8.))
                .p(theme.ui(12.))
                .rounded(px(RADIUS_SMALL))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .bg(rgb(theme.tokens.raised))
                .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(title.to_uppercase()))
        };
        let install = self.show_install.then(|| {
            card("Not installed yet?")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(10.))
                        .px(theme.ui(10.))
                        .py(theme.ui(6.))
                        .rounded(px(RADIUS_SMALL))
                        .bg(rgb(theme.tokens.ground))
                        .font_family(FONT_FAMILY)
                        .text_xs()
                        .child(div().flex_1().min_w_0().overflow_hidden().child(INSTALL_COMMAND))
                        .child(button("setup-copy", "Copy", false, true, &theme).h(theme.ui(24.)).on_click(
                            cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(INSTALL_COMMAND.to_string()));
                            }),
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.tokens.muted))
                        .child("Run it in a terminal, start claude once, then check again."),
                )
        });
        let looked = self.listed.is_empty().then(|| {
            card("Looked in").child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(2.))
                    .font_family(FONT_FAMILY)
                    .text_xs()
                    .text_color(rgb(theme.tokens.muted))
                    .children(self.checked.iter().map(|line| div().child(line.clone()))),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .px(theme.ui(22.))
            .pb(theme.ui(14.))
            .children(install)
            .children(looked)
    }
}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let found = !self.rows.is_empty();
        let (title, lede) = match (found, self.mode) {
            (true, _) => ("Set up your Claude config dirs".to_string(), lede(self.rows.len())),
            (false, Mode::FirstRun) => (
                "No Claude config dir found".to_string(),
                "Gantry reads Claude's session files to show your sessions. It needs a folder where Claude Code keeps them."
                    .to_string(),
            ),
            (false, Mode::Rerun) => (
                "No new Claude config dirs found".to_string(),
                "Every Claude config dir Gantry could find is already in your list. Choose a folder if yours is somewhere else."
                    .to_string(),
            ),
        };
        let ticked = self.ticked();
        let rows = found.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(theme.ui(6.))
                .px(theme.ui(12.))
                .pb(theme.ui(10.))
                .max_h(theme.ui(380.))
                .id("setup-rows")
                .overflow_y_scroll()
                .children((0..self.rows.len()).map(|index| self.render_row(index, cx)))
        });
        let body = (!found).then(|| self.render_empty(cx));
        let skip_label = if self.mode == Mode::FirstRun { "Skip for now" } else { "Close" };
        let import_label = match ticked {
            0 => "Import".to_string(),
            1 => "Import 1 dir".to_string(),
            n => format!("Import {n} dirs"),
        };
        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme.ui(10.))
            .px(theme.ui(22.))
            .py(theme.ui(14.))
            .border_t_1()
            .border_color(rgb(theme.tokens.line))
            .child(
                button(
                    "setup-folder",
                    if found { "Choose another folder…" } else { "Choose a folder…" },
                    !found,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_folder(window, cx))),
            )
            .when(!found, |footer| {
                footer.child(button("setup-again", "Check again", false, true, &theme).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.scan(cx);
                        cx.notify();
                    },
                )))
            })
            .child(div().flex_1())
            .child(
                button("setup-skip", skip_label, false, true, &theme)
                    .text_color(rgb(theme.tokens.muted))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.skip(cx))),
            )
            .when(found, |footer| {
                footer.child(
                    button("setup-import", import_label, true, ticked > 0, &theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import(cx))),
                )
            });
        div()
            .key_context("Setup")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.skip(cx)))
            .w(theme.ui(640.))
            .max_w_full()
            .max_h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS_LARGE))
            .overflow_hidden()
            .shadow_lg()
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(4.))
                    .px(theme.ui(22.))
                    .pt(theme.ui(20.))
                    .pb(theme.ui(12.))
                    .child(div().text_size(theme.ui(18.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_color(rgb(theme.tokens.muted)).child(lede)),
            )
            .children(rows)
            .children(body)
            .children(
                self.message
                    .clone()
                    .map(|message| div().px(theme.ui(22.)).pb(theme.ui(8.)).text_xs().child(format!("⚠ {message}"))),
            )
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(sessions: usize, projects: usize, last_used_ms: Option<u64>) -> Candidate {
        Candidate {
            path: "/x".into(),
            name: "X".into(),
            source: Source::Sibling,
            sessions,
            projects,
            last_used_ms,
            stale: false,
        }
    }

    #[test]
    fn a_row_says_how_much_is_in_the_dir_and_when_it_was_used() {
        let now = 10 * 24 * 60 * 60 * 1000;
        let hours = |h: u64| now - h * 60 * 60 * 1000;
        assert_eq!(describe(&candidate(142, 9, Some(hours(2))), now), "142 sessions · 9 projects · last used 2h ago");
        assert_eq!(describe(&candidate(1, 1, Some(now)), now), "1 session · 1 project · last used just now");
        assert_eq!(describe(&candidate(0, 0, None), now), "0 sessions · 0 projects · never used");
    }

    #[test]
    fn the_lede_counts_the_folders() {
        assert_eq!(
            lede(1),
            "Gantry found 1 folder that looks like a Claude config dir. Pick the ones to add as profiles."
        );
        assert_eq!(
            lede(3),
            "Gantry found 3 folders that look like Claude config dirs. Pick the ones to add as profiles."
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_places_looked_in_say_what_was_there() {
        let home = tempfile::tempdir().unwrap();
        let env = vec![("CLAUDE_CONFIG_DIR".to_string(), "/nope".to_string())];
        let none: Vec<(String, String)> = Vec::new();
        assert_eq!(checked(&none, Some(home.path()))[0], "$CLAUDE_CONFIG_DIR · not set");
        assert_eq!(checked(&none, Some(home.path()))[1], "~/.claude · missing");
        assert_eq!(checked(&env, Some(home.path()))[0], "$CLAUDE_CONFIG_DIR · /nope · no Claude files there");
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        assert_eq!(checked(&none, Some(home.path()))[1], "~/.claude · no Claude files there");
        assert_eq!(checked(&none, None)[1], "~/.claude · missing");
    }
}
