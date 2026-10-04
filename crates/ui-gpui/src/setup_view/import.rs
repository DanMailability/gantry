//! What Import writes: the ticked rows appended to `claudeDirs`, the default, and `setupDone`; plus which dirs are
//! already listed. Pure, so it is tested without GPUI.

use std::path::{Path, PathBuf};

use app_core::session_tree::display_path;
use app_core::settings::{ClaudeDir, Settings, expand_home};
use app_core::workspace::ClaudeDirs;
use claude_index::discovery::unique_name;

use crate::settings_view::sessions::materialize;

/// Whether the wizard opened by itself (the first start) or from Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// First start: shows the Default choice, and Skip or Import set `setupDone`.
    FirstRun,
    /// From Settings > Sessions: only adds dirs; the default and `setupDone` stay as they are.
    Rerun,
}

/// One ticked row of the wizard.
pub(super) struct Row {
    /// The config dir, as found.
    pub path: PathBuf,
    /// Its name (trimmed on import).
    pub name: String,
    /// The program that runs Claude there.
    pub command: String,
    /// Chosen as the default dir (first run only).
    pub default: bool,
}

/// Add the ticked `rows` to `claudeDirs` and, on the first run, set the default and `setupDone`. No rows = Skip.
/// A re-run with no dirs configured yet writes the implicit Default first, so it is not lost.
pub(super) fn apply_import(
    settings: &mut Settings,
    rows: &[Row],
    mode: Mode,
    daemon_dir: Option<&Path>,
    home: Option<&Path>,
) {
    if mode == Mode::Rerun && !rows.is_empty() {
        materialize(settings, daemon_dir, home);
    }
    let mut default = None;
    for row in rows {
        let path = PathBuf::from(display_path(&row.path, home));
        let configured: Vec<PathBuf> = settings.claude_dirs.iter().map(|dir| expand_home(&dir.path, home)).collect();
        if is_listed(&configured, &row.path) {
            continue;
        }
        let name = unique_name(row.name.trim(), &|name| settings.claude_dirs.iter().any(|dir| dir.name == name));
        if row.default {
            default = Some(name.clone());
        }
        settings.claude_dirs.push(ClaudeDir {
            name,
            path,
            command: row.command.trim().to_string(),
            args: Vec::new(),
            color: None,
        });
    }
    if mode == Mode::FirstRun {
        if let Some(name) = default {
            let first = settings.claude_dirs.first().map(|dir| dir.name.as_str());
            settings.default_claude_dir = (first != Some(name.as_str())).then_some(name);
        }
        settings.setup_done = true;
    }
}

/// The config dirs already in use, expanded: none on the first run (everything is offered), else the configured dirs or
/// the implicit Default.
pub(super) fn listed_paths(
    settings: &Settings,
    mode: Mode,
    daemon_dir: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    if mode == Mode::FirstRun {
        return Vec::new();
    }
    ClaudeDirs::from_settings(settings, daemon_dir, home).dirs.into_iter().map(|dir| dir.path).collect()
}

/// Whether `path` is one of `listed`, also when they reach the same folder through a symlink.
pub(super) fn is_listed(listed: &[PathBuf], path: &Path) -> bool {
    let real = path.canonicalize().ok();
    listed.iter().any(|other| {
        other == path || real.as_ref().is_some_and(|real| other.canonicalize().ok().as_ref() == Some(real))
    })
}

/// Whether the wizard opens by itself at start: no dirs configured, not answered before, and a `settings.json` it can
/// save to.
pub(crate) fn opens_by_itself(settings: &Settings, broken: bool) -> bool {
    settings.claude_dirs.is_empty() && !settings.setup_done && !broken
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::settings::ClaudeDir;

    const HOME: &str = "/home/me";

    fn home() -> Option<&'static Path> {
        Some(Path::new(HOME))
    }

    fn row(path: &str, name: &str, default: bool) -> Row {
        Row { path: path.into(), name: name.into(), command: "claude".into(), default }
    }

    fn names(settings: &Settings) -> Vec<&str> {
        settings.claude_dirs.iter().map(|dir| dir.name.as_str()).collect()
    }

    #[test]
    fn first_run_imports_the_rows_the_default_and_setup_done() {
        let mut settings = Settings::default();
        let rows = [row("/home/me/.claude", "Personal", false), row("/home/me/.claude-work", "Work", true)];
        apply_import(&mut settings, &rows, Mode::FirstRun, None, home());
        assert_eq!(names(&settings), ["Personal", "Work"]);
        assert_eq!(settings.claude_dirs[1].path, PathBuf::from("~/.claude-work"), "home is written as ~");
        assert_eq!(settings.claude_dirs[1].command, "claude");
        assert!(settings.claude_dirs[1].args.is_empty() && settings.claude_dirs[1].color.is_none());
        assert_eq!(settings.default_claude_dir.as_deref(), Some("Work"));
        assert!(settings.setup_done);
    }

    #[test]
    fn a_first_row_default_is_stored_as_nothing() {
        let mut settings = Settings::default();
        apply_import(&mut settings, &[row("/a", "A", true), row("/b", "B", false)], Mode::FirstRun, None, home());
        assert_eq!(settings.default_claude_dir, None);
    }

    #[test]
    fn skip_writes_only_setup_done() {
        let mut settings = Settings::default();
        apply_import(&mut settings, &[], Mode::FirstRun, None, home());
        assert!(settings.setup_done && settings.claude_dirs.is_empty() && settings.default_claude_dir.is_none());
    }

    #[test]
    fn a_rerun_appends_after_the_existing_dirs_and_leaves_the_default_and_setup_done_alone() {
        let mut settings = Settings::default();
        settings.claude_dirs.push(ClaudeDir {
            name: "Mine".into(),
            path: "~/.claude".into(),
            command: "claude".into(),
            args: vec!["--x".into()],
            color: None,
        });
        settings.default_claude_dir = Some("Mine".into());
        apply_import(&mut settings, &[row("/a", "A", true)], Mode::Rerun, None, home());
        assert_eq!(names(&settings), ["Mine", "A"]);
        assert_eq!(settings.claude_dirs[0].args, ["--x"]);
        assert_eq!(settings.default_claude_dir.as_deref(), Some("Mine"), "a re-run never moves the default");
        assert!(!settings.setup_done);
    }

    #[test]
    fn a_rerun_after_skip_keeps_the_implicit_default_first() {
        let mut settings = Settings::default();
        apply_import(&mut settings, &[row("/home/me/.claude-work", "Work", false)], Mode::Rerun, None, home());
        assert_eq!(names(&settings), ["Default", "Work"]);
        assert_eq!(settings.claude_dirs[0].path, PathBuf::from("~/.claude"));
        assert!(!settings.setup_done);
        let mut settings = Settings::default();
        apply_import(&mut settings, &[row("/x/work", "Work", false)], Mode::Rerun, Some(Path::new("/d/cfg")), home());
        assert_eq!(settings.claude_dirs[0].path, PathBuf::from("/d/cfg"), "the daemon's own dir");
    }

    #[test]
    fn equal_names_are_made_unique_and_names_are_trimmed() {
        let mut settings = Settings::default();
        let rows = [row("/a", " Work ", false), row("/b", "Work", false), row("/c", "Work", false)];
        apply_import(&mut settings, &rows, Mode::FirstRun, None, home());
        assert_eq!(names(&settings), ["Work", "Work 2", "Work 3"]);
    }

    #[test]
    fn a_path_already_listed_is_not_added_twice() {
        let mut settings = Settings::default();
        settings.claude_dirs.push(ClaudeDir {
            name: "Mine".into(),
            path: "/home/me/.claude".into(), // written absolute
            command: "claude".into(),
            args: vec![],
            color: None,
        });
        apply_import(&mut settings, &[row("/home/me/.claude", "Again", false)], Mode::Rerun, None, home());
        assert_eq!(names(&settings), ["Mine"]);
    }

    #[test]
    fn listed_paths_are_empty_on_the_first_run_and_expanded_on_a_rerun() {
        let mut settings = Settings::default();
        assert!(listed_paths(&settings, Mode::FirstRun, None, home()).is_empty());
        assert_eq!(listed_paths(&settings, Mode::Rerun, None, home()), [PathBuf::from("/home/me/.claude")]);
        assert_eq!(listed_paths(&settings, Mode::Rerun, Some(Path::new("/d/cfg")), home()), [PathBuf::from("/d/cfg")]);
        settings.claude_dirs.push(ClaudeDir {
            name: "W".into(),
            path: "~/.claude-work".into(),
            command: "claude".into(),
            args: vec![],
            color: None,
        });
        assert_eq!(listed_paths(&settings, Mode::Rerun, None, home()), [PathBuf::from("/home/me/.claude-work")]);
    }

    #[test]
    fn is_listed_compares_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        assert!(is_listed(&[dir.path().join("real")], &dir.path().join("real")));
        assert!(!is_listed(&[dir.path().join("real")], &dir.path().join("other")));
        // Symlinks: unix only.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
            assert!(is_listed(&[dir.path().join("real")], &dir.path().join("link")));
        }
    }

    #[test]
    fn the_wizard_opens_by_itself_only_on_a_fresh_valid_settings_file() {
        let mut settings = Settings::default();
        assert!(opens_by_itself(&settings, false));
        assert!(!opens_by_itself(&settings, true), "a settings.json that is not JSON can never be saved");
        settings.setup_done = true;
        assert!(!opens_by_itself(&settings, false));
        settings.setup_done = false;
        settings.claude_dirs.push(ClaudeDir {
            name: "W".into(),
            path: "/w".into(),
            command: "claude".into(),
            args: vec![],
            color: None,
        });
        assert!(!opens_by_itself(&settings, false), "users who already have dirs never see it");
    }
}
