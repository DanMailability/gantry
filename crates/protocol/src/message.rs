//! Control messages exchanged as JSON.
//!
//! `ClientMessage::Hello` and `ClientMessage::StopAll` are frozen: their JSON shape must never
//! change, so a GUI can always stop a daemon from another version.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Protocol version; bump on any incompatible change to non-frozen messages.
pub const PROTOCOL_VERSION: u32 = 1;

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
}

/// A live pane as listed by the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Pane id.
    pub pane: PaneId,
    /// Program the pane was started with.
    pub program: String,
    /// Working directory it was started in.
    pub cwd: PathBuf,
    /// Focus sequence number; higher = focused more recently (0 = never).
    pub last_focused: u64,
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
            spec: SpawnRequest { program: "claude".into(), args: vec!["--resume".into()], cwd: "/tmp".into(), cols: 80, rows: 24 },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), msg);
        let reply = DaemonMessage::Resync { pane: 9, cols: 100, rows: 30 };
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<DaemonMessage>(&json).unwrap(), reply);
    }
}
