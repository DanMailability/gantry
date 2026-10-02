//! The sidebar's Active list as plain rows: every pane of the layout and every Claude session running outside
//! Gantry, under the project it belongs to. Closed sessions live in the recent sessions pop-up (`recents`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus};

use crate::fuzzy::Fuzzy;
use crate::split_tree::NodeId;

/// How a session's row is coloured: what it needs from the user, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Waiting for an answer.
    NeedsInput,
    /// Busy.
    Working,
    /// Idle with a result the user has not seen.
    Done,
    /// Idle (or not reporting yet), nothing new.
    Idle,
    /// Running outside Gantry.
    Elsewhere,
    /// Not running.
    Closed,
}

/// Live entries of a project, by tone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToneCounts {
    /// Sessions needing input.
    pub needs_input: usize,
    /// Busy sessions.
    pub working: usize,
    /// Sessions with an unseen result.
    pub done: usize,
    /// Idle sessions.
    pub idle: usize,
    /// Sessions running outside Gantry.
    pub elsewhere: usize,
}

impl ToneCounts {
    fn add(&mut self, tone: Tone) {
        match tone {
            Tone::NeedsInput => self.needs_input += 1,
            Tone::Working => self.working += 1,
            Tone::Done => self.done += 1,
            Tone::Idle => self.idle += 1,
            Tone::Elsewhere => self.elsewhere += 1,
            Tone::Closed => {}
        }
    }
}

/// A pane of the layout, for the list (Claude panes and shells).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    /// Its leaf.
    pub leaf: NodeId,
    /// Directory it was started in.
    pub cwd: PathBuf,
    /// Pane title.
    pub title: String,
    /// Claude session it runs (or last ran); `None` for other programs.
    pub session: Option<String>,
    /// It runs Claude Code.
    pub claude: bool,
    /// Its process ended; the pane shows Continue / Restart.
    pub ended: bool,
}

/// What keeps an Active entry's place in the list: its leaf, or (outside Gantry) its session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntryKey {
    /// A pane of the layout (survives `/clear`, Continue and Resume in the same leaf).
    Leaf(NodeId),
    /// A session running outside Gantry.
    Session(String),
}

/// Arrival order of the listed entries: rows inside a project never move, and new ones go on top.
#[derive(Debug, Default)]
pub struct Arrivals {
    seq: HashMap<EntryKey, u64>,
    next: u64,
}

impl Arrivals {
    /// Number the keys not seen before, in the given order, and forget keys no longer listed.
    pub fn update(&mut self, keys: &[EntryKey]) {
        let listed: HashSet<&EntryKey> = keys.iter().collect();
        self.seq.retain(|key, _| listed.contains(key));
        for key in keys {
            if !self.seq.contains_key(key) {
                self.next += 1;
                self.seq.insert(key.clone(), self.next);
            }
        }
    }

    /// When `key` arrived (higher = later; 0 = unknown).
    pub fn of(&self, key: &EntryKey) -> u64 {
        self.seq.get(key).copied().unwrap_or(0)
    }
}

/// Everything the Active list is built from.
pub struct ActiveInput<'a> {
    /// Claude sessions.
    pub sessions: &'a [&'a SessionInfo],
    /// Panes of the layout.
    pub panes: &'a [PaneEntry],
    /// Home directory, shown as `~`.
    pub home: Option<&'a Path>,
    /// Fuzzy filter text (blank = everything).
    pub filter: &'a str,
    /// Projects the user collapsed.
    pub toggled: &'a HashSet<PathBuf>,
    /// External session whose details are open.
    pub details: Option<&'a str>,
    /// Sessions with an alert the user has not seen.
    pub unseen: &'a HashSet<String>,
    /// The project a directory belongs to (see [`project_root`]).
    pub project_of: &'a dyn Fn(&Path) -> PathBuf,
    /// Current time, ms since the Unix epoch.
    pub now_ms: u64,
}

/// Project roots of directories, cached (finding one stats parent directories).
#[derive(Debug, Default)]
pub struct ProjectRoots {
    home: Option<PathBuf>,
    cache: std::cell::RefCell<HashMap<PathBuf, PathBuf>>,
}

impl ProjectRoots {
    /// Roots below `home`.
    pub fn new(home: Option<PathBuf>) -> ProjectRoots {
        ProjectRoots { home, cache: Default::default() }
    }

    /// The project `cwd` belongs to: its git repository, else the directory itself.
    pub fn of(&self, cwd: &Path) -> PathBuf {
        if let Some(root) = self.cache.borrow().get(cwd) {
            return root.clone();
        }
        let root = project_root(cwd, self.home.as_deref(), |dir| dir.join(".git").exists());
        self.cache.borrow_mut().insert(cwd.to_path_buf(), root.clone());
        root
    }

    /// Forget every root, so repositories created or removed since are noticed.
    pub fn clear(&mut self) {
        self.cache.get_mut().clear();
    }
}

/// One sidebar row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// What it shows.
    pub kind: RowKind,
}

/// What a row shows.
#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// A project heading.
    Project {
        /// Project directory.
        path: PathBuf,
        /// Displayed name.
        label: String,
        /// Its entries are shown.
        expanded: bool,
        /// Its live entries by tone.
        counts: ToneCounts,
    },
    /// A Claude session (in a pane of the layout, or running outside Gantry).
    Session {
        /// The session.
        session: SessionInfo,
        /// Its colour.
        tone: Tone,
        /// Right-hand note: what it is doing and since when.
        note: String,
        /// The leaf showing it, if any.
        leaf: Option<NodeId>,
    },
    /// A pane the session list does not know: a shell, or a Claude pane before its first transcript.
    Pane {
        /// The pane.
        entry: PaneEntry,
        /// Its colour.
        tone: Tone,
        /// Right-hand note.
        note: String,
    },
    /// Details of an external session.
    Detail {
        /// Session id.
        session: String,
        /// Text.
        text: String,
    },
    /// "Take over…" under an outside session's details.
    TakeOver {
        /// Session id.
        session: String,
        /// Its directory (where it is resumed).
        cwd: PathBuf,
    },
}

/// Identifies a row across rebuilds (for keeping the selection).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// Project heading.
    Project(PathBuf),
    /// Session.
    Session(String),
    /// A pane the session list does not know.
    Leaf(NodeId),
    /// Details of a session.
    Detail(String),
    /// "Take over…" of a session.
    TakeOver(String),
}

/// What activating (click / Enter) a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Activation {
    /// Show the pane running this session.
    FocusPane(PaneId),
    /// Show this leaf.
    FocusLeaf(NodeId),
    /// Resume a closed session.
    Resume {
        /// Session id.
        session: String,
        /// Its directory.
        cwd: PathBuf,
    },
    /// Show or hide an external session's details.
    ToggleDetails(String),
    /// Stop the outside process running a session and resume it here.
    TakeOver {
        /// Session id.
        session: String,
        /// Its directory.
        cwd: PathBuf,
    },
    /// Expand or collapse a project.
    ToggleProject(PathBuf),
    /// Nothing (headings; a closed session whose directory is gone).
    Nothing,
}

impl Row {
    /// Identity of this row.
    pub fn key(&self) -> RowKey {
        match &self.kind {
            RowKind::Project { path, .. } => RowKey::Project(path.clone()),
            RowKind::Session { session, .. } => RowKey::Session(session.id.clone()),
            RowKind::Pane { entry, .. } => RowKey::Leaf(entry.leaf),
            RowKind::Detail { session, .. } => RowKey::Detail(session.clone()),
            RowKind::TakeOver { session, .. } => RowKey::TakeOver(session.clone()),
        }
    }

    /// Whether keyboard selection can land here (details are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Detail { .. })
    }

    /// What activating this row does.
    pub fn activation(&self) -> Activation {
        match &self.kind {
            RowKind::Project { path, .. } => Activation::ToggleProject(path.clone()),
            RowKind::Pane { entry, .. } => Activation::FocusLeaf(entry.leaf),
            RowKind::Session { session, leaf, .. } => match (session.state, leaf) {
                (SessionState::External { .. }, _) => Activation::ToggleDetails(session.id.clone()),
                (_, Some(leaf)) => Activation::FocusLeaf(*leaf),
                (SessionState::Managed { pane }, None) => Activation::FocusPane(pane),
                (SessionState::Closed, None) if session.cwd_exists => {
                    Activation::Resume { session: session.id.clone(), cwd: session.cwd.clone() }
                }
                (SessionState::Closed, None) => Activation::Nothing,
            },
            RowKind::TakeOver { session, cwd } => Activation::TakeOver { session: session.clone(), cwd: cwd.clone() },
            RowKind::Detail { .. } => Activation::Nothing,
        }
    }
}

/// Tone of a session in a Gantry pane from its status; `unseen` = it has an alert the user has not seen.
pub fn status_tone(status: Option<&SessionStatus>, unseen: bool) -> Tone {
    match status {
        Some(SessionStatus::NeedsInput { .. }) => Tone::NeedsInput,
        Some(SessionStatus::Busy) => Tone::Working,
        _ if unseen => Tone::Done,
        _ => Tone::Idle,
    }
}

/// Tone of any session.
pub fn tone(session: &SessionInfo, unseen: bool) -> Tone {
    match session.state {
        SessionState::Managed { .. } => status_tone(session.status.as_ref(), unseen),
        SessionState::External { .. } => Tone::Elsewhere,
        SessionState::Closed => Tone::Closed,
    }
}

/// The project `cwd` belongs to: its nearest enclosing repository (`is_repo`), else `cwd` itself. Home and its
/// ancestors never count as an enclosing repository, so a dotfiles repository does not swallow everything under `~`.
pub fn project_root(cwd: &Path, home: Option<&Path>, is_repo: impl Fn(&Path) -> bool) -> PathBuf {
    cwd.ancestors()
        .take_while(|dir| *dir == cwd || home.is_none_or(|home| !home.starts_with(dir)))
        .find(|dir| is_repo(dir))
        .unwrap_or(cwd)
        .to_path_buf()
}

/// A session's display title ("New session" until it has one).
pub fn session_title(session: &SessionInfo) -> &str {
    if session.title.is_empty() { "New session" } else { &session.title }
}

/// `path` for display, with the home directory as `~`.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// Compact time since `then_ms`: `now`, `5m`, `3h`, `2d`, `6w`.
pub fn age(now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    let (hour, day) = (60, 60 * 24);
    match minutes {
        0 => "now".into(),
        m if m < hour => format!("{m}m"),
        m if m < day => format!("{}h", m / hour),
        m if m < 14 * day => format!("{}d", m / day),
        m => format!("{}w", m / (7 * day)),
    }
}

/// Index of the row identified by `key`, if it is still there.
pub fn find(rows: &[Row], key: &RowKey) -> Option<usize> {
    rows.iter().position(|row| &row.key() == key)
}

/// What one Active row stands for, before grouping.
struct Entry<'a> {
    key: EntryKey,
    cwd: &'a Path,
    title: &'a str,
    /// `last_active_ms`; panes the session list does not know count as active now.
    activity: u64,
    kind: EntryKind<'a>,
}

enum EntryKind<'a> {
    Session { session: &'a SessionInfo, leaf: Option<&'a PaneEntry> },
    Pane(&'a PaneEntry),
}

/// The Active list.
pub fn active(input: &ActiveInput<'_>, arrivals: &mut Arrivals) -> Vec<Row> {
    let by_id: HashMap<&str, &SessionInfo> = input.sessions.iter().map(|s| (s.id.as_str(), *s)).collect();
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let mut in_leaves: HashSet<&str> = HashSet::new();
    for pane in input.panes {
        match pane.session.as_deref().and_then(|id| by_id.get(id).copied()) {
            Some(session) => {
                in_leaves.insert(session.id.as_str());
                let title = session_title(session);
                let kind = EntryKind::Session { session, leaf: Some(pane) };
                entries.push(Entry { key: EntryKey::Leaf(pane.leaf), cwd: &session.cwd, title, activity: session.last_active_ms, kind });
            }
            None => {
                let title = if pane.claude { "New session" } else { pane.title.as_str() };
                entries.push(Entry { key: EntryKey::Leaf(pane.leaf), cwd: &pane.cwd, title, activity: u64::MAX, kind: EntryKind::Pane(pane) });
            }
        }
    }
    for session in input.sessions.iter().copied() {
        if session.state == SessionState::Closed || in_leaves.contains(session.id.as_str()) {
            continue;
        }
        let kind = EntryKind::Session { session, leaf: None };
        entries.push(Entry { key: EntryKey::Session(session.id.clone()), cwd: &session.cwd, title: session_title(session), activity: session.last_active_ms, kind });
    }
    // New keys are numbered oldest first, so that at startup newer leaves sit above older ones and leaves above
    // outside sessions (which go by activity).
    let mut keys: Vec<(&EntryKey, (bool, u64))> = entries
        .iter()
        .map(|entry| match &entry.key {
            EntryKey::Leaf(leaf) => (&entry.key, (true, *leaf)),
            EntryKey::Session(_) => (&entry.key, (false, entry.activity)),
        })
        .collect();
    keys.sort_by_key(|(_, order)| *order);
    arrivals.update(&keys.into_iter().map(|(key, _)| key.clone()).collect::<Vec<_>>());

    let mut fuzzy = Fuzzy::new(input.filter);
    let filtering = fuzzy.is_some();
    let mut by_project: HashMap<PathBuf, Vec<Entry<'_>>> = HashMap::new();
    for entry in entries {
        if keep(&mut fuzzy, entry.title, entry.cwd, input.home) {
            by_project.entry((input.project_of)(entry.cwd)).or_default().push(entry);
        }
    }
    let mut projects: Vec<(PathBuf, Vec<Entry<'_>>)> = by_project.into_iter().collect();
    for (_, entries) in &mut projects {
        entries.sort_by_key(|entry| std::cmp::Reverse(arrivals.of(&entry.key)));
    }
    let latest = |entries: &[Entry<'_>]| entries.iter().map(|entry| entry.activity).max().unwrap_or(0);
    projects.sort_by(|(a_path, a), (b_path, b)| latest(b).cmp(&latest(a)).then_with(|| a_path.cmp(b_path)));
    let paths: Vec<PathBuf> = projects.iter().map(|(path, _)| path.clone()).collect();
    let labels = labels(&paths, input.home);

    let mut rows = Vec::new();
    for (path, entries) in projects {
        let rendered: Vec<Row> = entries.iter().map(|entry| entry_row(entry, input)).collect();
        let mut counts = ToneCounts::default();
        for row in &rendered {
            match &row.kind {
                RowKind::Session { tone, .. } => counts.add(*tone),
                RowKind::Pane { entry, tone, .. } if entry.claude => counts.add(*tone),
                _ => {}
            }
        }
        let expanded = filtering || !input.toggled.contains(&path);
        rows.push(Row { kind: RowKind::Project { label: labels[&path].clone(), path, expanded, counts } });
        if expanded {
            for row in rendered {
                let details = match &row.kind {
                    RowKind::Session { session, .. } if input.details == Some(session.id.as_str()) => Some(session.clone()),
                    _ => None,
                };
                rows.push(row);
                if let Some(session) = details {
                    push_details(&mut rows, &session);
                }
            }
        }
    }
    rows
}

/// The row of one entry: its tone and its note.
fn entry_row(entry: &Entry<'_>, input: &ActiveInput<'_>) -> Row {
    match &entry.kind {
        EntryKind::Session { session, leaf } => {
            let age = age(input.now_ms, session.last_active_ms);
            let (tone, note) = match (session.state, leaf) {
                (SessionState::Closed, Some(pane)) if pane.ended => (Tone::Closed, "Exited · tab open".to_string()),
                (SessionState::Closed, _) => (Tone::Idle, "Starting".to_string()),
                _ => {
                    let tone = tone(session, input.unseen.contains(&session.id));
                    (tone, format!("{} · {age}", tone_word(tone)))
                }
            };
            Row { kind: RowKind::Session { session: (*session).clone(), tone, note, leaf: leaf.map(|pane| pane.leaf) } }
        }
        EntryKind::Pane(pane) => {
            let (tone, note) = match (pane.claude, pane.ended) {
                (true, true) => (Tone::Closed, "Exited · tab open"),
                (true, false) => (Tone::Idle, "Idle"),
                (false, true) => (Tone::Closed, "Exited"),
                (false, false) => (Tone::Idle, "shell"),
            };
            Row { kind: RowKind::Pane { entry: (*pane).clone(), tone, note: note.into() } }
        }
    }
}

/// The word for a live tone in a row's note.
fn tone_word(tone: Tone) -> &'static str {
    match tone {
        Tone::NeedsInput => "Needs input",
        Tone::Working => "Working",
        Tone::Done => "Done",
        Tone::Idle => "Idle",
        Tone::Elsewhere => "Elsewhere",
        Tone::Closed => "Closed",
    }
}

/// An external session's details and, for host sessions whose folder exists, "Take over…".
fn push_details(rows: &mut Vec<Row>, session: &SessionInfo) {
    if let Some(text) = detail_text(session) {
        rows.push(Row { kind: RowKind::Detail { session: session.id.clone(), text } });
        if matches!(session.state, SessionState::External { pid: Some(_) }) && session.cwd_exists {
            rows.push(Row { kind: RowKind::TakeOver { session: session.id.clone(), cwd: session.cwd.clone() } });
        }
    }
}

/// Index of the row showing `leaf` (its session's row, or its pane's).
pub fn row_of_leaf(rows: &[Row], leaf: NodeId) -> Option<usize> {
    rows.iter().position(|row| match &row.kind {
        RowKind::Session { leaf: Some(shown), .. } => *shown == leaf,
        RowKind::Pane { entry, .. } => entry.leaf == leaf,
        _ => false,
    })
}

/// Project names: the folder's name, or its whole displayed path where two projects share a name.
pub(crate) fn labels(paths: &[PathBuf], home: Option<&Path>) -> HashMap<PathBuf, String> {
    let short = |path: &Path| match path.file_name() {
        Some(name) if Some(path) != home => name.to_string_lossy().into_owned(),
        _ => display_path(path, home),
    };
    let mut uses: HashMap<String, HashSet<&Path>> = HashMap::new();
    for path in paths {
        uses.entry(short(path)).or_default().insert(path);
    }
    paths
        .iter()
        .map(|path| {
            let name = short(path);
            let label = if uses[&name].len() > 1 { display_path(path, home) } else { name };
            (path.clone(), label)
        })
        .collect()
}

pub(crate) fn newest_first(a: &&SessionInfo, b: &&SessionInfo) -> std::cmp::Ordering {
    b.last_active_ms.cmp(&a.last_active_ms).then_with(|| a.id.cmp(&b.id))
}

/// Whether an entry passes the filter (matched against its title and displayed directory).
pub(crate) fn keep(fuzzy: &mut Option<Fuzzy>, title: &str, cwd: &Path, home: Option<&Path>) -> bool {
    fuzzy.as_mut().is_none_or(|fuzzy| fuzzy.score(&format!("{title} {}", display_path(cwd, home))).is_some())
}

fn detail_text(session: &SessionInfo) -> Option<String> {
    match session.state {
        SessionState::External { .. } if session.container.is_some() => Some("Running in a dev container".into()),
        SessionState::External { pid: Some(pid) } => Some(format!("Running outside Gantry (pid {pid})")),
        SessionState::External { pid: None } => Some("Running in a container".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::{ContainerPlace, SessionStatus};

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    const NOW: u64 = 100 * DAY_MS;
    const HOUR_MS: u64 = 60 * 60 * 1000;

    fn session(id: &str, cwd: &str, state: SessionState, last_active_ms: u64) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            cwd: cwd.into(),
            title: format!("title {id}"),
            last_active_ms,
            state,
            cwd_exists: true,
            status: None,
            status_since_ms: 0,
            container: None,
        }
    }

    fn managed(id: &str, cwd: &str, last: u64) -> SessionInfo {
        session(id, cwd, SessionState::Managed { pane: 1 }, last)
    }

    fn closed(id: &str, cwd: &str, last: u64) -> SessionInfo {
        session(id, cwd, SessionState::Closed, last)
    }

    fn external(id: &str, cwd: &str, last: u64) -> SessionInfo {
        session(id, cwd, SessionState::External { pid: Some(42) }, last)
    }

    fn with(mut session: SessionInfo, status: SessionStatus) -> SessionInfo {
        session.status = Some(status);
        session
    }

    /// Repositories of the fixtures: every `/r/<name>` directory.
    fn project_of(cwd: &Path) -> PathBuf {
        project_root(cwd, Some(Path::new("/home/me")), |dir| dir.parent() == Some(Path::new("/r")))
    }

    fn pane(leaf: NodeId, cwd: &str, session: Option<&str>) -> PaneEntry {
        PaneEntry { leaf, cwd: cwd.into(), title: format!("pane {leaf}"), session: session.map(String::from), claude: session.is_some(), ended: false }
    }

    fn shell_pane(leaf: NodeId, cwd: &str) -> PaneEntry {
        PaneEntry { claude: false, ..pane(leaf, cwd, None) }
    }

    #[derive(Default)]
    struct Fixture {
        sessions: Vec<SessionInfo>,
        panes: Vec<PaneEntry>,
        filter: String,
        toggled: HashSet<PathBuf>,
        details: Option<String>,
        unseen: HashSet<String>,
        arrivals: Arrivals,
    }

    impl Fixture {
        fn of(sessions: Vec<SessionInfo>, panes: Vec<PaneEntry>) -> Fixture {
            Fixture { sessions, panes, ..Fixture::default() }
        }

        fn rows(&mut self) -> Vec<Row> {
            let refs: Vec<&SessionInfo> = self.sessions.iter().collect();
            let input = ActiveInput {
                sessions: &refs,
                panes: &self.panes,
                home: Some(Path::new("/home/me")),
                filter: &self.filter,
                toggled: &self.toggled,
                details: self.details.as_deref(),
                unseen: &self.unseen,
                project_of: &project_of,
                now_ms: NOW,
            };
            active(&input, &mut self.arrivals)
        }

        fn outline(&mut self) -> Vec<String> {
            self.rows()
                .iter()
                .map(|row| match &row.kind {
                    RowKind::Project { label, expanded, .. } => format!("{} {label}", if *expanded { "▾" } else { "▸" }),
                    RowKind::Session { session, note, .. } => format!("  {} ({note})", session.id),
                    RowKind::Pane { entry, note, .. } => format!("  leaf {} ({note})", entry.leaf),
                    RowKind::Detail { text, .. } => format!("    ({text})"),
                    RowKind::TakeOver { .. } => "    Take over…".into(),
                })
                .collect()
        }
    }

    #[test]
    fn active_lists_panes_and_outside_sessions_by_project_and_nothing_closed() {
        let mut ended = pane(4, "/r/app", Some("gone"));
        ended.ended = true;
        let mut fixture = Fixture::of(
            vec![
                with(session("m", "/r/app", SessionState::Managed { pane: 1 }, NOW - 60_000), SessionStatus::Busy),
                closed("gone", "/r/app", NOW - HOUR_MS),
                closed("history", "/r/app", NOW - 10),
                external("e", "/r/web", NOW - 2 * HOUR_MS),
            ],
            vec![pane(1, "/r/app", Some("m")), pane(2, "/r/app", Some("fresh")), shell_pane(3, "/r/app/src"), ended],
        );
        assert_eq!(fixture.outline(), vec![
            "▾ app",
            "  gone (Exited · tab open)",
            "  leaf 3 (shell)",
            "  leaf 2 (Idle)",
            "  m (Working · 1m)",
            "▾ web",
            "  e (Elsewhere · 2h)",
        ]);
    }

    #[test]
    fn an_exited_leaf_whose_session_is_known_shows_as_that_session() {
        let mut ended = pane(4, "/r/app", Some("gone"));
        ended.ended = true;
        let mut fixture = Fixture::of(vec![closed("gone", "/r/app", NOW - HOUR_MS)], vec![ended]);
        let rows = fixture.rows();
        let RowKind::Session { tone, note, leaf, .. } = &rows[1].kind else { panic!("{rows:?}") };
        assert_eq!((*tone, note.as_str(), *leaf), (Tone::Closed, "Exited · tab open", Some(4)));
        assert_eq!(rows[1].activation(), Activation::FocusLeaf(4));
        // While Continue restarts it, it is starting, not exited.
        fixture.panes[0].ended = false;
        let rows = fixture.rows();
        let RowKind::Session { tone, note, .. } = &rows[1].kind else { panic!("{rows:?}") };
        assert_eq!((*tone, note.as_str()), (Tone::Idle, "Starting"));
    }

    #[test]
    fn a_session_running_in_a_leaf_is_listed_once() {
        // Just taken over: the daemon reports it Managed, and a leaf names it.
        let mut fixture = Fixture::of(vec![managed("t", "/r/app", NOW - 1)], vec![pane(7, "/r/app", Some("t"))]);
        assert_eq!(fixture.outline(), vec!["▾ app", "  t (Idle · now)"]);
        let rows = fixture.rows();
        assert_eq!(rows[1].activation(), Activation::FocusLeaf(7));
    }

    #[test]
    fn rows_inside_a_project_keep_their_place_and_new_ones_land_on_top() {
        let mut fixture = Fixture::of(
            vec![managed("a", "/r/app", NOW - 3 * HOUR_MS), managed("b", "/r/app", NOW - HOUR_MS)],
            vec![pane(1, "/r/app", Some("a")), pane(2, "/r/app", Some("b"))],
        );
        assert_eq!(fixture.outline(), vec!["▾ app", "  b (Idle · 1h)", "  a (Idle · 3h)"]);
        // "a" becomes busy and more recent: it does not move.
        fixture.sessions[0] = with(managed("a", "/r/app", NOW), SessionStatus::Busy);
        assert_eq!(fixture.outline(), vec!["▾ app", "  b (Idle · 1h)", "  a (Working · now)"]);
        // A new outside session arrives: it goes on top, whatever its activity.
        fixture.sessions.push(external("x", "/r/app", NOW - 9 * HOUR_MS));
        assert_eq!(fixture.outline()[1], "  x (Elsewhere · 9h)");
    }

    #[test]
    fn a_leaf_keeps_its_place_when_its_session_changes() {
        let mut fixture = Fixture::of(
            vec![managed("a", "/r/app", 1), managed("b", "/r/app", 2)],
            vec![pane(1, "/r/app", Some("a")), pane(2, "/r/app", Some("b"))],
        );
        let before = fixture.outline();
        // `/clear` in leaf 1: a new session id, same leaf.
        fixture.sessions[0] = managed("a2", "/r/app", 3);
        fixture.panes[0].session = Some("a2".into());
        let after = fixture.outline();
        assert_eq!(after.iter().position(|r| r.starts_with("  a2")), before.iter().position(|r| r.starts_with("  a ")));
    }

    #[test]
    fn filtering_does_not_reset_the_order() {
        let mut fixture = Fixture::of(
            vec![managed("a", "/r/app", NOW - 1), managed("b", "/r/app", NOW - 2)],
            vec![pane(1, "/r/app", Some("a")), pane(2, "/r/app", Some("b"))],
        );
        let before = fixture.outline();
        fixture.filter = "title b".into();
        assert_eq!(fixture.outline(), vec!["▾ app", "  b (Idle · now)"]);
        fixture.filter.clear();
        assert_eq!(fixture.outline(), before);
    }

    #[test]
    fn at_startup_newer_leaves_come_first_then_outside_sessions_newest_first() {
        let mut fixture = Fixture::of(
            vec![managed("one", "/r/app", 5), managed("two", "/r/app", 4), external("x", "/r/app", 3), external("y", "/r/app", 9)],
            vec![pane(1, "/r/app", Some("one")), pane(2, "/r/app", Some("two"))],
        );
        let ids: Vec<String> = fixture.outline().into_iter().skip(1).map(|r| r.split_whitespace().next().unwrap().to_string()).collect();
        assert_eq!(ids, ["two", "one", "y", "x"]);
    }

    #[test]
    fn projects_follow_their_latest_activity() {
        let mut fixture = Fixture::of(
            vec![managed("old", "/r/a", NOW - 5 * HOUR_MS), managed("new", "/r/b", NOW - HOUR_MS)],
            vec![pane(1, "/r/a", Some("old")), pane(2, "/r/b", Some("new"))],
        );
        assert_eq!(fixture.outline(), vec!["▾ b", "  new (Idle · 1h)", "▾ a", "  old (Idle · 5h)"]);
        fixture.sessions[0].last_active_ms = NOW;
        assert_eq!(fixture.outline()[0], "▾ a");
    }

    #[test]
    fn project_headers_count_claude_entries_by_tone_but_not_shells() {
        let mut fixture = Fixture::of(
            vec![
                with(managed("n", "/r/p", 6), SessionStatus::NeedsInput { reason: None }),
                with(managed("d", "/r/p", 4), SessionStatus::Idle),
                external("e", "/r/p", 2),
            ],
            vec![pane(1, "/r/p", Some("n")), pane(2, "/r/p", Some("d")), pane(3, "/r/p", Some("new")), shell_pane(4, "/r/p")],
        );
        fixture.unseen.insert("d".into());
        let rows = fixture.rows();
        let RowKind::Project { counts, .. } = &rows[0].kind else { panic!("{rows:?}") };
        assert_eq!(*counts, ToneCounts { needs_input: 1, working: 0, done: 1, idle: 1, elsewhere: 1 });
    }

    #[test]
    fn a_collapsed_project_hides_its_rows_until_filtering() {
        let mut fixture = Fixture::of(vec![], vec![shell_pane(1, "/r/p"), shell_pane(2, "/r/q")]);
        fixture.toggled.insert("/r/p".into());
        assert_eq!(fixture.outline(), vec!["▸ p", "▾ q", "  leaf 2 (shell)"]);
        fixture.filter = "pane 1".into();
        assert_eq!(fixture.outline(), vec!["▾ p", "  leaf 1 (shell)"]);
    }

    #[test]
    fn an_exited_shell_stays_listed() {
        let mut shell = shell_pane(5, "/r/p");
        shell.ended = true;
        let mut fixture = Fixture::of(vec![], vec![shell]);
        assert_eq!(fixture.outline(), vec!["▾ p", "  leaf 5 (Exited)"]);
    }

    #[test]
    fn the_row_of_a_leaf_is_found_for_sessions_and_panes() {
        let mut fixture = Fixture::of(vec![managed("m", "/r/p", 1)], vec![pane(1, "/r/p", Some("m")), shell_pane(2, "/r/p")]);
        let rows = fixture.rows();
        assert!(matches!(&rows[row_of_leaf(&rows, 1).unwrap()].kind, RowKind::Session { session, .. } if session.id == "m"));
        assert!(matches!(&rows[row_of_leaf(&rows, 2).unwrap()].kind, RowKind::Pane { entry, .. } if entry.leaf == 2));
        assert_eq!(row_of_leaf(&rows, 9), None);
    }

    #[test]
    fn take_over_is_offered_for_outside_host_sessions_whose_directory_exists() {
        let mut fixture = Fixture::of(vec![external("e", "/p", 3), SessionInfo { cwd_exists: false, ..external("gone", "/p", 2) }], vec![]);
        fixture.details = Some("gone".into());
        assert!(!fixture.outline().contains(&"    Take over…".to_string()));
        fixture.details = Some("e".into());
        let rows = fixture.rows();
        let index = find(&rows, &RowKey::TakeOver("e".into())).unwrap();
        assert!(rows[index].selectable());
        assert_eq!(rows[index].activation(), Activation::TakeOver { session: "e".into(), cwd: "/p".into() });
    }

    #[test]
    fn details_appear_under_their_external_session() {
        let mut boxed = external("box", "/p", 1);
        boxed.container = Some(ContainerPlace { host_folder: Some("/p".into()), path: "/workspaces/p".into() });
        let mut fixture = Fixture::of(vec![external("e", "/p", 2), boxed], vec![]);
        fixture.details = Some("e".into());
        assert_eq!(fixture.outline()[2..4], ["    (Running outside Gantry (pid 42))".to_string(), "    Take over…".to_string()]);
        fixture.details = Some("box".into());
        assert!(fixture.outline().contains(&"    (Running in a dev container)".to_string()));
    }

    #[test]
    fn activation_follows_the_row() {
        let mut fixture = Fixture::of(
            vec![external("e", "/r/p", 2), managed("orphan", "/r/p", 1)],
            vec![shell_pane(9, "/r/p"), pane(8, "/r/p", Some("unknown"))],
        );
        let rows = fixture.rows();
        let by = |key: RowKey| rows[find(&rows, &key).unwrap()].activation();
        assert_eq!(by(RowKey::Leaf(9)), Activation::FocusLeaf(9));
        assert_eq!(by(RowKey::Leaf(8)), Activation::FocusLeaf(8));
        assert_eq!(by(RowKey::Session("e".into())), Activation::ToggleDetails("e".into()));
        assert_eq!(by(RowKey::Session("orphan".into())), Activation::FocusPane(1));
        assert_eq!(by(RowKey::Project("/r/p".into())), Activation::ToggleProject("/r/p".into()));
    }

    #[test]
    fn projects_with_the_same_name_show_their_paths() {
        let mut fixture = Fixture::of(vec![], vec![shell_pane(1, "/r/api"), shell_pane(2, "/home/me/work/api")]);
        let outline = fixture.outline();
        assert!(outline.contains(&"▾ /r/api".to_string()) && outline.contains(&"▾ ~/work/api".to_string()), "{outline:?}");
    }

    #[test]
    fn tone_follows_state_status_and_whether_the_result_was_seen() {
        let idle = with(managed("i", "/a", 1), SessionStatus::Idle);
        assert_eq!(tone(&idle, false), Tone::Idle);
        assert_eq!(tone(&idle, true), Tone::Done);
        assert_eq!(tone(&managed("starting", "/a", 1), false), Tone::Idle);
        assert_eq!(tone(&with(managed("b", "/a", 1), SessionStatus::Busy), true), Tone::Working);
        assert_eq!(tone(&with(managed("n", "/a", 1), SessionStatus::NeedsInput { reason: None }), true), Tone::NeedsInput);
        assert_eq!(tone(&with(external("e", "/a", 1), SessionStatus::NeedsInput { reason: None }), false), Tone::Elsewhere);
        assert_eq!(tone(&closed("c", "/a", 1), true), Tone::Closed);
        assert_eq!(status_tone(None, false), Tone::Idle);
    }

    #[test]
    fn a_project_is_the_nearest_repository_below_home() {
        let home = Some(Path::new("/home/me"));
        let repos = [Path::new("/home/me"), Path::new("/home/me/code/app"), Path::new("/srv")];
        let root = |cwd: &str| project_root(Path::new(cwd), home, |dir| repos.contains(&dir));
        assert_eq!(root("/home/me/code/app/crates/core"), Path::new("/home/me/code/app"));
        assert_eq!(root("/home/me/code/app"), Path::new("/home/me/code/app"));
        // A repository at (or above) home does not swallow everything under it.
        assert_eq!(root("/home/me/notes"), Path::new("/home/me/notes"));
        assert_eq!(root("/home/me"), Path::new("/home/me"));
        assert_eq!(root("/srv/www"), Path::new("/srv"));
        assert_eq!(root("/tmp/x"), Path::new("/tmp/x"));
    }

    #[test]
    fn helpers_format_titles_paths_and_ages() {
        let mut untitled = closed("u", "/", 0);
        untitled.title.clear();
        assert_eq!(session_title(&untitled), "New session");
        let home = Some(Path::new("/home/me"));
        assert_eq!(display_path(Path::new("/home/me"), home), "~");
        assert_eq!(display_path(Path::new("/home/me/x/y"), home), "~/x/y");
        assert_eq!(display_path(Path::new("/home/meow"), home), "/home/meow");
        let minute = 60_000;
        assert_eq!([0, 5 * minute, 3 * 60 * minute, 2 * DAY_MS, 42 * DAY_MS].map(|ago| age(NOW, NOW - ago)), ["now", "5m", "3h", "2d", "6w"]);
    }
}
