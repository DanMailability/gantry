//! The daemon server: owns panes (PTY + terminal state) and serves one GUI client at a time.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use app_core::term_state::TermState;
use claude_index::index::SessionRecord;
use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use protocol::frame::{Frame, read_frame, write_frame};
use claude_index::host::LocalHost;
use protocol::message::{
    ClientMessage, DaemonMessage, PROTOCOL_VERSION, PaneId, PaneInfo, SessionInfo, SpawnRequest, TakeOverOutcome, claude_session,
};
use tokio::sync::Notify;

use crate::outbox::Outbox;
use crate::pty::{PtyProcess, SpawnSpec, drain_batch, spawn_output_reader};
use crate::sessions::{Gone, PaneSession, diff, resolve, spawn_indexer, still_claimed};
use crate::take_over::{self, FORCE_WAIT, GRACE};

const OUTPUT_QUEUE_CHUNKS: usize = 32;
const MAX_BATCH_BYTES: usize = 256 * 1024;
const CLIENT_DATA_BUDGET: usize = 4 << 20;
const RESYNC_TICK: Duration = Duration::from_millis(100);
const EXIT_DRAIN: Duration = Duration::from_millis(300);

/// How the server runs.
pub struct ServerOptions {
    /// Environment every pane starts from (the resolved login-shell environment).
    pub base_env: Vec<(String, String)>,
    /// If set (Unix), connections from other users are rejected.
    pub owner_uid: Option<u32>,
    /// Stop if no client connects within this time after start.
    pub idle_timeout: Duration,
    /// Claude config directory to index; `None` sends no session messages.
    pub claude_dir: Option<PathBuf>,
}

struct Pane {
    pty: PtyProcess,
    term: TermState,
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    /// Session its command line names.
    session_arg: Option<String>,
    /// Start time of its process (to recognise that process in the index after it exits).
    start: Option<String>,
    last_focused: u64,
    needs_resync: bool,
}

struct Client {
    id: u64,
    outbox: Outbox,
    attached: HashSet<PaneId>,
    /// Wakes this connection's reader so it drops the socket (takeover / shutdown).
    kicked: Arc<Notify>,
}

#[derive(Default)]
struct State {
    panes: HashMap<PaneId, Pane>,
    client: Option<Client>,
    next_pane: PaneId,
    next_client: u64,
    focus_seq: u64,
    ever_connected: bool,
    /// Whether session messages are sent (an index runs).
    sessions_enabled: bool,
    /// Latest index scan; `None` until the first scan finishes.
    records: Option<Arc<Vec<SessionRecord>>>,
    /// Sessions as last sent to (or computed for) the client.
    sent: Vec<SessionInfo>,
    /// Processes seen exiting that the index still lists as running.
    gone: Vec<Gone>,
}

struct Shared {
    state: Mutex<State>,
    shutdown: Notify,
    base_env: Vec<(String, String)>,
    owner_uid: Option<u32>,
    /// Identifies this daemon process (sent in `Welcome`).
    instance: String,
}

impl Shared {
    /// Lock the daemon state, recovering from poisoning: a panic in one pane's task must not
    /// wedge every other session (spec §6).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stop_if_idle(&self, state: &State) {
        if state.client.is_none() && state.panes.is_empty() {
            self.shutdown.notify_one();
        }
    }
}

impl State {
    fn control(&self, message: &DaemonMessage) {
        if let Some(client) = &self.client {
            client.outbox.push_control(Frame::control(message));
        }
    }

    /// Send `pane`'s live output to the client, falling back to a resync when it lags.
    fn deliver(&mut self, pane_id: PaneId, bytes: Vec<u8>) {
        let Some(client) = &self.client else { return };
        if !client.attached.contains(&pane_id) {
            return;
        }
        let outbox = client.outbox.clone();
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };
        if pane.needs_resync || !outbox.try_push_data(Frame::Data { pane: pane_id, bytes }) {
            pane.needs_resync = true;
            self.resync_if_room(pane_id);
        }
    }

    fn resync_if_room(&mut self, pane_id: PaneId) {
        let Some(client) = &self.client else { return };
        if !client.outbox.has_room() {
            return;
        }
        let outbox = client.outbox.clone();
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            Self::send_resync(&outbox, pane_id, pane);
        }
    }

    /// Send the session list (`full`) or what changed since the last one. Without a client only the
    /// baseline is updated; a connecting client always gets a full list.
    fn publish_sessions(&mut self, full: bool) {
        let Some(records) = self.records.as_ref().filter(|_| self.sessions_enabled) else { return };
        let panes: Vec<PaneSession> = self
            .panes
            .iter()
            .map(|(id, pane)| PaneSession { pane: *id, pid: pane.pty.pid(), session_arg: pane.session_arg.clone() })
            .collect();
        let sessions = resolve(records, &panes, &self.gone);
        if full {
            self.control(&DaemonMessage::Sessions { sessions: sessions.clone() });
        } else {
            let (upserted, removed) = diff(&self.sent, &sessions);
            if !upserted.is_empty() || !removed.is_empty() {
                self.control(&DaemonMessage::SessionsChanged { upserted, removed });
            }
        }
        self.sent = sessions;
    }

    fn send_resync(outbox: &Outbox, pane_id: PaneId, pane: &mut Pane) {
        let snapshot = pane.term.snapshot();
        outbox.push_control(Frame::control(&DaemonMessage::Resync {
            pane: pane_id,
            cols: snapshot.cols as u16,
            rows: snapshot.rows.len() as u16,
        }));
        outbox.force_push_data(Frame::Data { pane: pane_id, bytes: pane.term.replay_bytes() });
        pane.needs_resync = false;
    }
}

/// Serve clients until `StopAll` or until idle (no panes and no client).
pub async fn serve(listener: Listener, options: ServerOptions) -> anyhow::Result<()> {
    let claude_dir = options.claude_dir;
    let shared = Arc::new(Shared {
        state: Mutex::new(State { next_pane: 1, sessions_enabled: claude_dir.is_some(), ..State::default() }),
        shutdown: Notify::new(),
        base_env: options.base_env,
        owner_uid: options.owner_uid,
        instance: format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
        ),
    });
    let idle = {
        let shared = shared.clone();
        tokio::spawn(async move {
            tokio::time::sleep(options.idle_timeout).await;
            let state = shared.lock();
            if !state.ever_connected {
                shared.stop_if_idle(&state);
            }
        })
    };
    let ticker = {
        let shared = shared.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(RESYNC_TICK).await;
                let mut state = shared.lock();
                let lagging: Vec<PaneId> =
                    state.panes.iter().filter(|(_, pane)| pane.needs_resync).map(|(id, _)| *id).collect();
                for pane in lagging {
                    state.resync_if_room(pane);
                }
            }
        })
    };
    let sessions = claude_dir.map(|dir| {
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        spawn_indexer(dir, tx);
        let shared = shared.clone();
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let records = rx.borrow_and_update().clone();
                let mut state = shared.lock();
                if let Some(records) = &records {
                    state.gone.retain(|gone| still_claimed(gone, records));
                }
                // The first scan is the first list a connected client gets.
                let first = state.records.is_none();
                state.records = records;
                state.publish_sessions(first);
            }
        })
    });
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => { tokio::spawn(handle_connection(shared.clone(), stream)); }
                Err(err) => tracing::warn!("accept failed: {err}"),
            },
            _ = shared.shutdown.notified() => break,
        }
    }
    idle.abort();
    ticker.abort();
    if let Some(task) = sessions {
        task.abort();
    }
    let mut state = shared.lock();
    if let Some(client) = state.client.take() {
        client.outbox.close();
        client.kicked.notify_one();
    }
    state.panes.clear();
    Ok(())
}

async fn handle_connection(shared: Arc<Shared>, stream: Stream) {
    #[cfg(unix)]
    if let Some(owner) = shared.owner_uid {
        let peer = stream.peer_creds().ok().and_then(|creds| creds.euid());
        if peer != Some(owner) {
            tracing::warn!("rejected connection from uid {peer:?}");
            return;
        }
    }
    let (mut recv, mut send) = stream.split();
    let Ok(Some(first)) = read_frame(&mut recv).await else { return };
    let Some(Ok(ClientMessage::Hello { protocol })) = first.parse::<ClientMessage>() else { return };
    if protocol != PROTOCOL_VERSION {
        let _ = write_frame(&mut send, &Frame::control(&DaemonMessage::VersionMismatch { daemon_protocol: PROTOCOL_VERSION })).await;
        while let Ok(Some(frame)) = read_frame(&mut recv).await {
            if let Some(Ok(ClientMessage::StopAll)) = frame.parse::<ClientMessage>() {
                stop_all(&shared);
                return;
            }
        }
        return;
    }
    let outbox = Outbox::new(CLIENT_DATA_BUDGET);
    let kicked = Arc::new(Notify::new());
    let client_id = {
        let mut state = shared.lock();
        state.next_client += 1;
        state.ever_connected = true;
        let id = state.next_client;
        if let Some(old) = state.client.take() {
            old.outbox.push_control(Frame::control(&DaemonMessage::TakenOver));
            old.outbox.close();
            old.kicked.notify_one();
        }
        outbox.push_control(Frame::control(&DaemonMessage::Welcome { protocol: PROTOCOL_VERSION, instance: shared.instance.clone() }));
        state.client = Some(Client { id, outbox: outbox.clone(), attached: HashSet::new(), kicked: kicked.clone() });
        id
    };
    let writer = tokio::spawn(async move {
        while let Some(frame) = outbox.next().await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
        }
    });
    loop {
        let frame = tokio::select! {
            read = read_frame(&mut recv) => match read {
                Ok(Some(frame)) => frame,
                _ => break,
            },
            _ = kicked.notified() => break,
        };
        match frame {
            Frame::Data { pane, bytes } => {
                let mut state = shared.lock();
                if let Some(pane) = state.panes.get_mut(&pane) {
                    let _ = pane.pty.write(&bytes);
                }
            }
            control => match control.parse::<ClientMessage>() {
                Some(Ok(message)) => handle_message(&shared, client_id, message),
                _ => tracing::warn!("ignoring malformed control frame"),
            },
        }
    }
    {
        let mut state = shared.lock();
        if state.client.as_ref().is_some_and(|client| client.id == client_id) {
            if let Some(client) = state.client.take() {
                client.outbox.close();
            }
            shared.stop_if_idle(&state);
        }
    }
    let _ = writer.await;
}

fn stop_all(shared: &Shared) {
    shared.lock().panes.clear();
    shared.shutdown.notify_one();
}

/// Stop the outside process running `session`, then report. On success, the session is published as closed first
/// (its exit is remembered until the index notices), so the GUI's resume finds it closed.
fn take_over_session(shared: &Arc<Shared>, session: String, force: bool) {
    let target = {
        let state = shared.lock();
        let records = state.records.as_deref().map_or(&[][..], Vec::as_slice);
        take_over::target(&state.sent, records, &session)
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        let host = LocalHost::new();
        let outcome = match target.and_then(|target| take_over::signal(&target, force, &host).map(|_| target)) {
            Err(error) => TakeOverOutcome::Failed { error },
            Ok(target) => {
                if take_over::wait_for_exit(&target, &host, if force { FORCE_WAIT } else { GRACE }).await {
                    let mut state = shared.lock();
                    state.gone.push(Gone { pid: target.pid, start: Some(target.start) });
                    state.publish_sessions(false);
                    TakeOverOutcome::Exited
                } else if force {
                    TakeOverOutcome::Failed { error: "the process did not exit after SIGKILL".into() }
                } else {
                    TakeOverOutcome::StillRunning
                }
            }
        };
        shared.lock().control(&DaemonMessage::TakeOverResult { session, outcome });
    });
}

fn handle_message(shared: &Arc<Shared>, client_id: u64, message: ClientMessage) {
    match message {
        ClientMessage::Hello { .. } => {}
        ClientMessage::StopAll => stop_all(shared),
        ClientMessage::ListPanes => {
            let mut state = shared.lock();
            let mut panes: Vec<PaneInfo> = state
                .panes
                .iter()
                .map(|(id, pane)| PaneInfo {
                    pane: *id,
                    program: pane.program.clone(),
                    args: pane.args.clone(),
                    cwd: pane.cwd.clone(),
                    last_focused: pane.last_focused,
                })
                .collect();
            panes.sort_by(|a, b| b.last_focused.cmp(&a.last_focused).then(b.pane.cmp(&a.pane)));
            state.control(&DaemonMessage::Panes { panes });
            state.publish_sessions(true);
        }
        ClientMessage::Spawn { request, spec } => spawn_pane(shared, client_id, request, spec),
        ClientMessage::Attach { pane } => {
            let mut state = shared.lock();
            let Some(client) = state.client.as_mut() else { return };
            client.attached.insert(pane);
            let outbox = client.outbox.clone();
            match state.panes.get_mut(&pane) {
                Some(p) => State::send_resync(&outbox, pane, p),
                None => state.control(&DaemonMessage::Error { message: format!("no pane {pane}") }),
            }
        }
        ClientMessage::Resize { pane, cols, rows, cell_width, cell_height } => {
            let mut state = shared.lock();
            if let Some(p) = state.panes.get_mut(&pane) {
                p.term.resize(cols, rows);
                p.term.set_cell_size(cell_width, cell_height);
                if let Err(err) = p.pty.resize(cols, rows) {
                    tracing::warn!("resize failed: {err:#}");
                }
            }
        }
        ClientMessage::Focus { pane } => {
            let mut state = shared.lock();
            state.focus_seq += 1;
            let seq = state.focus_seq;
            if let Some(p) = state.panes.get_mut(&pane) {
                p.last_focused = seq;
            }
        }
        ClientMessage::TakeOver { session, force } => take_over_session(shared, session, force),
        ClientMessage::Kill { pane } => {
            let mut state = shared.lock();
            if let Some(p) = state.panes.get_mut(&pane) {
                let _ = p.pty.kill();
            }
        }
    }
}

fn spawn_pane(shared: &Arc<Shared>, client_id: u64, request: u64, spec: SpawnRequest) {
    let cwd = spec
        .cwd_from
        .and_then(|from| shared.lock().panes.get(&from).and_then(|pane| pane.pty.current_dir()))
        .unwrap_or_else(|| spec.cwd.clone());
    let session_arg = claude_session(&spec.program, &spec.args).map(String::from);
    let pty_spec = SpawnSpec {
        program: spec.program.clone(),
        args: spec.args.clone(),
        cwd: cwd.clone(),
        env: Vec::new(),
        base_env: Some(shared.base_env.clone()),
        cols: spec.cols,
        rows: spec.rows,
    };
    let mut state = shared.lock();
    let (pty, reader) = match PtyProcess::spawn(&pty_spec) {
        Ok(pair) => pair,
        Err(err) => {
            state.control(&DaemonMessage::SpawnFailed { request, error: format!("{err:#}") });
            return;
        }
    };
    let id = state.next_pane;
    state.next_pane += 1;
    state.focus_seq += 1;
    let exit = pty.exit_signal();
    let start = pty.pid().and_then(claude_index::host::process_start);
    let pane = Pane {
        pty,
        term: TermState::new(spec.cols, spec.rows),
        program: spec.program,
        args: spec.args,
        cwd: cwd.clone(),
        session_arg,
        start,
        last_focused: state.focus_seq,
        needs_resync: false,
    };
    state.panes.insert(id, pane);
    if let Some(client) = state.client.as_mut().filter(|client| client.id == client_id) {
        client.attached.insert(id);
    }
    state.control(&DaemonMessage::Spawned { request, pane: id, cwd });
    state.publish_sessions(false);
    drop(state);

    let output_done = Arc::new(Notify::new());
    let output = spawn_output_reader(reader, OUTPUT_QUEUE_CHUNKS);
    let (reader_shared, reader_done) = (shared.clone(), output_done.clone());
    tokio::spawn(async move {
        while let Ok(first) = output.recv().await {
            let bytes = drain_batch(&output, first, MAX_BATCH_BYTES);
            let pane_alive = {
                let mut state = reader_shared.lock();
                match state.panes.get_mut(&id) {
                    Some(pane) => {
                        pane.term.feed(&bytes);
                        let replies = pane.term.take_pty_writes();
                        if !replies.is_empty() {
                            let _ = pane.pty.write(&replies);
                        }
                        state.deliver(id, bytes);
                        true
                    }
                    None => false,
                }
            };
            if !pane_alive {
                break;
            }
            // A flooding pane keeps its channel full, so `recv` never waits; yield so connection
            // writers and other panes still run.
            tokio::task::yield_now().await;
        }
        reader_done.notify_one();
    });
    let exit_shared = shared.clone();
    tokio::spawn(async move {
        let code = exit.recv().await.unwrap_or(1);
        // Let the reader deliver the program's last output before announcing the exit.
        let _ = tokio::time::timeout(EXIT_DRAIN, output_done.notified()).await;
        let mut state = exit_shared.lock();
        // A lagging client would otherwise never see the final screen: send it before `Exited`.
        let lagging_outbox = state
            .client
            .as_ref()
            .filter(|client| client.attached.contains(&id))
            .map(|client| client.outbox.clone())
            .filter(|_| state.panes.get(&id).is_some_and(|pane| pane.needs_resync));
        if let (Some(outbox), Some(pane)) = (lagging_outbox, state.panes.get_mut(&id)) {
            State::send_resync(&outbox, id, pane);
        }
        if let Some(pane) = state.panes.remove(&id)
            && let Some(pid) = pane.pty.pid()
        {
            state.gone.push(Gone { pid, start: pane.start.clone() });
        }
        state.control(&DaemonMessage::Exited { pane: id, code });
        state.publish_sessions(false);
        exit_shared.stop_if_idle(&state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sessions_are_sent_only_once_the_first_scan_is_done() {
        use claude_index::index::IndexState;
        let outbox = Outbox::new(1 << 20);
        let mut state = State { sessions_enabled: true, ..State::default() };
        state.client = Some(Client { id: 1, outbox: outbox.clone(), attached: HashSet::new(), kicked: Arc::new(Notify::new()) });
        // A client connects (ListPanes) before the index has scanned: an empty list would claim no
        // session exists, so nothing is sent yet.
        state.publish_sessions(true);
        let record = SessionRecord {
            id: "s".into(),
            cwd: "/".into(),
            title: "t".into(),
            last_active_ms: 1,
            state: IndexState::Closed,
            cwd_exists: true,
            activity: None,
            activity_since_ms: 0,
            proc_start: None,
        };
        state.records = Some(Arc::new(vec![record]));
        state.publish_sessions(true);
        outbox.close();
        let mut messages = Vec::new();
        while let Some(frame) = outbox.next().await {
            messages.push(frame.parse::<DaemonMessage>().unwrap().unwrap());
        }
        assert!(matches!(&messages[..], [DaemonMessage::Sessions { sessions }] if sessions.len() == 1), "{messages:?}");
    }

    #[test]
    fn a_panic_while_holding_the_state_lock_does_not_wedge_the_daemon() {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            shutdown: Notify::new(),
            base_env: Vec::new(),
            owner_uid: None,
            instance: String::new(),
        });
        let poisoner = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("simulated pane bug");
        })
        .join();
        assert!(shared.state.is_poisoned());
        shared.lock().next_pane = 7;
        assert_eq!(shared.lock().next_pane, 7);
    }
}
