//! The state of one SSH host's connection: what the cards say, when to retry, and how a failed attempt is read.

use std::time::Duration;

/// Retries after a dropped connection before the host shows Reconnect.
pub const RETRY_LIMIT: u8 = 4;

/// How long to wait before retry `attempt` (1 to 4): 1, 2, 5, 10 seconds. `None` once the retries are used up.
pub fn reconnect_delay(attempt: u8) -> Option<Duration> {
    let seconds = match attempt {
        1 => 1,
        2 => 2,
        3 => 5,
        4 => 10,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

/// Where one SSH host's connection stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostState {
    /// Nothing tried yet (hosts connect on first use).
    NotConnected,
    /// An interactive attempt is running.
    Connecting,
    /// The daemon is attached.
    Connected,
    /// The connection dropped; retry `attempt` (1 to 4) is waiting or running, without prompts.
    Reconnecting {
        /// Which retry.
        attempt: u8,
    },
    /// All retries failed; waits for the user's Reconnect.
    Disconnected,
    /// ssh needs a password, passphrase or host-key answer.
    NeedsSignIn,
    /// The remote daemon speaks another protocol version.
    UpdateNeeded {
        /// What the remote runs, for the card (for example `protocol 6`).
        remote: Option<String>,
    },
    /// The remote has no Gantry at the configured command.
    NotInstalled,
    /// Any other failure, with ssh's last words.
    Failed {
        /// Why.
        reason: String,
    },
    /// Another Gantry took this host over; no retry (two windows would fight), waits for the user's Reconnect.
    TakenOver,
}

/// A small status dot for a host in the New menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDot {
    /// Connected.
    Ok,
    /// Working on it (connecting, reconnecting).
    Busy,
    /// Needs the user (sign-in, update, opened elsewhere).
    Warn,
    /// Not usable (lost, not installed, failed).
    Bad,
    /// Nothing to say (not connected yet).
    None,
}

/// Why an attempt failed, as read from ssh's exit and the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailKind {
    /// ssh needed input (credentials, a host-key answer) that a background attempt cannot get, or the user cancelled.
    NeedsInput,
    /// The remote daemon speaks another protocol version.
    UpdateNeeded {
        /// What the remote runs, for the card.
        remote: Option<String>,
    },
    /// The remote command was not found.
    NotInstalled,
    /// Anything else.
    Other {
        /// Why.
        reason: String,
    },
}

/// Something that happened to a host's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// The user asked to connect (a button, the New menu, Continue on a dead leaf).
    Connect,
    /// An attempt got through the handshake.
    Established,
    /// An established connection ended.
    Dropped,
    /// An attempt failed.
    AttemptFailed(FailKind),
    /// Another Gantry took the host's daemon over while connected.
    TakenOver,
    /// The user cancelled the attempt that was connecting.
    Cancel,
}

/// The message card of a host that is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCard {
    /// Bold first line.
    pub title: String,
    /// Explanation under it.
    pub detail: String,
    /// The one button, if the user can do something about it.
    pub button: Option<&'static str>,
}

impl HostState {
    /// Whether a user's Connect starts an attempt from this state.
    pub fn can_connect(&self) -> bool {
        !matches!(self, HostState::Connecting | HostState::Connected | HostState::Reconnecting { .. })
    }

    /// The state after `event`; an event that does not apply leaves the state as it is.
    pub fn next(&self, event: HostEvent) -> HostState {
        match (self, event) {
            (state, HostEvent::Connect) if state.can_connect() => HostState::Connecting,
            (HostState::Connecting | HostState::Reconnecting { .. }, HostEvent::Established) => HostState::Connected,
            (HostState::Connected, HostEvent::Dropped) => HostState::Reconnecting { attempt: 1 },
            (HostState::Connected, HostEvent::TakenOver) => HostState::TakenOver,
            (HostState::Connecting, HostEvent::Cancel) => HostState::NotConnected,
            (HostState::Connecting | HostState::Reconnecting { .. }, HostEvent::AttemptFailed(kind)) => match kind {
                FailKind::NeedsInput => HostState::NeedsSignIn,
                FailKind::UpdateNeeded { remote } => HostState::UpdateNeeded { remote },
                FailKind::NotInstalled => HostState::NotInstalled,
                FailKind::Other { reason } => match self {
                    HostState::Reconnecting { attempt } if *attempt < RETRY_LIMIT => {
                        HostState::Reconnecting { attempt: attempt + 1 }
                    }
                    HostState::Reconnecting { .. } => HostState::Disconnected,
                    _ => HostState::Failed { reason },
                },
            },
            (state, _) => state.clone(),
        }
    }

    /// The short state shown beside the host's name in the New menu.
    pub fn label(&self) -> String {
        match self {
            HostState::NotConnected => "not connected".into(),
            HostState::Connecting => "connecting…".into(),
            HostState::Connected => "connected".into(),
            HostState::Reconnecting { attempt } => format!("reconnecting {attempt}/{RETRY_LIMIT}"),
            HostState::Disconnected => "disconnected".into(),
            HostState::NeedsSignIn => "needs sign-in".into(),
            HostState::UpdateNeeded { .. } => "update needed".into(),
            HostState::NotInstalled => "not installed".into(),
            HostState::Failed { .. } => "failed".into(),
            HostState::TakenOver => "opened elsewhere".into(),
        }
    }

    /// The dot shown beside the host's name in the New menu.
    pub fn dot(&self) -> HostDot {
        match self {
            HostState::Connected => HostDot::Ok,
            HostState::Connecting | HostState::Reconnecting { .. } => HostDot::Busy,
            HostState::NeedsSignIn | HostState::UpdateNeeded { .. } | HostState::TakenOver => HostDot::Warn,
            HostState::Disconnected | HostState::NotInstalled | HostState::Failed { .. } => HostDot::Bad,
            HostState::NotConnected => HostDot::None,
        }
    }

    /// The card the panes of host `host` show in this state; `None` when connected.
    pub fn card(&self, host: &str) -> Option<HostCard> {
        let card = |title: String, detail: String, button| Some(HostCard { title, detail, button });
        match self {
            HostState::Connected => None,
            HostState::NotConnected => {
                card("Not connected".into(), format!("{host} is not connected yet."), Some("Connect"))
            }
            HostState::Connecting => card(
                format!("Connecting to {host}…"),
                "Starting ssh. A sign-in dialog opens here if it asks for anything.".into(),
                Some("Cancel"),
            ),
            HostState::Reconnecting { attempt } => card(
                format!("Reconnecting to {host}…"),
                format!("Attempt {attempt} of {RETRY_LIMIT}. Your sessions keep running there."),
                None,
            ),
            HostState::Disconnected => card(
                "Connection lost".into(),
                format!("{host} did not answer after {RETRY_LIMIT} tries. Your sessions keep running there."),
                Some("Reconnect"),
            ),
            HostState::NeedsSignIn => card(
                format!("Sign in to {host}"),
                format!("{host} needs a password or a key passphrase."),
                Some("Sign in"),
            ),
            HostState::UpdateNeeded { remote } => card(
                format!("Update Gantry on {host}"),
                match remote {
                    Some(remote) => {
                        format!("{host} runs a different Gantry ({remote}). Update it from this Gantry.")
                    }
                    None => {
                        format!("{host} runs a different Gantry version. Update it from this Gantry.")
                    }
                },
                Some("Update and restart"),
            ),
            HostState::NotInstalled => card(
                format!("Gantry is not installed on {host}"),
                "Gantry was not found there. Install it from this Gantry.".into(),
                Some("Install…"),
            ),
            HostState::Failed { reason } => {
                card(format!("Could not connect to {host}"), reason.clone(), Some("Reconnect"))
            }
            HostState::TakenOver => card(
                "Opened from another Gantry".into(),
                format!("Another Gantry took over {host}. Your sessions keep running there."),
                Some("Reconnect"),
            ),
        }
    }
}

/// Read a failed attempt. `exit_code`: ssh's exit status when it ended by itself; `stderr`: what it printed;
/// `version_mismatch`: the remote daemon's protocol version when the handshake said it differs;
/// `interactive_unasked`: the attempt was interactive and ssh never asked a question (so "Permission denied" is not
/// something a dialog can fix); `handshake_error`: why the handshake failed, when it did.
pub fn classify_failure(
    exit_code: Option<i32>,
    stderr: &str,
    version_mismatch: Option<u32>,
    interactive_unasked: bool,
    handshake_error: Option<&str>,
) -> FailKind {
    if let Some(protocol) = version_mismatch {
        return FailKind::UpdateNeeded { remote: Some(format!("protocol {protocol}")) };
    }
    let lower = stderr.to_lowercase();
    if exit_code == Some(127) {
        return FailKind::NotInstalled;
    }
    if lower.contains("remote host identification has changed") {
        return FailKind::Other { reason: "The host key of the host changed. Check ~/.ssh/known_hosts.".into() };
    }
    let last_line = stderr.lines().rev().find(|line| !line.trim().is_empty()).map(|line| line.trim().to_string());
    let wants_input = ["permission denied", "host key verification failed", "passphrase", "authentication failed"];
    if exit_code == Some(255) && wants_input.iter().any(|needle| lower.contains(needle)) {
        if interactive_unasked && lower.contains("permission denied") {
            let line = last_line.unwrap_or_default();
            return FailKind::Other { reason: format!("Authentication failed (ssh asked for nothing): {line}") };
        }
        return FailKind::NeedsInput;
    }
    // A plain end of stream says nothing: the exit code does better. Only another handshake error is named.
    let handshake_error = handshake_error.filter(|error| *error != "daemon closed the connection");
    let reason = match (last_line, handshake_error, exit_code) {
        (Some(line), _, _) => line,
        (None, Some(error), _) => error.to_string(),
        (None, None, Some(code)) => format!("ssh ended with exit code {code}"),
        (None, None, None) => "the connection closed".to_string(),
    };
    FailKind::Other { reason }
}

/// How a prompt from ssh is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A password or passphrase: the field shows bullets.
    Masked,
    /// A host-key question: two buttons.
    YesNo,
    /// Anything else: a plain text field.
    Plain,
}

/// Which kind of prompt `prompt` is.
pub fn prompt_kind(prompt: &str) -> PromptKind {
    let lower = prompt.to_lowercase();
    if lower.contains("(yes/no") {
        PromptKind::YesNo
    } else if lower.contains("password") || lower.contains("passphrase") {
        PromptKind::Masked
    } else {
        PromptKind::Plain
    }
}

/// The dialog title for `prompt` asked on behalf of `host`.
pub fn prompt_title(host: &str, prompt: &str) -> String {
    let lower = prompt.to_lowercase();
    match prompt_kind(prompt) {
        PromptKind::YesNo => format!("Trust {host}?"),
        PromptKind::Masked if lower.contains("passphrase") => format!("Key passphrase for {host}"),
        PromptKind::Masked => format!("Password for {host}"),
        PromptKind::Plain => format!("Sign in to {host}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn other(reason: &str) -> HostEvent {
        HostEvent::AttemptFailed(FailKind::Other { reason: reason.into() })
    }

    #[test]
    fn the_retry_schedule_is_1_2_5_10_seconds_then_nothing() {
        let delays: Vec<_> = (1..=4).map(|attempt| reconnect_delay(attempt).unwrap().as_secs()).collect();
        assert_eq!(delays, [1, 2, 5, 10]);
        assert_eq!(reconnect_delay(0), None);
        assert_eq!(reconnect_delay(5), None);
    }

    #[test]
    fn connect_starts_from_every_state_that_can_connect_and_from_no_other() {
        for state in [
            HostState::NotConnected,
            HostState::Disconnected,
            HostState::NeedsSignIn,
            HostState::UpdateNeeded { remote: None },
            HostState::NotInstalled,
            HostState::Failed { reason: "x".into() },
            HostState::TakenOver,
        ] {
            assert!(state.can_connect(), "{state:?}");
            assert_eq!(state.next(HostEvent::Connect), HostState::Connecting, "{state:?}");
        }
        for state in [HostState::Connecting, HostState::Connected, HostState::Reconnecting { attempt: 2 }] {
            assert!(!state.can_connect(), "{state:?}");
            assert_eq!(state.next(HostEvent::Connect), state, "{state:?}");
        }
    }

    #[test]
    fn an_interactive_attempt_ends_in_connected_or_a_reason() {
        let connecting = HostState::Connecting;
        assert_eq!(connecting.next(HostEvent::Established), HostState::Connected);
        assert_eq!(connecting.next(HostEvent::AttemptFailed(FailKind::NeedsInput)), HostState::NeedsSignIn);
        assert_eq!(
            connecting.next(HostEvent::AttemptFailed(FailKind::UpdateNeeded { remote: Some("protocol 6".into()) })),
            HostState::UpdateNeeded { remote: Some("protocol 6".into()) }
        );
        assert_eq!(connecting.next(HostEvent::AttemptFailed(FailKind::NotInstalled)), HostState::NotInstalled);
        assert_eq!(connecting.next(other("refused")), HostState::Failed { reason: "refused".into() });
    }

    #[test]
    fn a_dropped_connection_retries_four_times_then_disconnects() {
        let mut state = HostState::Connected.next(HostEvent::Dropped);
        assert_eq!(state, HostState::Reconnecting { attempt: 1 });
        for attempt in 2..=4 {
            state = state.next(other("timeout"));
            assert_eq!(state, HostState::Reconnecting { attempt });
        }
        assert_eq!(state.next(other("timeout")), HostState::Disconnected);
    }

    #[test]
    fn a_retry_that_needs_input_never_loops_it_asks_for_sign_in_at_once() {
        let retrying = HostState::Reconnecting { attempt: 2 };
        assert_eq!(retrying.next(HostEvent::AttemptFailed(FailKind::NeedsInput)), HostState::NeedsSignIn);
        assert_eq!(retrying.next(HostEvent::AttemptFailed(FailKind::NotInstalled)), HostState::NotInstalled);
        assert_eq!(retrying.next(HostEvent::Established), HostState::Connected);
    }

    #[test]
    fn events_that_do_not_apply_change_nothing() {
        assert_eq!(HostState::NotConnected.next(HostEvent::Dropped), HostState::NotConnected);
        assert_eq!(HostState::Disconnected.next(HostEvent::Established), HostState::Disconnected);
        assert_eq!(HostState::Connected.next(other("x")), HostState::Connected);
    }

    #[test]
    fn menu_labels_and_cards_name_the_state() {
        assert_eq!(HostState::NotConnected.label(), "not connected");
        assert_eq!(HostState::Reconnecting { attempt: 2 }.label(), "reconnecting 2/4");
        assert_eq!(HostState::NeedsSignIn.label(), "needs sign-in");
        assert!(HostState::Connected.card("devbox").is_none());
        let card = HostState::Disconnected.card("devbox").unwrap();
        assert_eq!((card.title.as_str(), card.button), ("Connection lost", Some("Reconnect")));
        let card = HostState::NotConnected.card("devbox").unwrap();
        assert_eq!((card.title.as_str(), card.button), ("Not connected", Some("Connect")));
        assert_eq!(HostState::NeedsSignIn.card("devbox").unwrap().button, Some("Sign in"));
        assert_eq!(HostState::Failed { reason: "refused".into() }.card("devbox").unwrap().detail, "refused");
        assert_eq!(HostState::Reconnecting { attempt: 1 }.card("devbox").unwrap().button, None);
        assert_eq!(HostState::UpdateNeeded { remote: None }.card("devbox").unwrap().button, Some("Update and restart"));
        let card = HostState::Reconnecting { attempt: 3 }.card("devbox").unwrap();
        assert_eq!(card.title, "Reconnecting to devbox…");
        assert!(card.detail.contains("Attempt 3 of 4"), "{}", card.detail);
        let card = HostState::UpdateNeeded { remote: Some("protocol 6".into()) }.card("devbox").unwrap();
        assert!(card.detail.contains("protocol 6"), "{}", card.detail);
        assert_eq!(card.button, Some("Update and restart"));
        assert_eq!(HostState::NotInstalled.card("devbox").unwrap().button, Some("Install…"));
    }

    #[test]
    fn a_failed_attempt_is_read_from_the_exit_code_and_ssh_s_last_words() {
        let read = |code, stderr| classify_failure(code, stderr, None, false, None);
        assert_eq!(
            read(Some(127), "bash: line 1: ~/.local/bin/gantry: No such file or directory"),
            FailKind::NotInstalled
        );
        assert_eq!(
            read(Some(1), "sh: gantry: command not found"),
            FailKind::Other { reason: "sh: gantry: command not found".into() },
            "only exit code 127 means not installed"
        );
        assert_eq!(read(Some(255), "me@devbox: Permission denied (publickey,password)."), FailKind::NeedsInput);
        assert_eq!(read(Some(255), "Host key verification failed."), FailKind::NeedsInput);
        assert_eq!(
            read(Some(255), "ssh: connect to host devbox port 22: Connection refused\n"),
            FailKind::Other { reason: "ssh: connect to host devbox port 22: Connection refused".into() }
        );
        assert_eq!(read(Some(3), ""), FailKind::Other { reason: "ssh ended with exit code 3".into() });
        assert_eq!(read(None, ""), FailKind::Other { reason: "the connection closed".into() });
        assert_eq!(
            classify_failure(Some(255), "Permission denied", Some(6), false, None),
            FailKind::UpdateNeeded { remote: Some("protocol 6".into()) },
            "a version mismatch wins"
        );
    }

    #[test]
    fn a_changed_host_key_says_so_instead_of_asking_for_sign_in() {
        let stderr =
            "@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@\nHost key verification failed.";
        assert_eq!(
            classify_failure(Some(255), stderr, None, false, None),
            FailKind::Other { reason: "The host key of the host changed. Check ~/.ssh/known_hosts.".into() }
        );
    }

    #[test]
    fn permission_denied_when_ssh_asked_nothing_is_an_authentication_failure_not_a_sign_in() {
        let stderr = "me@devbox: Permission denied (publickey).";
        assert_eq!(
            classify_failure(Some(255), stderr, None, true, None),
            FailKind::Other {
                reason: "Authentication failed (ssh asked for nothing): me@devbox: Permission denied (publickey)."
                    .into()
            }
        );
        assert_eq!(
            classify_failure(Some(255), stderr, None, false, None),
            FailKind::NeedsInput,
            "asked, or background"
        );
    }

    #[test]
    fn a_silent_ssh_exit_with_only_an_eof_handshake_error_reports_the_exit_code() {
        assert_eq!(
            classify_failure(Some(3), "", None, false, Some("daemon closed the connection")),
            FailKind::Other { reason: "ssh ended with exit code 3".into() }
        );
    }

    #[test]
    fn a_garbled_handshake_names_its_error_when_ssh_said_nothing() {
        assert_eq!(
            classify_failure(None, "", None, false, Some("unexpected handshake reply Exited")),
            FailKind::Other { reason: "unexpected handshake reply Exited".into() }
        );
        assert_eq!(
            classify_failure(None, "warning: x\n", None, false, Some("garbled")),
            FailKind::Other { reason: "warning: x".into() },
            "ssh's own words come first"
        );
    }

    #[test]
    fn a_remote_take_over_ends_in_opened_elsewhere_and_never_retries() {
        assert_eq!(HostState::Connected.next(HostEvent::TakenOver), HostState::TakenOver);
        assert_eq!(HostState::NotConnected.next(HostEvent::TakenOver), HostState::NotConnected);
        assert!(HostState::TakenOver.can_connect());
        assert_eq!(HostState::TakenOver.next(HostEvent::Connect), HostState::Connecting);
        assert_eq!(HostState::TakenOver.label(), "opened elsewhere");
        let card = HostState::TakenOver.card("devbox").unwrap();
        assert_eq!(card.title, "Opened from another Gantry");
        assert_eq!(card.detail, "Another Gantry took over devbox. Your sessions keep running there.");
        assert_eq!(card.button, Some("Reconnect"));
    }

    #[test]
    fn cancelling_a_connecting_host_returns_it_to_not_connected() {
        assert_eq!(HostState::Connecting.next(HostEvent::Cancel), HostState::NotConnected);
        assert_eq!(HostState::Connected.next(HostEvent::Cancel), HostState::Connected);
        assert_eq!(HostState::Connecting.card("devbox").unwrap().button, Some("Cancel"));
    }

    #[test]
    fn every_state_has_a_dot() {
        use HostDot::*;
        let dots = [
            (HostState::NotConnected, None),
            (HostState::Connecting, Busy),
            (HostState::Connected, Ok),
            (HostState::Reconnecting { attempt: 1 }, Busy),
            (HostState::Disconnected, Bad),
            (HostState::NeedsSignIn, Warn),
            (HostState::UpdateNeeded { remote: Option::None }, Warn),
            (HostState::NotInstalled, Bad),
            (HostState::Failed { reason: "x".into() }, Bad),
            (HostState::TakenOver, Warn),
        ];
        for (state, dot) in dots {
            assert_eq!(state.dot(), dot, "{state:?}");
        }
    }

    #[test]
    fn prompts_are_masked_asked_yes_no_or_plain() {
        assert_eq!(prompt_kind("me@devbox's password: "), PromptKind::Masked);
        assert_eq!(prompt_kind("Enter passphrase for key '/home/me/.ssh/id_ed25519':"), PromptKind::Masked);
        assert_eq!(
            prompt_kind("Are you sure you want to continue connecting (yes/no/[fingerprint])? "),
            PromptKind::YesNo
        );
        assert_eq!(prompt_kind("Verification code: "), PromptKind::Plain);
        assert_eq!(prompt_title("devbox", "me@devbox's password: "), "Password for devbox");
        assert_eq!(prompt_title("devbox", "Enter passphrase for key 'k':"), "Key passphrase for devbox");
        assert_eq!(prompt_title("devbox", "continue connecting (yes/no)?"), "Trust devbox?");
        assert_eq!(prompt_title("devbox", "Verification code: "), "Sign in to devbox");
        assert_eq!(Duration::from_secs(1), reconnect_delay(1).unwrap());
    }
}
