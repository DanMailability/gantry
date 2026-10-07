//! App versions (`MAJOR.MINOR.PATCH`) and what the update notice says: whether a newer Gantry is installed or the
//! sessions still run an older one, and whether restarting closes them.

use std::fmt;

/// A `MAJOR.MINOR.PATCH` version; ordered numerically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// Major number.
    pub major: u64,
    /// Minor number.
    pub minor: u64,
    /// Patch number.
    pub patch: u64,
}

impl Version {
    /// Exactly three dot-separated numbers (surrounding whitespace ignored); anything else is `None`.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.trim().split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = Version { major: next()?, minor: next()?, patch: next()? };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The version in `gantry --version` output (`gantry 0.3.0 (a1b2c3d)`).
pub fn parse_version_output(output: &str) -> Option<Version> {
    Version::parse(output.trim().strip_prefix("gantry ")?.split_whitespace().next()?)
}

/// What is out of date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// A newer Gantry is installed than this window runs.
    WindowOutdated {
        /// This window's version.
        running: Version,
    },
    /// The window is current; the sessions run an older version (`None`: the daemon did not say which).
    SessionsOutdated {
        /// The daemon's version.
        daemon: Option<Version>,
    },
}

/// What restarting does to the running sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    /// Closes this many live panes, then resumes them.
    ClosesSessions(usize),
    /// Leaves them running.
    KeepsSessions,
}

/// The update notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Update {
    /// The version a restart runs.
    pub target: Version,
    /// What is out of date.
    pub detail: Detail,
    /// Whether the daemon must restart too (Stop all first).
    pub restart_daemon: bool,
    /// What happens to the sessions.
    pub note: Note,
}

/// The notice for a window running `running`, a daemon running `daemon` and an installed binary `installed`, with
/// `live_panes` panes running; `None` when nothing is older than the newest of them.
pub fn update(
    running: Version,
    daemon: Option<Version>,
    installed: Option<Version>,
    live_panes: usize,
) -> Option<Update> {
    let target = installed.map_or(running, |installed| installed.max(running));
    let window_outdated = target > running;
    let restart_daemon = daemon.is_none_or(|daemon| daemon < target);
    if !window_outdated && !restart_daemon {
        return None;
    }
    let detail = if window_outdated { Detail::WindowOutdated { running } } else { Detail::SessionsOutdated { daemon } };
    let note = if restart_daemon && live_panes > 0 { Note::ClosesSessions(live_panes) } else { Note::KeepsSessions };
    Some(Update { target, detail, restart_daemon, note })
}

impl Update {
    /// The title-bar pill.
    pub fn pill(&self) -> String {
        format!("↑ Update to {}", self.target)
    }

    /// The card's title.
    pub fn title(&self) -> String {
        format!("Restart to finish updating to {}", self.target)
    }

    /// The line under the title.
    pub fn detail_text(&self) -> String {
        match self.detail {
            Detail::WindowOutdated { running } => {
                format!("Gantry {} is installed. This window runs {running}.", self.target)
            }
            Detail::SessionsOutdated { daemon: Some(daemon) } => format!("Your running sessions still use {daemon}."),
            Detail::SessionsOutdated { daemon: None } => "Your running sessions use an older version.".into(),
        }
    }

    /// The note above the button.
    pub fn note_text(&self) -> String {
        match self.note {
            Note::ClosesSessions(count) => format!(
                "Restarting closes your {count} running session{} and then resumes them. Shells start fresh in the \
                 same folders.",
                if count == 1 { "" } else { "s" }
            ),
            Note::KeepsSessions => "Your running sessions stay open.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_three_numbers_only() {
        assert_eq!(v(" 0.10.2\n"), Version { major: 0, minor: 10, patch: 2 });
        for bad in ["", "0.3", "0.3.0.1", "0.3.0-pre", "a.b.c", "0.3.x"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(v("0.3.0").to_string(), "0.3.0");
    }

    #[test]
    fn orders_numerically() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.3.1") > v("0.3.0"));
    }

    #[test]
    fn reads_version_output() {
        assert_eq!(parse_version_output("gantry 0.3.0 (a1b2c3d)\n"), Some(v("0.3.0")));
        assert_eq!(parse_version_output("gantry 0.3.0 (unknown)"), Some(v("0.3.0")));
        for bad in ["", "gantry", "other 0.3.0 (x)", "gantry 0.3 (x)", "\u{7f}ELF garbage"] {
            assert_eq!(parse_version_output(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn nothing_when_everything_is_current() {
        assert_eq!(update(v("0.3.0"), Some(v("0.3.0")), None, 2), None);
        assert_eq!(update(v("0.3.0"), Some(v("0.3.0")), Some(v("0.3.0")), 2), None);
    }

    #[test]
    fn downgrade_or_daemon_newer_shows_nothing() {
        assert_eq!(update(v("0.3.0"), Some(v("0.3.0")), Some(v("0.2.0")), 2), None);
        assert_eq!(update(v("0.3.0"), Some(v("0.4.0")), None, 2), None);
    }

    #[test]
    fn installed_newer_with_sessions_closes_them() {
        let up = update(v("0.2.0"), Some(v("0.2.0")), Some(v("0.3.0")), 3).unwrap();
        assert_eq!(up.target, v("0.3.0"));
        assert_eq!(up.detail, Detail::WindowOutdated { running: v("0.2.0") });
        assert!(up.restart_daemon);
        assert_eq!(up.note, Note::ClosesSessions(3));
        assert_eq!(up.pill(), "↑ Update to 0.3.0");
        assert_eq!(up.title(), "Restart to finish updating to 0.3.0");
        assert_eq!(up.detail_text(), "Gantry 0.3.0 is installed. This window runs 0.2.0.");
        assert_eq!(
            up.note_text(),
            "Restarting closes your 3 running sessions and then resumes them. Shells start fresh in the same folders."
        );
    }

    #[test]
    fn one_session_is_singular() {
        let up = update(v("0.2.0"), Some(v("0.2.0")), Some(v("0.3.0")), 1).unwrap();
        assert!(up.note_text().starts_with("Restarting closes your 1 running session and"));
    }

    #[test]
    fn installed_newer_without_sessions_keeps_them() {
        let up = update(v("0.2.0"), Some(v("0.2.0")), Some(v("0.3.0")), 0).unwrap();
        assert!(up.restart_daemon);
        assert_eq!(up.note, Note::KeepsSessions);
        assert_eq!(up.note_text(), "Your running sessions stay open.");
    }

    #[test]
    fn daemon_already_current_keeps_sessions() {
        let up = update(v("0.2.0"), Some(v("0.3.0")), Some(v("0.3.0")), 4).unwrap();
        assert!(!up.restart_daemon);
        assert_eq!(up.note, Note::KeepsSessions);
    }

    #[test]
    fn only_the_daemon_is_older() {
        let up = update(v("0.3.0"), Some(v("0.2.0")), None, 2).unwrap();
        assert_eq!(up.detail, Detail::SessionsOutdated { daemon: Some(v("0.2.0")) });
        assert_eq!(up.detail_text(), "Your running sessions still use 0.2.0.");
        assert!(up.restart_daemon);
        assert_eq!(up.note, Note::ClosesSessions(2));
    }

    #[test]
    fn a_daemon_without_a_version_is_older() {
        let up = update(v("0.3.0"), None, None, 2).unwrap();
        assert_eq!(up.detail, Detail::SessionsOutdated { daemon: None });
        assert_eq!(up.detail_text(), "Your running sessions use an older version.");
        assert!(up.restart_daemon);
    }
}
