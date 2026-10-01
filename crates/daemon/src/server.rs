//! The daemon server: owns panes (PTY + terminal state) and serves one GUI client at a time.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::path::Path;
use std::time::{Duration, Instant};

use app_core::term_state::TermState;
use claude_index::containers::{DockerCli, to_container};
use claude_index::host::{Marked, processes_with_env};
use claude_index::index::{IndexState, SessionRecord};
use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use protocol::frame::{Frame, read_frame, write_frame};
use claude_index::host::LocalHost;
use protocol::message::{
    ClientMessage, ContainerChoice, DaemonMessage, PROTOCOL_VERSION, PaneId, PaneInfo, SessionInfo, SpawnRequest, TakeOverOutcome, claude_session,
};
use tokio::sync::Notify;

use crate::containers::{self, PANE_MARKER, Registry, marker};
use crate::outbox::Outbox;
use crate::pty::{PtyProcess, SpawnSpec, drain_batch, spawn_output_reader};
use crate::sessions::{ContainerView, Gone, PaneSession, diff, resolve, spawn_indexer, still_claimed};
use crate::take_over::{self, FORCE_WAIT, GRACE};

const OUTPUT_QUEUE_CHUNKS: usize = 32;
const MAX_BATCH_BYTES: usize = 256 * 1024;
const CLIENT_DATA_BUDGET: usize = 4 << 20;
const RESYNC_TICK: Duration = Duration::from_millis(100);
const EXIT_DRAIN: Duration = Duration::from_millis(300);
/// Least time between Docker lookups caused by container ids the registry does not know.
const UNKNOWN_CONTAINER_REFRESH: Duration = Duration::from_secs(10);

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
    /// Where dev container mappings are remembered; `None` keeps them in memory only.
    pub containers_file: Option<PathBuf>,
    /// The `docker` program; `None` turns dev container features off.
    pub docker: Option<String>,
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
    /// Host folder of the dev container it runs in.
    container: Option<PathBuf>,
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
    /// This daemon's instance id (the prefix of its panes' `GANTRY_PANE` markers).
    instance: String,
    /// Dev containers.
    registry: Registry,
    /// Container processes (host pid) carrying one of this daemon's pane markers.
    marked: HashMap<u32, PaneId>,
    /// Container processes already checked and found carrying no marker of ours.
    unmarked: HashSet<u32>,
    /// When unknown container ids last caused a Docker lookup.
    last_unknown_refresh: Option<Instant>,
    /// Whether "Docker is unavailable" was logged already.
    docker_warned: bool,
}

struct Shared {
    state: Mutex<State>,
    shutdown: Notify,
    base_env: Vec<(String, String)>,
    owner_uid: Option<u32>,
    /// Identifies this daemon process (sent in `Welcome`).
    instance: String,
    /// The `docker` program; `None` = no dev containers.
    docker: Option<String>,
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
        let Some(records) = self.records.clone().filter(|_| self.sessions_enabled) else { return };
        self.refresh_marks(&records);
        let panes: Vec<PaneSession> = self
            .panes
            .iter()
            .map(|(id, pane)| PaneSession { pane: *id, pid: pane.pty.pid(), session_arg: pane.session_arg.clone() })
            .collect();
        let view = ContainerView { registry: &self.registry, marked: &self.marked, dir_exists: &|dir: &Path| dir.is_dir() };
        let sessions = resolve(&records, &panes, &self.gone, &view);
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

    /// Find which container processes carry this daemon's pane markers. `/proc` is scanned only while some
    /// container pane exists and the index reports a container process not checked yet.
    fn refresh_marks(&mut self, records: &[SessionRecord]) {
        let live: HashSet<u32> = records
            .iter()
            .filter_map(|r| match r.state {
                IndexState::InContainer { pid } => pid,
                _ => None,
            })
            .collect();
        let panes = &self.panes;
        self.marked.retain(|pid, pane| live.contains(pid) && panes.contains_key(pane));
        self.unmarked.retain(|pid| live.contains(pid));
        let container_panes: Vec<PaneId> = panes.iter().filter(|(_, p)| p.container.is_some()).map(|(id, _)| *id).collect();
        if container_panes.is_empty() || live.iter().all(|pid| self.marked.contains_key(pid) || self.unmarked.contains(pid)) {
            return;
        }
        let found = pane_marks(&processes_with_env(Path::new("/proc"), PANE_MARKER), &self.instance, &container_panes);
        for pid in live {
            match found.get(&pid) {
                Some(pane) => {
                    self.marked.insert(pid, *pane);
                }
                None => {
                    if !self.marked.contains_key(&pid) {
                        self.unmarked.insert(pid);
                    }
                }
            }
        }
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
    let instance = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    );
    let state = State {
        next_pane: 1,
        sessions_enabled: claude_dir.is_some(),
        instance: instance.clone(),
        registry: Registry::load(options.containers_file),
        ..State::default()
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(state),
        shutdown: Notify::new(),
        base_env: options.base_env,
        owner_uid: options.owner_uid,
        instance,
        docker: options.docker,
    });
    refresh_containers(&shared);
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
                let unknown = records.iter().flat_map(|r| r.iter()).filter_map(|r| r.container_id.as_deref()).any(|id| !state.registry.knows(id));
                if unknown && state.last_unknown_refresh.is_none_or(|at| at.elapsed() >= UNKNOWN_CONTAINER_REFRESH) {
                    state.last_unknown_refresh = Some(Instant::now());
                    refresh_containers(&shared);
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
    let mut state = shared.lock();
    let container_panes: Vec<PaneId> = state.panes.iter().filter(|(_, p)| p.container.is_some()).map(|(id, _)| *id).collect();
    for pane in container_panes {
        stop_container_processes(&marker(&shared.instance, pane));
    }
    state.panes.clear();
    drop(state);
    shared.shutdown.notify_one();
}

/// Look up dev containers (off the async threads) and republish sessions with what was found.
fn refresh_containers(shared: &Arc<Shared>) {
    let Some(program) = shared.docker.clone() else { return };
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let mut registry = shared.lock().registry.clone();
        let result = registry.refresh(&DockerCli { program });
        let mut state = shared.lock();
        match result {
            Ok(()) => {
                state.registry = registry;
                state.publish_sessions(false);
            }
            Err(err) if !state.docker_warned => {
                state.docker_warned = true;
                tracing::info!("dev containers are unavailable: {err}");
            }
            Err(_) => {}
        }
    });
}

/// The verified processes of a container pane: those carrying its marker whose start time is known.
fn marked_targets(marked: &[Marked], marker: &str) -> Vec<take_over::Target> {
    marked
        .iter()
        .filter(|m| m.value == marker)
        .filter_map(|m| Some(take_over::Target { pid: m.pid, start: m.start.clone()? }))
        .collect()
}

/// Host pid → pane for processes carrying a marker of `instance` that names one of `panes`.
fn pane_marks(marked: &[Marked], instance: &str, panes: &[PaneId]) -> HashMap<u32, PaneId> {
    marked
        .iter()
        .filter_map(|m| {
            let (owner, pane) = m.value.rsplit_once(':')?;
            let pane: PaneId = pane.parse().ok()?;
            (owner == instance && panes.contains(&pane)).then_some((m.pid, pane))
        })
        .collect()
}

/// Ask the processes a container pane started (inside the container) to stop. Killing the `docker exec` client
/// alone does not end them.
fn stop_container_processes(marker: &str) {
    let host = LocalHost::new();
    for target in marked_targets(&processes_with_env(Path::new("/proc"), PANE_MARKER), marker) {
        if let Err(err) = take_over::signal(&target, false, &host) {
            tracing::warn!("could not stop container process {}: {err}", target.pid);
        }
    }
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
                    TakeOverOutcome::Failed { error: "it did not exit after SIGKILL".into() }
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
                    container: pane.container.clone(),
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
        ClientMessage::FindContainer { request, dir } => find_container(shared, request, dir),
        ClientMessage::Kill { pane } => {
            let mut state = shared.lock();
            if state.panes.get(&pane).is_some_and(|p| p.container.is_some()) {
                stop_container_processes(&marker(&shared.instance, pane));
            }
            if let Some(p) = state.panes.get_mut(&pane) {
                let _ = p.pty.kill();
            }
        }
    }
}

fn spawn_pane(shared: &Arc<Shared>, client_id: u64, request: u64, spec: SpawnRequest) {
    if let Some(folder) = spec.container.clone() {
        spawn_in_container(shared.clone(), client_id, request, spec, folder);
        return;
    }
    let cwd = spec
        .cwd_from
        .and_then(|from| shared.lock().panes.get(&from).and_then(|pane| pane.pty.current_dir()))
        .unwrap_or_else(|| spec.cwd.clone());
    let command = (spec.program.clone(), spec.args.clone());
    start_pane(shared, client_id, request, spec, cwd, |_| command, None);
}

/// Answer `FindContainer`: refresh the dev containers (off the async threads) and name the one that mounts `dir`.
fn find_container(shared: &Arc<Shared>, request: u64, dir: PathBuf) {
    let Some(program) = shared.docker.clone() else {
        shared.lock().control(&DaemonMessage::ContainerFound { request, container: None });
        return;
    };
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let mut registry = shared.lock().registry.clone();
        let refreshed = registry.refresh(&DockerCli { program });
        let mut state = shared.lock();
        if refreshed.is_ok() {
            state.registry = registry;
        }
        let container = refreshed.ok().and_then(|()| containers::for_dir(state.registry.containers(), &dir)).map(|c| ContainerChoice {
            host_folder: c.host_folder.clone(),
            name: c.name.clone(),
            short_id: c.short_id().into(),
            running: c.running,
        });
        state.control(&DaemonMessage::ContainerFound { request, container });
    });
}

/// Start `spec` in the dev container serving `folder`: find it (starting it when stopped), then run the command
/// through `docker exec` with this pane's marker.
fn spawn_in_container(shared: Arc<Shared>, client_id: u64, request: u64, spec: SpawnRequest, folder: PathBuf) {
    tokio::spawn(async move {
        let fail = |shared: &Shared, error: String| shared.lock().control(&DaemonMessage::SpawnFailed { request, error });
        let Some(program) = shared.docker.clone() else {
            return fail(&shared, "Dev containers are not available: Docker is turned off".into());
        };
        let mut registry = shared.lock().registry.clone();
        let looked_up = tokio::task::spawn_blocking(move || {
            let ready = containers::ready(&DockerCli { program }, &mut registry, &folder);
            (registry, ready, folder)
        })
        .await;
        let Ok((registry, ready, folder)) = looked_up else { return fail(&shared, "the Docker lookup failed".into()) };
        shared.lock().registry = registry;
        let container = match ready {
            Ok(container) => container,
            Err(error) => return fail(&shared, error),
        };
        let Some(inner_cwd) = to_container(&container.mounts, &spec.cwd) else {
            return fail(&shared, format!("{} is not inside the dev container {}", spec.cwd.display(), container.name));
        };
        let cwd = spec.cwd.clone();
        let (program, args) = (spec.program.clone(), spec.args.clone());
        let instance = shared.instance.clone();
        start_pane(
            &shared,
            client_id,
            request,
            spec,
            cwd,
            |id| containers::exec_command(&container, &inner_cwd, &program, &args, &marker(&instance, id)),
            Some(folder),
        );
    });
}

/// Start a pane for `spec` in `cwd`, running the command `command` builds from the new pane's id.
fn start_pane(
    shared: &Arc<Shared>,
    client_id: u64,
    request: u64,
    spec: SpawnRequest,
    cwd: PathBuf,
    command: impl FnOnce(PaneId) -> (String, Vec<String>),
    container: Option<PathBuf>,
) {
    let session_arg = claude_session(&spec.program, &spec.args).map(String::from);
    let mut state = shared.lock();
    let id = state.next_pane;
    let (program, args) = command(id);
    let pty_spec = SpawnSpec {
        program,
        args,
        cwd: cwd.clone(),
        env: Vec::new(),
        base_env: Some(shared.base_env.clone()),
        cols: spec.cols,
        rows: spec.rows,
    };
    let (pty, reader) = match PtyProcess::spawn(&pty_spec) {
        Ok(pair) => pair,
        Err(err) => {
            state.control(&DaemonMessage::SpawnFailed { request, error: format!("{err:#}") });
            return;
        }
    };
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
        container,
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
            container_id: None,
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
            docker: None,
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

    #[test]
    fn closing_a_container_pane_signals_only_its_verified_processes() {
        use claude_index::host::Marked;
        let marked = [
            Marked { pid: 10, value: "i:3".into(), start: Some("5".into()) },
            Marked { pid: 11, value: "i:3".into(), start: None },
            Marked { pid: 12, value: "i:30".into(), start: Some("6".into()) },
            Marked { pid: 13, value: "other:3".into(), start: Some("7".into()) },
        ];
        assert_eq!(marked_targets(&marked, "i:3"), vec![take_over::Target { pid: 10, start: "5".into() }]);
    }

    #[test]
    fn marks_are_read_only_for_this_daemons_live_panes() {
        use claude_index::host::Marked;
        let marked = [
            Marked { pid: 10, value: "i:3".into(), start: None },
            Marked { pid: 11, value: "i:9".into(), start: None },
            Marked { pid: 12, value: "j:3".into(), start: None },
            Marked { pid: 13, value: "i:x".into(), start: None },
        ];
        assert_eq!(pane_marks(&marked, "i", &[3, 4]), HashMap::from([(10, 3)]));
    }
}
