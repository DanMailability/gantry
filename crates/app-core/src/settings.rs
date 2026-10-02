//! The settings file (`settings.json`): typed model, tolerant loading, and writing only changed keys.

use std::io;
use std::path::Path;

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

/// Everything in settings.json (phase 1).
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

        Settings { theme, status_fills, terminal_font, ui_font_size, alerts }
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
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &settings, &Map::new()).unwrap();
        let loaded = load(&path);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings, Some(settings));
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("#C1E5CD"), Some(0xc1e5cd));
        assert_eq!(hex(0xc1e5cd), "#c1e5cd");
        assert_eq!(parse_hex("c1e5cd"), None);
        assert_eq!(parse_hex("#c1e5c"), None);
    }
}
