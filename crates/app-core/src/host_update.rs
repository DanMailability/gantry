//! When an SSH host's daemon is older than this Gantry and what the sidebar's update click does.

use crate::host_state::HostState;
use crate::version::Version;

/// Whether a host's row shows the update pill: connected, and its daemon reports a version below `local`.
pub fn needs_update(state: &HostState, remote: Option<Version>, local: Version) -> bool {
    matches!(state, HostState::Connected) && remote.is_some_and(|remote| remote < local)
}

/// What a click on the pill does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Nothing runs on the host: update at once.
    Start,
    /// Ask first: restarting ends this many running sessions.
    Ask {
        /// Running panes on the host.
        running: usize,
    },
}

/// The click's plan for a host with `running` running panes.
pub fn click_plan(running: usize) -> Click {
    if running == 0 { Click::Start } else { Click::Ask { running } }
}

/// The confirmation card's amber line.
pub fn ask_note(host: &str, running: usize) -> String {
    if running == 1 {
        format!("Restarting ends the 1 session running on {host}. Its pane shows Continue afterwards.")
    } else {
        format!("Restarting ends the {running} sessions running on {host}. Their panes show Continue afterwards.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(minor: u64) -> Version {
        Version { major: 0, minor, patch: 0 }
    }

    #[test]
    fn pill_only_when_connected_and_older() {
        assert!(needs_update(&HostState::Connected, Some(v(8)), v(9)));
        assert!(!needs_update(&HostState::Connected, Some(v(9)), v(9)));
        assert!(!needs_update(&HostState::Connected, Some(v(10)), v(9)));
        assert!(!needs_update(&HostState::Connected, None, v(9)));
        let others = [
            HostState::NotConnected,
            HostState::Connecting,
            HostState::Reconnecting { attempt: 1 },
            HostState::Disconnected,
            HostState::NeedsSignIn,
            HostState::UpdateNeeded { remote: None },
            HostState::NotInstalled,
            HostState::Failed { reason: String::new() },
            HostState::TakenOver,
        ];
        for state in &others {
            assert!(!needs_update(state, Some(v(8)), v(9)), "{state:?}");
        }
    }

    #[test]
    fn click_plan_starts_at_once_with_no_sessions() {
        assert_eq!(click_plan(0), Click::Start);
        assert_eq!(click_plan(3), Click::Ask { running: 3 });
    }

    #[test]
    fn note_counts_sessions() {
        assert!(ask_note("box", 1).contains("the 1 session running on box"));
        assert!(ask_note("box", 1).contains("Its pane shows"));
        assert!(ask_note("box", 2).contains("the 2 sessions running on box"));
        assert!(ask_note("box", 2).contains("Their panes show"));
    }
}
