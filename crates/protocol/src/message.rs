//! Control messages exchanged as JSON.
//!
//! `ClientMessage::Hello` and `ClientMessage::StopAll` are frozen: their JSON shape must never
//! change, so a GUI can always stop a daemon from another version.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Protocol version; bump on any incompatible change to non-frozen messages.
pub const PROTOCOL_VERSION: u32 = 5;

/// Daemon-assigned identifier of a pane (one PTY session).
pub type PaneId = u64;

/// What to run in a new pane. The daemon supplies the environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// Program to run.
    pub program: String,
    /// Program arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Initial columns.
    pub cols: u16,
    /// Initial rows.
    pub rows: u16,
    /// Start in this live pane's current directory instead of `cwd`, when the daemon can tell.
    #[serde(default)]
    pub cwd_from: Option<PaneId>,
    /// Run it in the dev container serving this host folder (`None` = on this machine).
    #[serde(default)]
    pub container: Option<PathBuf>,
}

/// A live pane as listed by the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Pane id.
    pub pane: PaneId,
    /// Program the pane was started with.
    pub program: String,
    /// Arguments the program was started with.
    pub args: Vec<String>,
    /// Working directory it was started in.
    pub cwd: PathBuf,
    /// Focus sequence number; higher = focused more recently (0 = never).
    pub last_focused: u64,
    /// Host folder of the dev container it runs in (`None` = this machine).
    #[serde(default)]
    pub container: Option<PathBuf>,
}

/// Where a container session runs (ran).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPlace {
    /// The dev container's host folder (its key for launching); `None` when Gantry never saw that container.
    pub host_folder: Option<PathBuf>,
    /// Its directory inside the container.
    pub path: PathBuf,
}

/// A dev container that can run sessions for a folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerChoice {
    /// The host folder it serves (its key for launching).
    pub host_folder: PathBuf,
    /// Container name.
    pub name: String,
    /// Whether it runs (a stopped one is started on launch).
    pub running: bool,
}

/// Where a Claude session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionState {
    /// In this daemon's pane.
    Managed {
        /// That pane.
        pane: PaneId,
    },
    /// Outside Gantry: another terminal on this host (`pid`), or a container (`pid: None`).
    External {
        /// Host process id, when it runs on this host.
        pid: Option<u32>,
    },
    /// Nothing runs it; it can be resumed.
    Closed,
}

/// What a live Claude session is doing (from its pid file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStatus {
    /// Working on a turn.
    Busy,
    /// Waiting for the next prompt.
    Idle,
    /// Blocked on the user (a permission prompt, a dialog, a question).
    NeedsInput {
        /// Claude's reason, e.g. `input needed`, `dialog open`.
        reason: Option<String>,
    },
}

/// How a `TakeOver` ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TakeOverOutcome {
    /// The process is gone; the session can be resumed.
    Exited,
    /// It was asked to stop but still runs after the grace period (offer Force quit).
    StillRunning,
    /// Nothing was signalled (it cannot or must not be stopped), or a forced stop failed.
    Failed {
        /// Human-readable reason.
        error: String,
    },
}

/// One Claude session, from the daemon's index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id (`claude --resume <id>`).
    pub id: String,
    /// Directory it runs (ran) in.
    pub cwd: PathBuf,
    /// Title; empty while a live session has no transcript yet.
    pub title: String,
    /// Last activity, ms since the Unix epoch.
    pub last_active_ms: u64,
    /// Where it runs.
    pub state: SessionState,
    /// Whether `cwd` still exists.
    pub cwd_exists: bool,
    /// What it is doing; `None` when closed, or when Claude has not said.
    pub status: Option<SessionStatus>,
    /// When `status` was last written, ms since the Unix epoch (0 = unknown).
    pub status_since_ms: u64,
    /// Where it runs (ran) when it is a dev container session; `cwd` is then its directory on this host when known.
    #[serde(default)]
    pub container: Option<ContainerPlace>,
}

/// The session a Claude command line names: the value after `--session-id`, `--resume` or `-r`, when
/// `program` is `claude`.
pub fn claude_session<'a>(program: &str, args: &'a [String]) -> Option<&'a str> {
    if std::path::Path::new(program).file_stem().is_none_or(|stem| stem != "claude") {
        return None;
    }
    args.windows(2).find(|pair| matches!(pair[0].as_str(), "--session-id" | "--resume" | "-r")).map(|pair| pair[1].as_str())
}

/// GUI → daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message of every connection (frozen).
    Hello {
        /// Client protocol version.
        protocol: u32,
    },
    /// Ask for `Panes`.
    ListPanes,
    /// Start a new pane; the client is attached to it automatically.
    Spawn {
        /// Client-chosen id echoed in the reply.
        request: u64,
        /// What to run.
        spec: SpawnRequest,
    },
    /// Start receiving a pane's output; the daemon first sends `Resync` + a replay.
    Attach {
        /// Pane to attach to.
        pane: PaneId,
    },
    /// The pane's visible grid changed size.
    Resize {
        /// Pane.
        pane: PaneId,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
        /// Cell width in pixels (for size queries).
        cell_width: u16,
        /// Cell height in pixels (for size queries).
        cell_height: u16,
    },
    /// The user focused this pane.
    Focus {
        /// Pane.
        pane: PaneId,
    },
    /// Terminate this pane's process.
    Kill {
        /// Pane.
        pane: PaneId,
    },
    /// Stop the process that runs `session` outside Gantry (SIGTERM, or SIGKILL when `force`), after verifying it.
    TakeOver {
        /// Session id.
        session: String,
        /// Kill immediately instead of asking it to stop.
        force: bool,
    },
    /// Which dev container (if any) mounts `dir`; answered by `ContainerFound`.
    FindContainer {
        /// Client-chosen id echoed in the reply.
        request: u64,
        /// Directory a new session would start in.
        dir: PathBuf,
    },
    /// Terminate every pane and stop the daemon (frozen).
    StopAll,
}

/// Daemon → GUI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Handshake accepted.
    Welcome {
        /// Daemon protocol version.
        protocol: u32,
        /// Identifies this daemon process; pane ids are only meaningful within one instance.
        instance: String,
    },
    /// Handshake rejected; only `StopAll` is accepted afterwards.
    VersionMismatch {
        /// Daemon protocol version.
        daemon_protocol: u32,
    },
    /// Live panes, most recently focused first.
    Panes {
        /// Panes.
        panes: Vec<PaneInfo>,
    },
    /// A `Spawn` succeeded.
    Spawned {
        /// Echoed request id.
        request: u64,
        /// New pane.
        pane: PaneId,
        /// Directory the program started in.
        cwd: PathBuf,
    },
    /// A `Spawn` failed.
    SpawnFailed {
        /// Echoed request id.
        request: u64,
        /// Human-readable reason.
        error: String,
    },
    /// Reset this pane's terminal to `cols`×`rows`; the next data frame for it is a full replay.
    Resync {
        /// Pane.
        pane: PaneId,
        /// Columns of the replayed screen.
        cols: u16,
        /// Rows of the replayed screen.
        rows: u16,
    },
    /// The pane's process exited; the pane is gone.
    Exited {
        /// Pane.
        pane: PaneId,
        /// Exit code.
        code: u32,
    },
    /// Every known Claude session, replacing any earlier list (sent after `Panes`).
    Sessions {
        /// Sessions, most recently active first.
        sessions: Vec<SessionInfo>,
    },
    /// Sessions that appeared or changed, and ids that disappeared, since the last list.
    SessionsChanged {
        /// New or changed sessions.
        upserted: Vec<SessionInfo>,
        /// Ids no longer known.
        removed: Vec<String>,
    },
    /// How a `TakeOver` request ended.
    TakeOverResult {
        /// Session id from the request.
        session: String,
        /// What happened.
        outcome: TakeOverOutcome,
    },
    /// The answer to `FindContainer`.
    ContainerFound {
        /// Echoed request id.
        request: u64,
        /// The container that mounts the directory, if any.
        container: Option<ContainerChoice>,
    },
    /// Another GUI connected; this connection is closed next.
    TakenOver,
    /// A request could not be handled.
    Error {
        /// Human-readable reason.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_messages_keep_their_wire_shape() {
        assert_eq!(serde_json::to_string(&ClientMessage::Hello { protocol: 7 }).unwrap(), r#"{"type":"hello","protocol":7}"#);
        assert_eq!(serde_json::to_string(&ClientMessage::StopAll).unwrap(), r#"{"type":"stop_all"}"#);
    }

    #[test]
    fn messages_round_trip() {
        let msg = ClientMessage::Spawn {
            request: 3,
            spec: SpawnRequest { program: "claude".into(), args: vec!["--resume".into()], cwd: "/tmp".into(), cols: 80, rows: 24, cwd_from: Some(4), container: None },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), msg);
        for reply in [
            DaemonMessage::Resync { pane: 9, cols: 100, rows: 30 },
            DaemonMessage::Welcome { protocol: PROTOCOL_VERSION, instance: "123-456".into() },
            DaemonMessage::Spawned { request: 3, pane: 9, cwd: "/srv".into() },
        ] {
            let json = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<DaemonMessage>(&json).unwrap(), reply);
        }
    }

    #[test]
    fn cwd_from_is_optional_on_the_wire() {
        let json = r#"{"program":"sh","args":[],"cwd":"/","cols":80,"rows":24}"#;
        assert_eq!(serde_json::from_str::<SpawnRequest>(json).unwrap().cwd_from, None);
    }

    #[test]
    fn session_messages_round_trip_with_a_tagged_state_and_status() {
        let session = SessionInfo {
            id: "9835b878".into(),
            cwd: "/home/me/repo".into(),
            title: "Fix the build".into(),
            last_active_ms: 1_790_840_985_757,
            state: SessionState::Managed { pane: 3 },
            cwd_exists: true,
            status: Some(SessionStatus::NeedsInput { reason: Some("dialog open".into()) }),
            status_since_ms: 1_790_840_985_000,
            container: None,
        };
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains(r#""state":{"kind":"managed","pane":3}"#), "{json}");
        assert!(json.contains(r#""status":{"kind":"needs_input","reason":"dialog open"}"#), "{json}");
        for message in [
            DaemonMessage::Sessions { sessions: vec![session.clone()] },
            DaemonMessage::SessionsChanged {
                upserted: vec![SessionInfo { state: SessionState::External { pid: None }, status: None, ..session.clone() }],
                removed: vec!["gone".into()],
            },
        ] {
            let json = serde_json::to_string(&message).unwrap();
            assert_eq!(serde_json::from_str::<DaemonMessage>(&json).unwrap(), message);
        }
    }

    #[test]
    fn take_over_messages_round_trip() {
        let request = ClientMessage::TakeOver { session: "s".into(), force: true };
        assert_eq!(serde_json::to_string(&request).unwrap(), r#"{"type":"take_over","session":"s","force":true}"#);
        for outcome in [TakeOverOutcome::Exited, TakeOverOutcome::StillRunning, TakeOverOutcome::Failed { error: "no".into() }] {
            let reply = DaemonMessage::TakeOverResult { session: "s".into(), outcome };
            let json = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<DaemonMessage>(&json).unwrap(), reply);
        }
    }

    #[test]
    fn claude_session_reads_the_session_flags_of_claude_only() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(claude_session("claude", &args(&["--session-id", "abc"])), Some("abc"));
        assert_eq!(claude_session("/usr/bin/claude", &args(&["--model", "x", "--resume", "def"])), Some("def"));
        assert_eq!(claude_session("claude", &args(&["-r", "ghi"])), Some("ghi"));
        assert_eq!(claude_session("claude", &args(&["--continue"])), None);
        assert_eq!(claude_session("sh", &args(&["--session-id", "abc"])), None);
        assert_eq!(claude_session("claude", &args(&["--resume"])), None);
    }

    #[test]
    fn messages_from_before_containers_still_parse() {
        let spawn = r#"{"program":"sh","args":[],"cwd":"/","cols":80,"rows":24}"#;
        assert_eq!(serde_json::from_str::<SpawnRequest>(spawn).unwrap().container, None);
        let info = r#"{"id":"s","cwd":"/","title":"","last_active_ms":0,"state":{"kind":"closed"},"cwd_exists":true,"status":null,"status_since_ms":0}"#;
        assert_eq!(serde_json::from_str::<SessionInfo>(info).unwrap().container, None);
    }
}
