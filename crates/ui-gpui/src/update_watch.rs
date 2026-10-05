//! The installed-version check: what the daemon runs, the binary this window started from, and the footer lines.

use gpui::Global;

/// The connected daemon's build, as its `Welcome` said.
#[derive(Clone, Debug, Default)]
pub(crate) struct DaemonBuild {
    /// App version, if the daemon sent one.
    pub version: Option<String>,
    /// Git commit, if the daemon sent one.
    pub commit: Option<String>,
}

impl Global for DaemonBuild {}

/// The Settings footer: this window's build, then the daemon's.
pub(crate) fn footer_lines(daemon: Option<&DaemonBuild>) -> [String; 2] {
    let app = format!("Gantry {} ({})", protocol::build::VERSION, protocol::build::COMMIT);
    let daemon = match daemon.and_then(|daemon| daemon.version.as_deref().map(|version| (version, daemon))) {
        Some((version, daemon)) => format!("daemon {version} ({})", daemon.commit.as_deref().unwrap_or("unknown")),
        None => "daemon version unknown".into(),
    };
    [app, daemon]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_names_both_versions() {
        let daemon = DaemonBuild { version: Some("0.2.0".into()), commit: Some("fc3d94a".into()) };
        let [app, daemon_line] = footer_lines(Some(&daemon));
        assert_eq!(app, format!("Gantry {} ({})", protocol::build::VERSION, protocol::build::COMMIT));
        assert_eq!(daemon_line, "daemon 0.2.0 (fc3d94a)");
    }

    #[test]
    fn footer_without_a_daemon_version() {
        let none = DaemonBuild { version: None, commit: None };
        assert_eq!(footer_lines(Some(&none))[1], "daemon version unknown");
        assert_eq!(footer_lines(None)[1], "daemon version unknown");
        let no_commit = DaemonBuild { version: Some("0.3.0".into()), commit: None };
        assert_eq!(footer_lines(Some(&no_commit))[1], "daemon 0.3.0 (unknown)");
    }
}
