//! The settings file (`settings.json`): typed model, tolerant loading, and writing only changed keys.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Theme choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// The dark Mist theme.
    #[default]
    Mist,
    /// The light Mist theme.
    MistLight,
    /// Follow the system's light/dark setting.
    System,
}

impl ThemeChoice {
    fn name(self) -> &'static str {
        match self {
            ThemeChoice::Mist => "mist",
            ThemeChoice::MistLight => "mist-light",
            ThemeChoice::System => "system",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "mist" => Some(ThemeChoice::Mist),
            "mist-light" => Some(ThemeChoice::MistLight),
            "system" => Some(ThemeChoice::System),
            _ => None,
        }
    }
}

/// Built-in alert sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertTone {
    /// A bell.
    Bell,
    /// A short tick.
    Tick,
}

impl AlertTone {
    fn name(self) -> &'static str {
        match self {
            AlertTone::Bell => "bell",
            AlertTone::Tick => "tick",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bell" => Some(AlertTone::Bell),
            "tick" => Some(AlertTone::Tick),
            _ => None,
        }
    }
}

/// Which channels an event uses, and its sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventAlerts {
    /// Show the unseen badge.
    pub badge: bool,
    /// Raise a native notification.
    pub notification: bool,
    /// Play the sound.
    pub sound: bool,
    /// Which sound to play.
    pub tone: AlertTone,
}

/// Status fills chosen by the user (`None` = the theme's default).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct StatusFills {
    /// Fill for a session that needs input.
    pub needs_input: Option<u32>,
    /// Fill for a working session.
    pub working: Option<u32>,
    /// Fill for a finished session.
    pub done: Option<u32>,
    /// Fill for an idle session.
    pub idle: Option<u32>,
}

/// Terminal font (`family: None` = the platform default).
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFont {
    /// Font family, or the platform default.
    pub family: Option<String>,
    /// Size in points.
    pub size: f32,
    /// Line height as a multiple of the size.
    pub line_height: f32,
}

/// Alert settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlertSettings {
    /// Alerts for a session that needs input.
    pub needs_input: EventAlerts,
    /// Alerts for a session that finished.
    pub finished: EventAlerts,
    /// Seconds a session must be busy before finishing counts.
    pub finished_after_seconds: u32,
    /// Alert even when the pane is visible.
    pub when_visible: bool,
    /// Alert only while the window is unfocused.
    pub only_when_unfocused: bool,
    /// Master switch for sounds.
    pub play_sounds: bool,
    /// Sound volume, 0 to 1.
    pub volume: f32,
}

/// One Claude config dir the user listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeDir {
    /// Unique, non-empty display name.
    pub name: String,
    /// Directory as written (`~` kept).
    pub path: PathBuf,
    /// Program to run for Claude in this dir.
    pub command: String,
    /// Arguments placed before Gantry's own.
    pub args: Vec<String>,
}

/// Whether the sidebar is shown when Gantry starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidebarAtStart {
    /// Use the saved layout's state.
    #[default]
    Remember,
    /// Always shown.
    Shown,
    /// Always hidden.
    Hidden,
}

impl SidebarAtStart {
    /// Whether the sidebar is visible at start, given whether the saved layout had it visible.
    pub fn visible(self, saved: bool) -> bool {
        match self {
            SidebarAtStart::Remember => saved,
            SidebarAtStart::Shown => true,
            SidebarAtStart::Hidden => false,
        }
    }

    fn name(self) -> &'static str {
        match self {
            SidebarAtStart::Remember => "remember",
            SidebarAtStart::Shown => "shown",
            SidebarAtStart::Hidden => "hidden",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "remember" => Some(SidebarAtStart::Remember),
            "shown" => Some(SidebarAtStart::Shown),
            "hidden" => Some(SidebarAtStart::Hidden),
            _ => None,
        }
    }
}

/// The shell new shell panes run.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShellSettings {
    /// Program; empty = `$SHELL`.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

/// Sizes of the recent sessions pop-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentsSettings {
    /// Sessions listed per project (1 to 20).
    pub per_project: usize,
    /// Project chips (0 to 12).
    pub chips: usize,
}

/// Window behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSettings {
    /// Restore tabs and panes on start.
    pub restore_layout: bool,
    /// Sidebar state at start.
    pub sidebar_at_start: SidebarAtStart,
    /// Scrollback lines for new panes (1000 to 100000).
    pub scrollback: u32,
    /// Reorder right-to-left text for display.
    pub right_to_left: bool,
}

/// `~/x` becomes `<home>/x`; other paths are unchanged.
pub fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Everything in settings.json.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Theme choice.
    pub theme: ThemeChoice,
    /// Status fills.
    pub status_fills: StatusFills,
    /// Terminal font.
    pub terminal_font: TerminalFont,
    /// Interface font size in points.
    pub ui_font_size: f32,
    /// Alert settings.
    pub alerts: AlertSettings,
    /// Action id to keystroke; `None` = explicitly unset; absent = the default.
    pub shortcuts: BTreeMap<String, Option<String>>,
    /// Configured Claude config dirs; empty = the implicit "Default".
    pub claude_dirs: Vec<ClaudeDir>,
    /// Name of the default dir; `None` or unknown = the first entry.
    pub default_claude_dir: Option<String>,
    /// Folder to the name of the dir picked for it.
    pub folder_claude_dirs: BTreeMap<PathBuf, String>,
    /// Shell for new shell panes.
    pub shell: ShellSettings,
    /// Recent sessions pop-up sizes.
    pub recents: RecentsSettings,
    /// Pinned folders, in order.
    pub favorites: Vec<PathBuf>,
    /// Window behaviour.
    pub window: WindowSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::Mist,
            status_fills: StatusFills::default(),
            terminal_font: TerminalFont { family: None, size: 14.0, line_height: 1.3 },
            ui_font_size: 14.0,
            alerts: AlertSettings {
                needs_input: EventAlerts { badge: true, notification: true, sound: true, tone: AlertTone::Bell },
                finished: EventAlerts { badge: true, notification: true, sound: true, tone: AlertTone::Tick },
                finished_after_seconds: 3,
                when_visible: false,
                only_when_unfocused: false,
                play_sounds: true,
                volume: 0.4,
            },
            shortcuts: BTreeMap::new(),
            claude_dirs: Vec::new(),
            default_claude_dir: None,
            folder_claude_dirs: BTreeMap::new(),
            shell: ShellSettings::default(),
            recents: RecentsSettings { per_project: 5, chips: 6 },
            favorites: Vec::new(),
            window: WindowSettings { restore_layout: true, sidebar_at_start: SidebarAtStart::Remember, scrollback: 10_000, right_to_left: true },
        }
    }
}

/// One problem found while loading: a dotted key path (e.g. "alerts.volume", "statusColors.done") or "" for
/// the whole file, and what was wrong (for the banner).
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    /// Dotted key path, or "" for the whole file.
    pub key: String,
    /// What was wrong.
    pub message: String,
}

/// The result of reading the file.
pub struct Loaded {
    /// The settings to apply: `None` only when the file is not JSON at all (keep the last good ones).
    pub settings: Option<Settings>,
    /// Everything that was wrong.
    pub problems: Vec<Problem>,
    /// The raw JSON object as read (unknown keys included), kept so writes preserve them.
    pub raw: Map<String, Value>,
}

/// `#rrggbb` → 0xRRGGBB.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// 0xRRGGBB → `#rrggbb`.
pub fn hex(color: u32) -> String {
    format!("#{:06x}", color & 0xff_ffff)
}

/// Read `path`. A missing file gives defaults, no problems, and an empty `raw`.
pub fn load(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Loaded { settings: Some(Settings::default()), problems: Vec::new(), raw: Map::new() }
        }
        Err(error) => Loaded {
            settings: None,
            problems: vec![Problem { key: String::new(), message: format!("cannot read the settings file: {error}") }],
            raw: Map::new(),
        },
    }
}

/// Parse JSON text (`load` reads the file and calls this).
pub fn parse(text: &str) -> Loaded {
    let raw = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return broken("the top level must be a JSON object".into()),
        Err(error) => return broken(format!("not valid JSON: {error}")),
    };
    let mut reader = Reader { problems: Vec::new() };
    let settings = reader.settings(&raw);
    Loaded { settings: Some(settings), problems: reader.problems, raw }
}

fn broken(message: String) -> Loaded {
    Loaded { settings: None, problems: vec![Problem { key: String::new(), message }], raw: Map::new() }
}

struct Reader {
    problems: Vec<Problem>,
}

impl Reader {
    fn problem(&mut self, key: &str, message: String) {
        self.problems.push(Problem { key: key.into(), message });
    }

    /// The object at `key`, or an empty one (with a problem when it is present but not an object).
    fn object(&mut self, parent: &Map<String, Value>, prefix: &str, key: &str) -> Map<String, Value> {
        match parent.get(key) {
            None => Map::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected an object".into());
                Map::new()
            }
        }
    }

    fn bool(&mut self, map: &Map<String, Value>, prefix: &str, key: &str, default: bool) -> bool {
        match map.get(key) {
            None => default,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected true or false".into());
                default
            }
        }
    }

    fn number(&mut self, map: &Map<String, Value>, prefix: &str, key: &str, default: f64, range: (f64, f64)) -> f64 {
        match map.get(key) {
            None => default,
            Some(Value::Number(number)) => {
                let value = number.as_f64().unwrap_or(default);
                if value < range.0 || value > range.1 {
                    self.problem(&format!("{prefix}{key}"), format!("{value} is outside {} to {}; clamped", range.0, range.1));
                }
                value.clamp(range.0, range.1)
            }
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected a number".into());
                default
            }
        }
    }

    fn choice<T: Copy>(
        &mut self,
        map: &Map<String, Value>,
        prefix: &str,
        key: &str,
        default: T,
        from_name: fn(&str) -> Option<T>,
    ) -> T {
        match map.get(key) {
            None => default,
            Some(Value::String(name)) => from_name(name).unwrap_or_else(|| {
                self.problem(&format!("{prefix}{key}"), format!("unknown value \"{name}\""));
                default
            }),
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected a string".into());
                default
            }
        }
    }

    fn color(&mut self, map: &Map<String, Value>, prefix: &str, key: &str) -> Option<u32> {
        match map.get(key)? {
            Value::String(text) => parse_hex(text).or_else(|| {
                self.problem(&format!("{prefix}{key}"), format!("\"{text}\" is not a #rrggbb colour"));
                None
            }),
            _ => {
                self.problem(&format!("{prefix}{key}"), "expected a #rrggbb colour".into());
                None
            }
        }
    }

    fn string(&mut self, map: &Map<String, Value>, prefix: &str, key: &str) -> String {
        match map.get(key) {
            None => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected a string".into());
                String::new()
            }
        }
    }

    fn strings(&mut self, map: &Map<String, Value>, prefix: &str, key: &str) -> Vec<String> {
        match map.get(key) {
            None => Vec::new(),
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                items.iter().filter_map(|item| item.as_str().map(String::from)).collect()
            }
            Some(_) => {
                self.problem(&format!("{prefix}{key}"), "expected a list of strings".into());
                Vec::new()
            }
        }
    }

    fn shortcuts(&mut self, raw: &Map<String, Value>) -> BTreeMap<String, Option<String>> {
        let map = self.object(raw, "", "shortcuts");
        let mut out = BTreeMap::new();
        for (id, value) in map {
            match value {
                Value::String(keystroke) => {
                    out.insert(id, Some(keystroke));
                }
                Value::Null => {
                    out.insert(id, None);
                }
                _ => self.problem(&format!("shortcuts.{id}"), "expected a keystroke or null".into()),
            }
        }
        out
    }

    fn claude_dirs(&mut self, raw: &Map<String, Value>) -> Vec<ClaudeDir> {
        let items = match raw.get("claudeDirs") {
            None => return Vec::new(),
            Some(Value::Array(items)) => items,
            Some(_) => {
                self.problem("claudeDirs", "expected a list".into());
                return Vec::new();
            }
        };
        let mut dirs: Vec<ClaudeDir> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let key = format!("claudeDirs[{index}]");
            let Value::Object(map) = item else {
                self.problem(&key, "expected an object; dropped".into());
                continue;
            };
            let name = self.string(map, &format!("{key}."), "name");
            let path = self.string(map, &format!("{key}."), "path");
            let command = match map.get("command") {
                None => "claude".to_string(),
                Some(_) => self.string(map, &format!("{key}."), "command"),
            };
            let args = self.strings(map, &format!("{key}."), "args");
            if name.is_empty() || path.is_empty() || command.is_empty() {
                self.problem(&key, "needs a name, a path and a command; dropped".into());
            } else if dirs.iter().any(|dir| dir.name == name) {
                self.problem(&key, format!("the name \"{name}\" is used twice; dropped"));
            } else if dirs.iter().any(|dir| dir.path == Path::new(&path)) {
                self.problem(&key, format!("the path \"{path}\" is used twice; dropped"));
            } else {
                dirs.push(ClaudeDir { name, path: path.into(), command, args });
            }
        }
        dirs
    }

    fn folder_claude_dirs(&mut self, raw: &Map<String, Value>, names: &BTreeSet<&str>) -> BTreeMap<PathBuf, String> {
        let map = self.object(raw, "", "folderClaudeDirs");
        let mut out = BTreeMap::new();
        for (folder, value) in map {
            let Value::String(name) = value else {
                self.problem(&format!("folderClaudeDirs.{folder}"), "expected a dir name".into());
                continue;
            };
            if !names.contains(name.as_str()) {
                self.problem(&format!("folderClaudeDirs.{folder}"), format!("no config dir named \"{name}\"; the default is used"));
            }
            out.insert(PathBuf::from(folder), name);
        }
        out
    }

    fn favorites(&mut self, raw: &Map<String, Value>) -> Vec<PathBuf> {
        self.strings(raw, "", "favorites").into_iter().map(PathBuf::from).collect()
    }

    fn event(&mut self, parent: &Map<String, Value>, key: &str, default: EventAlerts) -> EventAlerts {
        let map = self.object(parent, "alerts.", key);
        let prefix = format!("alerts.{key}.");
        EventAlerts {
            badge: self.bool(&map, &prefix, "badge", default.badge),
            notification: self.bool(&map, &prefix, "notification", default.notification),
            sound: self.bool(&map, &prefix, "sound", default.sound),
            tone: self.choice(&map, &prefix, "tone", default.tone, AlertTone::from_name),
        }
    }

    fn settings(&mut self, raw: &Map<String, Value>) -> Settings {
        let defaults = Settings::default();
        let theme = self.choice(raw, "", "theme", defaults.theme, ThemeChoice::from_name);

        let colors = self.object(raw, "", "statusColors");
        let status_fills = StatusFills {
            needs_input: self.color(&colors, "statusColors.", "needsInput"),
            working: self.color(&colors, "statusColors.", "working"),
            done: self.color(&colors, "statusColors.", "done"),
            idle: self.color(&colors, "statusColors.", "idle"),
        };

        let font = self.object(raw, "", "terminalFont");
        let family = match font.get("family") {
            None => None,
            Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
            Some(Value::String(_)) => None,
            Some(_) => {
                self.problem("terminalFont.family", "expected a font name".into());
                None
            }
        };
        let terminal_font = TerminalFont {
            family,
            size: self.number(&font, "terminalFont.", "size", 14.0, (8.0, 40.0)) as f32,
            line_height: self.number(&font, "terminalFont.", "lineHeight", 1.3, (1.0, 2.0)) as f32,
        };

        let ui_font_size = self.number(raw, "", "uiFontSize", 14.0, (11.0, 20.0)) as f32;

        let map = self.object(raw, "", "alerts");
        let default = defaults.alerts;
        let alerts = AlertSettings {
            needs_input: self.event(&map, "needsInput", default.needs_input),
            finished: self.event(&map, "finished", default.finished),
            finished_after_seconds: self.number(&map, "alerts.", "finishedAfterSeconds", 3.0, (0.0, 600.0)).round() as u32,
            when_visible: self.bool(&map, "alerts.", "whenVisible", default.when_visible),
            only_when_unfocused: self.bool(&map, "alerts.", "onlyWhenUnfocused", default.only_when_unfocused),
            play_sounds: self.bool(&map, "alerts.", "playSounds", default.play_sounds),
            volume: self.number(&map, "alerts.", "volume", 0.4, (0.0, 1.0)) as f32,
        };

        let shortcuts = self.shortcuts(raw);
        let claude_dirs = self.claude_dirs(raw);
        let implicit = BTreeSet::from(["Default"]);
        let names: BTreeSet<&str> = if claude_dirs.is_empty() { implicit } else { claude_dirs.iter().map(|dir| dir.name.as_str()).collect() };
        let default_claude_dir = match raw.get("defaultClaudeDir") {
            None => None,
            Some(Value::String(name)) => {
                if !names.contains(name.as_str()) {
                    self.problem("defaultClaudeDir", format!("no config dir named \"{name}\"; the first one is the default"));
                }
                Some(name.clone())
            }
            Some(_) => {
                self.problem("defaultClaudeDir", "expected a dir name".into());
                None
            }
        };
        let folder_claude_dirs = self.folder_claude_dirs(raw, &names);

        let map = self.object(raw, "", "shell");
        let shell = ShellSettings { program: self.string(&map, "shell.", "program"), args: self.strings(&map, "shell.", "args") };

        let map = self.object(raw, "", "recents");
        let recents = RecentsSettings {
            per_project: self.number(&map, "recents.", "perProject", 5.0, (1.0, 20.0)).round() as usize,
            chips: self.number(&map, "recents.", "chips", 6.0, (0.0, 12.0)).round() as usize,
        };

        let favorites = self.favorites(raw);

        let map = self.object(raw, "", "window");
        let window = WindowSettings {
            restore_layout: self.bool(&map, "window.", "restoreLayout", defaults.window.restore_layout),
            sidebar_at_start: self.choice(&map, "window.", "sidebarAtStart", defaults.window.sidebar_at_start, SidebarAtStart::from_name),
            scrollback: self.number(&map, "window.", "scrollback", 10_000.0, (1000.0, 100_000.0)).round() as u32,
            right_to_left: self.bool(&map, "window.", "rightToLeft", defaults.window.right_to_left),
        };

        Settings {
            theme,
            status_fills,
            terminal_font,
            ui_font_size,
            alerts,
            shortcuts,
            claude_dirs,
            default_claude_dir,
            folder_claude_dirs,
            shell,
            recents,
            favorites,
            window,
        }
    }
}

/// A float as JSON without f32 noise (0.4 stays 0.4); whole numbers are written as integers.
fn number(value: f32) -> Value {
    let value: f64 = value.to_string().parse().unwrap_or(0.0);
    if value.fract() == 0.0 && value.abs() < 1e9 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

/// Every known key of `settings` as (path, value, differs from the default).
fn leaves(settings: &Settings) -> Vec<(Vec<&'static str>, Value, bool)> {
    let default = Settings::default();
    let mut out = Vec::new();
    let mut add = |path: &[&'static str], value: Value, changed: bool| out.push((path.to_vec(), value, changed));
    add(&["theme"], settings.theme.name().into(), settings.theme != default.theme);
    let fills = [
        ("needsInput", settings.status_fills.needs_input),
        ("working", settings.status_fills.working),
        ("done", settings.status_fills.done),
        ("idle", settings.status_fills.idle),
    ];
    for (key, fill) in fills {
        add(&["statusColors", key], fill.map_or(Value::Null, |color| hex(color).into()), fill.is_some());
    }
    let font = &settings.terminal_font;
    add(&["terminalFont", "family"], font.family.clone().map_or(Value::Null, Value::from), font.family.is_some());
    add(&["terminalFont", "size"], number(font.size), font.size != default.terminal_font.size);
    add(&["terminalFont", "lineHeight"], number(font.line_height), font.line_height != default.terminal_font.line_height);
    add(&["uiFontSize"], number(settings.ui_font_size), settings.ui_font_size != default.ui_font_size);
    let (alerts, base) = (&settings.alerts, &default.alerts);
    for (key, event, base) in [("needsInput", alerts.needs_input, base.needs_input), ("finished", alerts.finished, base.finished)] {
        add(&["alerts", key, "badge"], event.badge.into(), event.badge != base.badge);
        add(&["alerts", key, "notification"], event.notification.into(), event.notification != base.notification);
        add(&["alerts", key, "sound"], event.sound.into(), event.sound != base.sound);
        add(&["alerts", key, "tone"], event.tone.name().into(), event.tone != base.tone);
    }
    add(&["alerts", "finishedAfterSeconds"], alerts.finished_after_seconds.into(), alerts.finished_after_seconds != base.finished_after_seconds);
    add(&["alerts", "whenVisible"], alerts.when_visible.into(), alerts.when_visible != base.when_visible);
    add(&["alerts", "onlyWhenUnfocused"], alerts.only_when_unfocused.into(), alerts.only_when_unfocused != base.only_when_unfocused);
    add(&["alerts", "playSounds"], alerts.play_sounds.into(), alerts.play_sounds != base.play_sounds);
    add(&["alerts", "volume"], number(alerts.volume), alerts.volume != base.volume);
    let shortcuts: Map<String, Value> =
        settings.shortcuts.iter().map(|(id, key)| (id.clone(), key.clone().map_or(Value::Null, Value::from))).collect();
    add(&["shortcuts"], Value::Object(shortcuts), !settings.shortcuts.is_empty());
    let dirs: Vec<Value> = settings
        .claude_dirs
        .iter()
        .map(|dir| serde_json::json!({"name": dir.name, "path": dir.path.to_string_lossy(), "command": dir.command, "args": dir.args}))
        .collect();
    add(&["claudeDirs"], Value::Array(dirs), !settings.claude_dirs.is_empty());
    add(&["defaultClaudeDir"], settings.default_claude_dir.clone().map_or(Value::Null, Value::from), settings.default_claude_dir.is_some());
    let folders: Map<String, Value> =
        settings.folder_claude_dirs.iter().map(|(folder, name)| (folder.to_string_lossy().into_owned(), name.clone().into())).collect();
    add(&["folderClaudeDirs"], Value::Object(folders), !settings.folder_claude_dirs.is_empty());
    add(&["shell", "program"], settings.shell.program.clone().into(), settings.shell.program != default.shell.program);
    add(&["shell", "args"], settings.shell.args.clone().into(), settings.shell.args != default.shell.args);
    add(&["recents", "perProject"], settings.recents.per_project.into(), settings.recents.per_project != default.recents.per_project);
    add(&["recents", "chips"], settings.recents.chips.into(), settings.recents.chips != default.recents.chips);
    let favorites: Vec<Value> = settings.favorites.iter().map(|path| path.to_string_lossy().into_owned().into()).collect();
    add(&["favorites"], Value::Array(favorites), !settings.favorites.is_empty());
    let (window, base) = (&settings.window, &default.window);
    add(&["window", "restoreLayout"], window.restore_layout.into(), window.restore_layout != base.restore_layout);
    add(&["window", "sidebarAtStart"], window.sidebar_at_start.name().into(), window.sidebar_at_start != base.sidebar_at_start);
    add(&["window", "scrollback"], window.scrollback.into(), window.scrollback != base.scrollback);
    add(&["window", "rightToLeft"], window.right_to_left.into(), window.right_to_left != base.right_to_left);
    out
}

/// Set (`Some`) or remove (`None`) the value at `path`; objects left empty by a removal are removed too.
fn put(map: &mut Map<String, Value>, path: &[&str], value: Option<Value>) {
    let (key, rest) = path.split_first().expect("a non-empty path");
    if rest.is_empty() {
        match value {
            Some(value) => {
                map.insert((*key).into(), value);
            }
            None => {
                map.remove(*key);
            }
        }
        return;
    }
    if value.is_some() && !matches!(map.get(*key), Some(Value::Object(_))) {
        map.insert((*key).into(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(child)) = map.get_mut(*key) {
        put(child, rest, value);
        if child.is_empty() {
            map.remove(*key);
        }
    }
}

/// Write `settings` to `path` atomically: `raw` with every known key replaced by its value in `settings`, and
/// known keys equal to their default removed (an object left empty is removed too). Unknown keys stay.
pub fn save(path: &Path, settings: &Settings, raw: &Map<String, Value>) -> io::Result<()> {
    let mut out = raw.clone();
    for (key, value, changed) in leaves(settings) {
        put(&mut out, &key, changed.then_some(value));
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(out)).map_err(io::Error::other)?;
    text.push('\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load(&dir.path().join("settings.json"));
        assert_eq!(loaded.settings, Some(Settings::default()));
        assert!(loaded.problems.is_empty());
        assert!(loaded.raw.is_empty());
    }

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.theme, ThemeChoice::Mist);
        assert_eq!((s.terminal_font.size, s.terminal_font.line_height, s.terminal_font.family.clone()), (14.0, 1.3, None));
        assert_eq!(s.ui_font_size, 14.0);
        assert_eq!(s.alerts.finished_after_seconds, 3);
        assert_eq!(s.alerts.volume, 0.4);
        assert_eq!((s.alerts.needs_input.tone, s.alerts.finished.tone), (AlertTone::Bell, AlertTone::Tick));
        assert_eq!(s.status_fills, StatusFills::default());
        assert!(!s.alerts.when_visible && !s.alerts.only_when_unfocused && s.alerts.play_sounds);
    }

    #[test]
    fn default_values_are_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &Settings::default(), &Map::new()).unwrap();
        assert_eq!(read_json(&path), serde_json::json!({}));
    }

    #[test]
    fn only_changed_keys_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.json");
        let mut settings = Settings::default();
        settings.status_fills.done = Some(0xaabbcc);
        settings.alerts.volume = 0.7;
        save(&path, &settings, &Map::new()).unwrap();
        assert_eq!(read_json(&path), serde_json::json!({"statusColors": {"done": "#aabbcc"}, "alerts": {"volume": 0.7}}));
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("}\n"));
        assert!(!path.with_file_name("settings.json.tmp").exists());
    }

    #[test]
    fn unknown_keys_survive_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let loaded = parse(r#"{"futureKey": 1, "alerts": {"futureNested": true}}"#);
        assert!(loaded.problems.is_empty());
        save(&path, &loaded.settings.unwrap(), &loaded.raw).unwrap();
        assert_eq!(read_json(&path), serde_json::json!({"futureKey": 1, "alerts": {"futureNested": true}}));
    }

    #[test]
    fn out_of_range_is_clamped_with_a_problem() {
        let loaded = parse(r#"{"terminalFont": {"size": 99}}"#);
        assert_eq!(loaded.settings.unwrap().terminal_font.size, 40.0);
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.problems[0].key, "terminalFont.size");
    }

    #[test]
    fn bad_values_fall_back_per_key() {
        let loaded = parse(r#"{"theme": "neon", "statusColors": {"done": "green"}, "alerts": {"volume": "loud"}, "uiFontSize": 16}"#);
        let settings = loaded.settings.unwrap();
        let defaults = Settings::default();
        assert_eq!(settings.theme, defaults.theme);
        assert_eq!(settings.status_fills.done, None);
        assert_eq!(settings.alerts.volume, defaults.alerts.volume);
        assert_eq!(settings.ui_font_size, 16.0);
        let keys: Vec<_> = loaded.problems.iter().map(|problem| problem.key.as_str()).collect();
        assert_eq!(keys, ["theme", "statusColors.done", "alerts.volume"]);
    }

    #[test]
    fn broken_file_keeps_last_good() {
        let loaded = parse("{ \"theme\": ");
        assert!(loaded.settings.is_none());
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.problems[0].key, "");
        assert!(loaded.problems[0].message.contains("line 1"), "{}", loaded.problems[0].message);
        assert!(parse("[1]").settings.is_none());
    }

    #[test]
    fn round_trip() {
        let settings = Settings {
            theme: ThemeChoice::MistLight,
            status_fills: StatusFills { needs_input: Some(0x112233), working: Some(0x445566), done: Some(0x778899), idle: Some(0xaabbcc) },
            terminal_font: TerminalFont { family: Some("Fira Code".into()), size: 17.5, line_height: 1.45 },
            ui_font_size: 16.0,
            alerts: AlertSettings {
                needs_input: EventAlerts { badge: false, notification: false, sound: false, tone: AlertTone::Tick },
                finished: EventAlerts { badge: false, notification: true, sound: false, tone: AlertTone::Bell },
                finished_after_seconds: 30,
                when_visible: true,
                only_when_unfocused: true,
                play_sounds: false,
                volume: 0.7,
            },
            ..Settings::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &settings, &Map::new()).unwrap();
        let loaded = load(&path);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings, Some(settings));
    }

    fn phase2_non_default() -> Settings {
        let mut settings = Settings::default();
        settings.shortcuts.insert("toggleSidebar".into(), Some("ctrl-alt-s".into()));
        settings.shortcuts.insert("splitRightClaude".into(), None);
        settings.claude_dirs = vec![
            ClaudeDir { name: "Personal".into(), path: "~/.claude".into(), command: "claude".into(), args: vec![] },
            ClaudeDir { name: "Work".into(), path: "~/.claude-work".into(), command: "claude-w".into(), args: vec!["--model".into(), "opus".into()] },
        ];
        settings.default_claude_dir = Some("Work".into());
        settings.folder_claude_dirs.insert("/home/me/work/ops".into(), "Work".into());
        settings.shell = ShellSettings { program: "/bin/zsh".into(), args: vec!["-l".into()] };
        settings.recents = RecentsSettings { per_project: 9, chips: 2 };
        settings.favorites = vec!["/a".into(), "/b".into()];
        settings.window = WindowSettings { restore_layout: false, sidebar_at_start: SidebarAtStart::Hidden, scrollback: 50000, right_to_left: false };
        settings
    }

    #[test]
    fn phase2_defaults() {
        let s = Settings::default();
        assert!(s.shortcuts.is_empty() && s.claude_dirs.is_empty() && s.folder_claude_dirs.is_empty() && s.favorites.is_empty());
        assert_eq!(s.default_claude_dir, None);
        assert_eq!(s.shell, ShellSettings { program: String::new(), args: vec![] });
        assert_eq!(s.recents, RecentsSettings { per_project: 5, chips: 6 });
        assert_eq!(s.window, WindowSettings { restore_layout: true, sidebar_at_start: SidebarAtStart::Remember, scrollback: 10000, right_to_left: true });
    }

    #[test]
    fn sidebar_at_start_overrides_the_saved_state_only_when_asked() {
        for saved in [true, false] {
            assert_eq!(SidebarAtStart::Remember.visible(saved), saved);
            assert!(SidebarAtStart::Shown.visible(saved));
            assert!(!SidebarAtStart::Hidden.visible(saved));
        }
    }

    #[test]
    fn phase2_defaults_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &Settings::default(), &Map::new()).unwrap();
        assert_eq!(read_json(&path), serde_json::json!({}));
    }

    #[test]
    fn phase2_round_trip() {
        let settings = phase2_non_default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &settings, &Map::new()).unwrap();
        let loaded = load(&path);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings, Some(settings));
    }

    #[test]
    fn null_shortcut_survives_round_trip() {
        let loaded = parse(r#"{"shortcuts": {"splitRightClaude": null, "futureAction": "ctrl-x"}}"#);
        assert!(loaded.problems.is_empty());
        let settings = loaded.settings.unwrap();
        assert_eq!(settings.shortcuts.get("splitRightClaude"), Some(&None));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &settings, &loaded.raw).unwrap();
        assert_eq!(read_json(&path), serde_json::json!({"shortcuts": {"splitRightClaude": null, "futureAction": "ctrl-x"}}));
    }

    #[test]
    fn claude_dirs_drop_duplicates_with_problem() {
        let loaded = parse(
            r#"{"claudeDirs": [
                {"name": "A", "path": "/a"},
                {"name": "A", "path": "/b"},
                {"name": "B", "path": "/a"},
                "nope",
                {"name": "", "path": "/c"},
                {"name": "C", "path": "/c", "command": "x", "args": ["-y"]}
            ]}"#,
        );
        let dirs = loaded.settings.unwrap().claude_dirs;
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0], ClaudeDir { name: "A".into(), path: "/a".into(), command: "claude".into(), args: vec![] });
        assert_eq!(dirs[1].args, vec!["-y".to_string()]);
        let keys: Vec<_> = loaded.problems.iter().map(|problem| problem.key.as_str()).collect();
        assert_eq!(keys, ["claudeDirs[1]", "claudeDirs[2]", "claudeDirs[3]", "claudeDirs[4]"]);
    }

    #[test]
    fn folder_memory_for_unknown_dir_is_kept_with_problem() {
        let loaded = parse(
            r#"{"claudeDirs": [{"name": "A", "path": "/a"}], "defaultClaudeDir": "Zed",
                "folderClaudeDirs": {"/x": "A", "/y": "Gone"}}"#,
        );
        let settings = loaded.settings.unwrap();
        assert_eq!(settings.folder_claude_dirs.len(), 2);
        assert_eq!(settings.folder_claude_dirs.get(Path::new("/y")).map(String::as_str), Some("Gone"));
        let keys: Vec<_> = loaded.problems.iter().map(|problem| problem.key.as_str()).collect();
        assert_eq!(keys, ["defaultClaudeDir", "folderClaudeDirs./y"]);
        assert_eq!(settings.default_claude_dir.as_deref(), Some("Zed"));
    }

    #[test]
    fn ranges_clamp() {
        let loaded = parse(r#"{"recents": {"perProject": 99, "chips": -3}, "window": {"scrollback": 5, "sidebarAtStart": "sideways"}}"#);
        let settings = loaded.settings.unwrap();
        assert_eq!(settings.recents, RecentsSettings { per_project: 20, chips: 0 });
        assert_eq!(settings.window.scrollback, 1000);
        assert_eq!(settings.window.sidebar_at_start, SidebarAtStart::Remember);
        let keys: Vec<_> = loaded.problems.iter().map(|problem| problem.key.as_str()).collect();
        assert_eq!(keys, ["recents.perProject", "recents.chips", "window.sidebarAtStart", "window.scrollback"]);
        assert_eq!(parse(r#"{"window": {"scrollback": 1000000}}"#).settings.unwrap().window.scrollback, 100000);
    }

    #[test]
    fn expand_home_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(expand_home(Path::new("~/x"), Some(home)), PathBuf::from("/home/me/x"));
        assert_eq!(expand_home(Path::new("~"), Some(home)), PathBuf::from("/home/me"));
        assert_eq!(expand_home(Path::new("/abs/x"), Some(home)), PathBuf::from("/abs/x"));
        assert_eq!(expand_home(Path::new("~other/x"), Some(home)), PathBuf::from("~other/x"));
        assert_eq!(expand_home(Path::new("~/x"), None), PathBuf::from("~/x"));
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("#C1E5CD"), Some(0xc1e5cd));
        assert_eq!(hex(0xc1e5cd), "#c1e5cd");
        assert_eq!(parse_hex("c1e5cd"), None);
        assert_eq!(parse_hex("#c1e5c"), None);
    }
}
