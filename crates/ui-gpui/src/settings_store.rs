//! The applied settings (a GPUI global): loading `settings.json`, watching it, saving changes, and rebuilding
//! the [`Theme`] from them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use app_core::settings::{self, Loaded, Problem, Settings};
use app_core::theme_colors::{readable_fill, resolve, tokens};
use gpui::{App, Global, Task, WindowAppearance};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Map, Value};

use crate::theme::Theme;

/// Interface font size that draws at scale 1.0.
const BASE_UI_FONT_SIZE: f32 = 14.0;
/// Quiet time after the last file event before the file is read.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// What to do with a freshly read settings file.
#[derive(Debug, PartialEq)]
enum Reload {
    /// Replace the applied settings.
    Apply(Settings),
    /// Leave them alone (unchanged, or the file is not JSON).
    Keep,
}

fn reload_decision(applied: &Settings, loaded: &Loaded) -> Reload {
    match &loaded.settings {
        Some(settings) if settings != applied => Reload::Apply(settings.clone()),
        _ => Reload::Keep,
    }
}

/// The applied settings, the raw JSON they came from, what was wrong with the file, and the file watcher.
pub(crate) struct SettingsStore {
    settings: Settings,
    raw: Map<String, Value>,
    /// Problems found reading (or writing) the file.
    file_problems: Vec<Problem>,
    /// `file_problems` plus the status fills the theme cannot draw readably.
    problems: Vec<Problem>,
    path: PathBuf,
    _watcher: Option<RecommendedWatcher>,
    _reload: Option<Task<()>>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    /// Load `path` and install both globals (the theme resolved with the current system appearance).
    pub fn init(path: PathBuf, cx: &mut App) {
        let loaded = settings::load(&path);
        let (watcher, reload) = watch(&path, cx).unzip();
        cx.set_global(SettingsStore {
            settings: loaded.settings.unwrap_or_default(),
            raw: loaded.raw,
            file_problems: loaded.problems,
            problems: Vec::new(),
            path,
            _watcher: watcher,
            _reload: reload,
        });
        apply(cx);
    }

    /// The applied settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Everything wrong with the file or the settings, for the Settings tab's banner.
    #[allow(dead_code)] // read by the Settings tab
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Where the settings are stored.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Change the settings from the UI: apply, save (skipped if unchanged), refresh every window.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        let store = cx.global_mut::<SettingsStore>();
        let mut new = store.settings.clone();
        change(&mut new);
        if new == store.settings {
            return;
        }
        if let Err(error) = settings::save(&store.path, &new, &store.raw) {
            store.file_problems.push(Problem { key: String::new(), message: format!("could not save: {error}") });
        }
        store.settings = new;
        apply(cx);
    }

    /// Re-resolve the theme (the system appearance changed).
    pub fn reapply(cx: &mut App) {
        apply(cx);
    }

    fn reload(cx: &mut App) {
        let store = cx.global_mut::<SettingsStore>();
        let loaded = settings::load(&store.path);
        let decision = reload_decision(&store.settings, &loaded);
        // A broken file reads as an empty object: keep the last good one so a later save keeps unknown keys.
        if loaded.settings.is_some() {
            store.raw = loaded.raw;
        }
        store.file_problems = loaded.problems;
        if let Reload::Apply(settings) = decision {
            store.settings = settings;
        }
        apply(cx);
    }
}

/// Rebuild the [`Theme`] from the settings and redraw everything. Each window follows through its observer.
fn apply(cx: &mut App) {
    let dark = matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let store = cx.global_mut::<SettingsStore>();
    let settings = &store.settings;
    let preset = resolve(settings.theme, dark);
    let tokens = tokens(preset);
    let mut fills = settings.status_fills;
    let mut problems = store.file_problems.clone();
    for (name, fill) in [
        ("needsInput", &mut fills.needs_input),
        ("working", &mut fills.working),
        ("done", &mut fills.done),
        ("idle", &mut fills.idle),
    ] {
        if fill.is_some_and(|color| !readable_fill(color, &tokens)) {
            *fill = None;
            problems.push(Problem { key: format!("statusColors.{name}"), message: "too low contrast".into() });
        }
    }
    let ui_scale = settings.ui_font_size / BASE_UI_FONT_SIZE;
    store.problems = problems;
    cx.set_global(Theme { preset, tokens, fills, ui_scale });
    cx.refresh_windows();
}

/// Watch the settings file's directory; a foreground task reloads after the events settle.
fn watch(path: &Path, cx: &mut App) -> Option<(RecommendedWatcher, Task<()>)> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_owned();
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::warn!("settings directory not created: {error}");
        return None;
    }
    let (events, changes) = async_channel::unbounded::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event.paths.iter().any(|path| path.file_name() == Some(name.as_os_str()))
        {
            let _ = events.try_send(());
        }
    })
    .inspect_err(|error| tracing::warn!("settings file not watched: {error}"))
    .ok()?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .inspect_err(|error| tracing::warn!("settings file not watched: {error}"))
        .ok()?;
    let task = cx.spawn(async move |cx| {
        while changes.recv().await.is_ok() {
            cx.background_executor().timer(DEBOUNCE).await;
            while changes.try_recv().is_ok() {}
            cx.update(SettingsStore::reload);
        }
    });
    Some((watcher, task))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(settings: Option<Settings>, problems: Vec<Problem>) -> Loaded {
        Loaded { settings, problems, raw: Map::new() }
    }

    #[test]
    fn own_write_is_not_a_change() {
        let applied = Settings::default();
        assert_eq!(reload_decision(&applied, &loaded(Some(applied.clone()), Vec::new())), Reload::Keep);
    }

    #[test]
    fn broken_file_keeps_applied() {
        let problem = Problem { key: String::new(), message: "not JSON".into() };
        let broken = loaded(None, vec![problem]);
        assert_eq!(reload_decision(&Settings::default(), &broken), Reload::Keep);
        assert_eq!(broken.problems.len(), 1);
    }

    #[test]
    fn changed_file_applies() {
        let mut changed = Settings::default();
        changed.terminal_font.size = 18.0;
        assert_eq!(reload_decision(&Settings::default(), &loaded(Some(changed.clone()), Vec::new())), Reload::Apply(changed));
    }
}
