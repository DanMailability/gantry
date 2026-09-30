//! GUI side of the daemon connection. Runs its own thread and tokio runtime; the GUI talks to it
//! through non-blocking methods and a bounded event channel.

use std::fmt;
use std::io;
use std::time::Duration;

use interprocess::local_socket::tokio::{Stream, prelude::*};
use protocol::frame::{Frame, read_frame, write_frame};
use protocol::message::{ClientMessage, DaemonMessage, PROTOCOL_VERSION, PaneId, PaneInfo, SpawnRequest};
use tokio::sync::mpsc;

use crate::launch::connect_or_spawn;
use crate::paths::Paths;

/// Events delivered to the GUI, in daemon order.
const EVENT_QUEUE: usize = 64;

/// Something the daemon told the GUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    /// Program output (or, right after `Resync`, the replay).
    Output {
        /// Pane.
        pane: PaneId,
        /// Bytes.
        bytes: Vec<u8>,
    },
    /// Reset the pane's terminal to this size before feeding the next output.
    Resync {
        /// Pane.
        pane: PaneId,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// The pane's process exited.
    Exited {
        /// Pane.
        pane: PaneId,
        /// Exit code.
        code: u32,
    },
    /// Another GUI took over the daemon.
    TakenOver,
    /// The connection to the daemon was lost.
    Disconnected,
}

/// What the GUI was asked to open.
#[derive(Clone, Debug)]
pub struct Launch {
    /// Explicitly requested command (`ccm -- cmd` or `--cwd`); always spawns a new pane.
    pub explicit: Option<SpawnRequest>,
    /// What to spawn when there is nothing to reattach to.
    pub default: SpawnRequest,
}

/// Decision taken at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Startup {
    /// Reattach to this live pane.
    Attach(PaneId),
    /// Start a new pane.
    Spawn(SpawnRequest),
}

/// Explicit command → spawn it; else the most recently focused live pane; else the default.
pub fn choose_startup(panes: &[PaneInfo], launch: &Launch) -> Startup {
    if let Some(spec) = &launch.explicit {
        return Startup::Spawn(spec.clone());
    }
    panes
        .iter()
        .max_by_key(|pane| (pane.last_focused, pane.pane))
        .map_or_else(|| Startup::Spawn(launch.default.clone()), |pane| Startup::Attach(pane.pane))
}

/// Why the client could not start.
#[derive(Debug)]
pub enum StartError {
    /// The running daemon speaks a different protocol version.
    VersionMismatch {
        /// Its version.
        daemon_protocol: u32,
    },
    /// The daemon could not start the requested program.
    SpawnFailed(String),
    /// Connection or protocol failure.
    Io(anyhow::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::VersionMismatch { daemon_protocol } => {
                write!(f, "the running daemon uses protocol {daemon_protocol}, this ccm uses {PROTOCOL_VERSION}")
            }
            StartError::SpawnFailed(error) => write!(f, "failed to start program: {error}"),
            StartError::Io(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for StartError {}

enum Outgoing {
    Frame(Frame),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Handle to the daemon connection; cheap to clone.
#[derive(Clone)]
pub struct DaemonClient {
    outgoing: mpsc::UnboundedSender<Outgoing>,
}

impl DaemonClient {
    /// Connect (starting the daemon via `spawn` if needed), pick the startup pane, and begin streaming events.
    pub fn start(
        paths: &Paths,
        launch: Launch,
        spawn: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> Result<(DaemonClient, PaneId, async_channel::Receiver<ClientEvent>), StartError> {
        let paths = paths.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = async_channel::bounded(EVENT_QUEUE);
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = ready_tx.send(Err(StartError::Io(err.into())));
                    return;
                }
            };
            runtime.block_on(run_connection(paths, launch, spawn, ready_tx, outgoing_rx, events_tx));
        });
        let pane = ready_rx.recv().map_err(|_| StartError::Io(anyhow::anyhow!("client thread ended")))??;
        Ok((DaemonClient { outgoing }, pane, events))
    }

    /// Send keyboard/paste input to a pane.
    pub fn input(&self, pane: PaneId, bytes: &[u8]) {
        let _ = self.outgoing.send(Outgoing::Frame(Frame::Data { pane, bytes: bytes.to_vec() }));
    }

    /// Report the pane's new grid and cell size.
    pub fn resize(&self, pane: PaneId, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        self.send(&ClientMessage::Resize { pane, cols, rows, cell_width, cell_height });
    }

    /// Mark the pane as most recently focused.
    pub fn focus(&self, pane: PaneId) {
        self.send(&ClientMessage::Focus { pane });
    }

    /// Ask the daemon to stop everything; returns once the request is written (or after `timeout`).
    pub fn stop_all_and_wait(&self, timeout: Duration) {
        self.send(&ClientMessage::StopAll);
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if self.outgoing.send(Outgoing::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(timeout);
        }
    }

    fn send(&self, message: &ClientMessage) {
        let _ = self.outgoing.send(Outgoing::Frame(Frame::control(message)));
    }
}

async fn run_connection(
    paths: Paths,
    launch: Launch,
    spawn: impl FnOnce() -> io::Result<()>,
    ready: std::sync::mpsc::Sender<Result<PaneId, StartError>>,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
    events: async_channel::Sender<ClientEvent>,
) {
    let stream = match connect_or_spawn(&paths, spawn).await {
        Ok(stream) => stream,
        Err(err) => {
            let _ = ready.send(Err(StartError::Io(err.into())));
            return;
        }
    };
    let (mut recv, mut send) = stream.split();
    let startup = handshake(&mut recv, &mut send, &launch).await;
    let pane = match startup {
        Ok(pane) => pane,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    let _ = ready.send(Ok(pane));
    let writer = tokio::spawn(async move {
        while let Some(item) = outgoing.recv().await {
            match item {
                Outgoing::Frame(frame) => {
                    if write_frame(&mut send, &frame).await.is_err() {
                        break;
                    }
                }
                Outgoing::Flush(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    });
    loop {
        let event = match read_frame(&mut recv).await {
            Ok(Some(Frame::Data { pane, bytes })) => ClientEvent::Output { pane, bytes },
            Ok(Some(frame)) => match frame.parse::<DaemonMessage>() {
                Some(Ok(DaemonMessage::Resync { pane, cols, rows })) => ClientEvent::Resync { pane, cols, rows },
                Some(Ok(DaemonMessage::Exited { pane, code })) => ClientEvent::Exited { pane, code },
                Some(Ok(DaemonMessage::TakenOver)) => ClientEvent::TakenOver,
                Some(Ok(other)) => {
                    tracing::debug!("ignoring daemon message {other:?}");
                    continue;
                }
                _ => continue,
            },
            Ok(None) | Err(_) => ClientEvent::Disconnected,
        };
        let last = matches!(event, ClientEvent::Disconnected | ClientEvent::TakenOver);
        if events.send(event).await.is_err() || last {
            break;
        }
    }
    writer.abort();
}

async fn handshake<R, W>(recv: &mut R, send: &mut W, launch: &Launch) -> Result<PaneId, StartError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let io = |err: io::Error| StartError::Io(err.into());
    write_frame(send, &Frame::control(&ClientMessage::Hello { protocol: PROTOCOL_VERSION })).await.map_err(io)?;
    match next_message(recv).await? {
        DaemonMessage::Welcome { .. } => {}
        DaemonMessage::VersionMismatch { daemon_protocol } => return Err(StartError::VersionMismatch { daemon_protocol }),
        other => return Err(StartError::Io(anyhow::anyhow!("unexpected handshake reply {other:?}"))),
    }
    write_frame(send, &Frame::control(&ClientMessage::ListPanes)).await.map_err(io)?;
    let panes = loop {
        if let DaemonMessage::Panes { panes } = next_message(recv).await? {
            break panes;
        }
    };
    let pane = match choose_startup(&panes, launch) {
        Startup::Attach(pane) => {
            write_frame(send, &Frame::control(&ClientMessage::Attach { pane })).await.map_err(io)?;
            pane
        }
        Startup::Spawn(spec) => {
            write_frame(send, &Frame::control(&ClientMessage::Spawn { request: 1, spec })).await.map_err(io)?;
            loop {
                match next_message(recv).await? {
                    DaemonMessage::Spawned { request: 1, pane } => break pane,
                    DaemonMessage::SpawnFailed { error, .. } => return Err(StartError::SpawnFailed(error)),
                    _ => {}
                }
            }
        }
    };
    write_frame(send, &Frame::control(&ClientMessage::Focus { pane })).await.map_err(io)?;
    Ok(pane)
}

async fn next_message<R: tokio::io::AsyncRead + Unpin>(recv: &mut R) -> Result<DaemonMessage, StartError> {
    loop {
        match read_frame(recv).await {
            Ok(Some(frame)) => {
                if let Some(parsed) = frame.parse::<DaemonMessage>() {
                    return parsed.map_err(|err| StartError::Io(err.into()));
                }
            }
            Ok(None) => return Err(StartError::Io(anyhow::anyhow!("daemon closed the connection"))),
            Err(err) => return Err(StartError::Io(err.into())),
        }
    }
}

/// Stop the profile's daemon (any protocol version) and wait until it no longer accepts connections.
pub fn restart_daemon(paths: &Paths) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let stream = Stream::connect(paths.socket_name()?).await?;
        let (mut recv, mut send) = stream.split();
        write_frame(&mut send, &Frame::control(&ClientMessage::Hello { protocol: PROTOCOL_VERSION })).await?;
        let _ = read_frame(&mut recv).await?;
        write_frame(&mut send, &Frame::control(&ClientMessage::StopAll)).await?;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if Stream::connect(paths.socket_name()?).await.is_err() {
                return Ok(());
            }
        }
        anyhow::bail!("daemon did not stop")
    })
}

/// Merge consecutive `Output` events of one pane (up to `max_bytes`); stop after the first other event.
pub fn batch_events(first: ClientEvent, rx: &async_channel::Receiver<ClientEvent>, max_bytes: usize) -> Vec<ClientEvent> {
    let mut batch = vec![first];
    loop {
        let Some(ClientEvent::Output { pane, bytes }) = batch.last_mut() else { return batch };
        if bytes.len() >= max_bytes {
            return batch;
        }
        let pane = *pane;
        match rx.try_recv() {
            Ok(ClientEvent::Output { pane: next, bytes: more }) if next == pane => bytes.extend_from_slice(&more),
            Ok(other) => {
                batch.push(other);
                return batch;
            }
            Err(_) => return batch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(program: &str) -> SpawnRequest {
        SpawnRequest { program: program.into(), args: vec![], cwd: "/".into(), cols: 80, rows: 24 }
    }

    fn info(pane: PaneId, last_focused: u64) -> PaneInfo {
        PaneInfo { pane, program: "sh".into(), cwd: "/".into(), last_focused }
    }

    #[test]
    fn startup_prefers_explicit_command_then_most_recent_pane() {
        let explicit = Launch { explicit: Some(request("claude")), default: request("sh") };
        assert_eq!(choose_startup(&[info(1, 5)], &explicit), Startup::Spawn(request("claude")));
        let plain = Launch { explicit: None, default: request("sh") };
        assert_eq!(choose_startup(&[info(1, 2), info(2, 9)], &plain), Startup::Attach(2));
        assert_eq!(choose_startup(&[], &plain), Startup::Spawn(request("sh")));
    }

    #[test]
    fn consecutive_output_is_batched_up_to_the_limit() {
        let (tx, rx) = async_channel::unbounded();
        for chunk in [b"b".to_vec(), b"c".to_vec(), b"d".to_vec()] {
            tx.send_blocking(ClientEvent::Output { pane: 1, bytes: chunk }).unwrap();
        }
        tx.send_blocking(ClientEvent::Exited { pane: 1, code: 0 }).unwrap();
        let batch = batch_events(ClientEvent::Output { pane: 1, bytes: b"a".to_vec() }, &rx, 3);
        assert_eq!(batch, vec![ClientEvent::Output { pane: 1, bytes: b"abc".to_vec() }]);
        let rest = batch_events(rx.try_recv().unwrap(), &rx, 100);
        assert_eq!(rest, vec![ClientEvent::Output { pane: 1, bytes: b"d".to_vec() }, ClientEvent::Exited { pane: 1, code: 0 }]);
    }
}
