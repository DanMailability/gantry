//! The sidebar's session list as plain rows: the Projects view (sessions under the repository they ran in) and the
//! Status view (sessions grouped by state, most urgent first). Both show what is live plus a few recent closed
//! sessions; the fuzzy filter reaches every session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus};

use crate::fuzzy::Fuzzy;
use crate::split_tree::NodeId;

/// Closed sessions count as recent for this many days.
pub const RECENT_DAYS: u64 = 3;
/// Recent closed sessions shown per project.
pub const RECENT_PER_PROJECT: usize = 8;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// A shell pane, for the list (shells are not in the daemon's session list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEntry {
    /// Its leaf.
    pub leaf: NodeId,
    /// Directory it was started in.
    pub cwd: PathBuf,
    /// Pane title.
    pub title: String,
}

/// Everything the list is built from.
pub struct TreeInput<'a> {
    /// Claude sessions.
    pub sessions: &'a [&'a SessionInfo],
    /// Shell panes.
    pub shells: &'a [ShellEntry],
    /// Home directory, shown as `~`.
    pub home: Option<&'a Path>,
    /// Fuzzy filter text (blank = the default, present-focused list).
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

/// One sidebar row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// What it shows.
    pub kind: RowKind,
}

/// What a row shows.
#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// Section heading (Status view, and "Shells").
    Group {
        /// Heading.
        label: &'static str,
        /// Entries in the section.
        count: usize,
    },
    /// A project heading (Projects view).
    Project {
        /// Project directory.
        path: PathBuf,
        /// Displayed name.
        label: String,
        /// Its sessions are shown.
        expanded: bool,
        /// Its live sessions by tone.
        counts: ToneCounts,
    },
    /// A Claude session.
    Session {
        /// The session.
        session: SessionInfo,
        /// Its colour.
        tone: Tone,
        /// Time since last activity.
        age: String,
        /// Its project's name (Status view only).
        project: Option<String>,
    },
    /// A shell pane.
    Shell {
        /// The shell.
        entry: ShellEntry,
        /// Its project's name (Status view only).
        project: Option<String>,
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
    /// Section heading.
    Group(&'static str),
    /// Project heading.
    Project(PathBuf),
    /// Session.
    Session(String),
    /// Shell leaf.
    Shell(NodeId),
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
    /// Show this shell's leaf.
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
            RowKind::Group { label, .. } => RowKey::Group(label),
            RowKind::Project { path, .. } => RowKey::Project(path.clone()),
            RowKind::Session { session, .. } => RowKey::Session(session.id.clone()),
            RowKind::Shell { entry, .. } => RowKey::Shell(entry.leaf),
            RowKind::Detail { session, .. } => RowKey::Detail(session.clone()),
            RowKind::TakeOver { session, .. } => RowKey::TakeOver(session.clone()),
        }
    }

    /// Whether keyboard selection can land here (headings and details are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Group { .. } | RowKind::Detail { .. })
    }

    /// What activating this row does.
    pub fn activation(&self) -> Activation {
        match &self.kind {
            RowKind::Project { path, .. } => Activation::ToggleProject(path.clone()),
            RowKind::Shell { entry, .. } => Activation::FocusLeaf(entry.leaf),
            RowKind::Session { session, .. } => match session.state {
                SessionState::Managed { pane } => Activation::FocusPane(pane),
                SessionState::External { .. } => Activation::ToggleDetails(session.id.clone()),
                SessionState::Closed if session.cwd_exists => {
                    Activation::Resume { session: session.id.clone(), cwd: session.cwd.clone() }
                }
                SessionState::Closed => Activation::Nothing,
            },
            RowKind::TakeOver { session, cwd } => Activation::TakeOver { session: session.clone(), cwd: cwd.clone() },
            RowKind::Group { .. } | RowKind::Detail { .. } => Activation::Nothing,
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

/// The first session or shell row: what `Enter` right after typing a filter should open.
pub fn first_entry(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| matches!(row.kind, RowKind::Session { .. } | RowKind::Shell { .. }))
}

/// The Projects view.
pub fn projects(input: &TreeInput<'_>) -> Vec<Row> {
    let shown = Shown::new(input);
    let mut emitter = Emitter { input, labels: &shown.labels, rows: Vec::new() };
    for project in &shown.projects {
        let mut counts = ToneCounts::default();
        for session in &project.live {
            counts.add(tone(session, input.unseen.contains(&session.id)));
        }
        let expanded = shown.filtering || !input.toggled.contains(&project.path);
        let label = shown.labels[&project.path].clone();
        emitter.rows.push(Row { kind: RowKind::Project { path: project.path.clone(), label, expanded, counts } });
        if expanded {
            for session in project.live.iter().chain(&project.closed) {
                emitter.session(session, false);
            }
        }
    }
    emitter.shells(&shown.shells, false);
    emitter.rows
}

/// The Status view.
pub fn status(input: &TreeInput<'_>) -> Vec<Row> {
    let shown = Shown::new(input);
    let mut live: Vec<&SessionInfo> = shown.projects.iter().flat_map(|project| project.live.iter().copied()).collect();
    let mut closed: Vec<&SessionInfo> = shown.projects.iter().flat_map(|project| project.closed.iter().copied()).collect();
    live.sort_by(newest_first);
    closed.sort_by(newest_first);
    let mut emitter = Emitter { input, labels: &shown.labels, rows: Vec::new() };
    let sections = [
        ("Needs input", Tone::NeedsInput),
        ("Working", Tone::Working),
        ("Done", Tone::Done),
        ("Idle", Tone::Idle),
        ("Running elsewhere", Tone::Elsewhere),
    ];
    for (label, wanted) in sections {
        let group: Vec<&SessionInfo> =
            live.iter().copied().filter(|s| tone(s, input.unseen.contains(&s.id)) == wanted).collect();
        emitter.group(label, &group);
    }
    emitter.shells(&shown.shells, true);
    emitter.group(if shown.filtering { "Closed" } else { "Recent" }, &closed);
    emitter.rows
}

/// A project and its shown sessions.
struct Project<'a> {
    path: PathBuf,
    /// Live sessions, newest first.
    live: Vec<&'a SessionInfo>,
    /// Closed sessions, newest first (recent ones only, unless filtering).
    closed: Vec<&'a SessionInfo>,
}

/// The entries both views show, by project.
struct Shown<'a> {
    filtering: bool,
    /// Projects with live sessions first, then by latest activity.
    projects: Vec<Project<'a>>,
    shells: Vec<&'a ShellEntry>,
    /// Displayed project names, by project directory (shells' projects too).
    labels: HashMap<PathBuf, String>,
}

impl<'a> Shown<'a> {
    fn new(input: &TreeInput<'a>) -> Shown<'a> {
        let mut fuzzy = Fuzzy::new(input.filter);
        let filtering = fuzzy.is_some();
        let cutoff = input.now_ms.saturating_sub(RECENT_DAYS * DAY_MS);
        let mut by_path: HashMap<PathBuf, Project<'a>> = HashMap::new();
        for session in input.sessions.iter().copied() {
            let recent = session.cwd_exists && session.last_active_ms >= cutoff;
            let wanted = if filtering { true } else { session.state != SessionState::Closed || recent };
            if !wanted || !keep(&mut fuzzy, session_title(session), &session.cwd, input.home) {
                continue;
            }
            let path = (input.project_of)(&session.cwd);
            let project = by_path.entry(path.clone()).or_insert_with(|| Project { path, live: Vec::new(), closed: Vec::new() });
            if session.state == SessionState::Closed { project.closed.push(session) } else { project.live.push(session) }
        }
        let mut projects: Vec<Project<'a>> = by_path.into_values().collect();
        for project in &mut projects {
            project.live.sort_by(newest_first);
            project.closed.sort_by(newest_first);
            if !filtering {
                project.closed.truncate(RECENT_PER_PROJECT);
            }
        }
        let latest = |project: &Project<'_>| {
            let entries = if project.live.is_empty() { &project.closed } else { &project.live };
            entries.iter().map(|s| s.last_active_ms).max().unwrap_or(0)
        };
        projects.sort_by(|a, b| {
            (!b.live.is_empty(), latest(b)).cmp(&(!a.live.is_empty(), latest(a))).then_with(|| a.path.cmp(&b.path))
        });
        let shells: Vec<&ShellEntry> = input.shells.iter().filter(|s| keep(&mut fuzzy, &s.title, &s.cwd, input.home)).collect();
        let mut paths: Vec<PathBuf> = projects.iter().map(|project| project.path.clone()).collect();
        paths.extend(shells.iter().map(|shell| (input.project_of)(&shell.cwd)));
        let labels = labels(&paths, input.home);
        Shown { filtering, projects, shells, labels }
    }
}

/// Project names: the folder's name, or its whole displayed path where two projects share a name.
fn labels(paths: &[PathBuf], home: Option<&Path>) -> HashMap<PathBuf, String> {
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

fn newest_first(a: &&SessionInfo, b: &&SessionInfo) -> std::cmp::Ordering {
    b.last_active_ms.cmp(&a.last_active_ms).then_with(|| a.id.cmp(&b.id))
}

/// Whether an entry passes the filter (matched against its title and displayed directory).
fn keep(fuzzy: &mut Option<Fuzzy>, title: &str, cwd: &Path, home: Option<&Path>) -> bool {
    fuzzy.as_mut().is_none_or(|fuzzy| fuzzy.score(&format!("{title} {}", display_path(cwd, home))).is_some())
}

fn detail_text(session: &SessionInfo) -> Option<String> {
    match session.state {
        SessionState::External { pid: Some(pid) } => Some(format!("Running outside Gantry (pid {pid})")),
        SessionState::External { pid: None } => Some("Running in a container".into()),
        _ => None,
    }
}

struct Emitter<'i, 'a> {
    input: &'i TreeInput<'a>,
    labels: &'i HashMap<PathBuf, String>,
    rows: Vec<Row>,
}

impl Emitter<'_, '_> {
    /// A section heading and its sessions (nothing when empty).
    fn group(&mut self, label: &'static str, sessions: &[&SessionInfo]) {
        if sessions.is_empty() {
            return;
        }
        self.rows.push(Row { kind: RowKind::Group { label, count: sessions.len() } });
        for session in sessions {
            self.session(session, true);
        }
    }

    fn shells(&mut self, shells: &[&ShellEntry], with_project: bool) {
        if shells.is_empty() {
            return;
        }
        self.rows.push(Row { kind: RowKind::Group { label: "Shells", count: shells.len() } });
        for shell in shells {
            let project = with_project.then(|| self.label(&shell.cwd));
            self.rows.push(Row { kind: RowKind::Shell { entry: (*shell).clone(), project } });
        }
    }

    fn label(&self, cwd: &Path) -> String {
        let path = (self.input.project_of)(cwd);
        self.labels.get(&path).cloned().unwrap_or_else(|| display_path(&path, self.input.home))
    }

    fn session(&mut self, session: &SessionInfo, with_project: bool) {
        let project = with_project.then(|| self.label(&session.cwd));
        let age = age(self.input.now_ms, session.last_active_ms);
        let tone = tone(session, self.input.unseen.contains(&session.id));
        self.rows.push(Row { kind: RowKind::Session { session: session.clone(), tone, age, project } });
        if self.input.details == Some(session.id.as_str())
            && let Some(text) = detail_text(session)
        {
            self.rows.push(Row { kind: RowKind::Detail { session: session.id.clone(), text } });
            if matches!(session.state, SessionState::External { pid: Some(_) }) && session.cwd_exists {
                self.rows.push(Row { kind: RowKind::TakeOver { session: session.id.clone(), cwd: session.cwd.clone() } });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::SessionStatus;

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

    #[derive(Default)]
    struct Fixture {
        sessions: Vec<SessionInfo>,
        shells: Vec<ShellEntry>,
        filter: String,
        toggled: HashSet<PathBuf>,
        details: Option<String>,
        unseen: HashSet<String>,
    }

    impl Fixture {
        fn of(sessions: Vec<SessionInfo>) -> Fixture {
            Fixture { sessions, ..Fixture::default() }
        }

        fn rows(&self, view: fn(&TreeInput<'_>) -> Vec<Row>) -> Vec<Row> {
            let refs: Vec<&SessionInfo> = self.sessions.iter().collect();
            view(&TreeInput {
                sessions: &refs,
                shells: &self.shells,
                home: Some(Path::new("/home/me")),
                filter: &self.filter,
                toggled: &self.toggled,
                details: self.details.as_deref(),
                unseen: &self.unseen,
                project_of: &project_of,
                now_ms: NOW,
            })
        }

        fn outline(&self, view: fn(&TreeInput<'_>) -> Vec<Row>) -> Vec<String> {
            outline(&self.rows(view))
        }
    }

    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match &row.kind {
                RowKind::Group { label, count } => format!("{label} ({count})"),
                RowKind::Project { label, expanded, .. } => format!("{} {label}", if *expanded { "▾" } else { "▸" }),
                RowKind::Session { session, project: None, .. } => format!("  {}", session.id),
                RowKind::Session { session, project: Some(project), .. } => format!("  {} @{project}", session.id),
                RowKind::Shell { entry, .. } => format!("  shell {}", entry.leaf),
                RowKind::Detail { text, .. } => format!("    ({text})"),
                RowKind::TakeOver { .. } => "    Take over…".into(),
            })
            .collect()
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
    fn projects_list_live_ones_first_then_recent_ones_with_shells_last() {
        let mut fixture = Fixture::of(vec![
            closed("old-but-recent", "/r/quiet", NOW - HOUR_MS),
            managed("m", "/r/app/src", NOW - 2 * HOUR_MS),
            external("e", "/r/app", NOW - 3 * HOUR_MS),
            closed("c", "/r/app", NOW - 10),
            managed("t", "/tmp/scratch", NOW - 5 * HOUR_MS),
        ]);
        fixture.shells.push(ShellEntry { leaf: 3, cwd: "/r/app".into(), title: "zsh".into() });
        assert_eq!(fixture.outline(projects), vec![
            "▾ app", "  m", "  e", "  c", "▾ scratch", "  t", "▾ quiet", "  old-but-recent", "Shells (1)", "  shell 3",
        ]);
    }

    #[test]
    fn recent_means_the_last_three_days_at_most_eight_per_project_with_the_folder_still_there() {
        let mut sessions: Vec<SessionInfo> = (1..=10).map(|n| closed(&format!("c{n}"), "/r/app", NOW - n * HOUR_MS)).collect();
        sessions.push(closed("stale", "/r/app", NOW - RECENT_DAYS * DAY_MS - 1));
        sessions.push(SessionInfo { cwd_exists: false, ..closed("gone", "/r/app", NOW - 1) });
        sessions.push(closed("ancient", "/r/old", NOW - 30 * DAY_MS));
        let fixture = Fixture::of(sessions);
        assert_eq!(fixture.outline(projects), vec!["▾ app", "  c1", "  c2", "  c3", "  c4", "  c5", "  c6", "  c7", "  c8"]);
        assert_eq!(fixture.outline(status)[0], "Recent (8)");
        assert_eq!(fixture.outline(status).len(), 9);
    }

    #[test]
    fn filtering_reaches_every_session_without_caps() {
        let mut sessions: Vec<SessionInfo> = (1..=10).map(|n| closed(&format!("c{n}"), "/r/app", NOW - n * HOUR_MS)).collect();
        sessions.push(SessionInfo { cwd_exists: false, ..closed("gone", "/r/app", NOW - 40 * DAY_MS) });
        sessions.push(closed("other", "/r/else", NOW - 1));
        let mut fixture = Fixture::of(sessions);
        fixture.toggled.insert("/r/app".into());
        fixture.filter = "app".into();
        let rows = fixture.outline(projects);
        assert_eq!((rows[0].as_str(), rows.len()), ("▾ app", 12));
        assert!(rows.contains(&"  gone".to_string()));
        let rows = fixture.outline(status);
        assert_eq!((rows[0].as_str(), rows.len()), ("Closed (11)", 12));
    }

    #[test]
    fn projects_with_the_same_name_show_their_paths() {
        let fixture = Fixture::of(vec![
            managed("a", "/r/api", NOW - 1),
            managed("b", "/home/me/work/api", NOW - 2),
            managed("h", "/home/me", NOW - 3),
        ]);
        assert_eq!(fixture.outline(projects), vec!["▾ /r/api", "  a", "▾ ~/work/api", "  b", "▾ ~", "  h"]);
        assert_eq!(fixture.outline(status)[1..], ["  a @/r/api".to_string(), "  b @~/work/api".to_string(), "  h @~".to_string()]);
    }

    #[test]
    fn project_headers_count_live_sessions_by_tone() {
        let mut fixture = Fixture::of(vec![
            with(managed("n", "/r/p", 6), SessionStatus::NeedsInput { reason: None }),
            with(managed("w", "/r/p/sub", 5), SessionStatus::Busy),
            with(managed("d", "/r/p", 4), SessionStatus::Idle),
            managed("i", "/r/p", 3),
            external("e", "/r/p", 2),
            closed("c", "/r/p", NOW - 1),
        ]);
        fixture.unseen.insert("d".into());
        let rows = fixture.rows(projects);
        let RowKind::Project { counts, .. } = &rows[0].kind else { panic!("{rows:?}") };
        assert_eq!(*counts, ToneCounts { needs_input: 1, working: 1, done: 1, idle: 1, elsewhere: 1 });
        let RowKind::Session { tone, .. } = &rows[3].kind else { panic!("{rows:?}") };
        assert_eq!(*tone, Tone::Done);
    }

    #[test]
    fn a_collapsed_project_hides_its_sessions_until_filtering() {
        let mut fixture = Fixture::of(vec![managed("m", "/r/p", 2), managed("q", "/r/q", 1)]);
        fixture.toggled.insert("/r/p".into());
        assert_eq!(fixture.outline(projects), vec!["▸ p", "▾ q", "  q"]);
        fixture.filter = "title m".into();
        assert_eq!(fixture.outline(projects), vec!["▾ p", "  m"]);
    }

    #[test]
    fn status_view_sections_follow_urgency_and_hide_when_empty() {
        let mut fixture = Fixture::of(vec![
            with(managed("waits", "/r/a", NOW - 10), SessionStatus::NeedsInput { reason: None }),
            with(managed("works", "/r/a", NOW - 20), SessionStatus::Busy),
            with(managed("finished", "/r/a", NOW - 30), SessionStatus::Idle),
            with(managed("rests", "/r/a", NOW - 40), SessionStatus::Idle),
            managed("starts", "/r/b", NOW - 50),
            with(external("e", "/r/b", NOW - 60), SessionStatus::Busy),
            closed("c", "/r/c", NOW - 2 * DAY_MS),
            closed("old", "/r/c", NOW - 40 * DAY_MS),
        ]);
        fixture.unseen.insert("finished".into());
        fixture.shells.push(ShellEntry { leaf: 4, cwd: "/home/me".into(), title: "zsh".into() });
        assert_eq!(fixture.outline(status), vec![
            "Needs input (1)", "  waits @a", "Working (1)", "  works @a", "Done (1)", "  finished @a",
            "Idle (2)", "  rests @a", "  starts @b", "Running elsewhere (1)", "  e @b", "Shells (1)", "  shell 4",
            "Recent (1)", "  c @c",
        ]);
        let rows = fixture.rows(status);
        let RowKind::Shell { project, .. } = &rows[12].kind else { panic!("{rows:?}") };
        assert_eq!(project.as_deref(), Some("~"));
        fixture.sessions.retain(|s| s.id == "c");
        fixture.shells.clear();
        assert_eq!(fixture.outline(status), vec!["Recent (1)", "  c @c"]);
    }

    #[test]
    fn take_over_is_offered_for_outside_host_sessions_whose_directory_exists() {
        let mut fixture = Fixture::of(vec![
            external("e", "/p", 3),
            SessionInfo { cwd_exists: false, ..external("gone", "/p", 2) },
        ]);
        fixture.details = Some("gone".into());
        assert!(!fixture.outline(projects).contains(&"    Take over…".to_string()));
        fixture.details = Some("e".into());
        let rows = fixture.rows(projects);
        let index = find(&rows, &RowKey::TakeOver("e".into())).unwrap();
        assert!(rows[index].selectable());
        assert_eq!(rows[index].activation(), Activation::TakeOver { session: "e".into(), cwd: "/p".into() });
    }

    #[test]
    fn details_appear_under_their_external_session() {
        let mut fixture = Fixture::of(vec![
            external("e", "/p", 2),
            session("box", "/p", SessionState::External { pid: None }, 1),
        ]);
        fixture.details = Some("e".into());
        assert_eq!(fixture.outline(projects), vec![
            "▾ p", "  e", "    (Running outside Gantry (pid 42))", "    Take over…", "  box",
        ]);
        fixture.details = Some("box".into());
        assert_eq!(fixture.outline(status)[2..], ["  box @p".to_string(), "    (Running in a container)".to_string()]);
    }

    #[test]
    fn activation_follows_the_session_state() {
        let row = |session: SessionInfo| {
            let tone = tone(&session, false);
            Row { kind: RowKind::Session { session, tone, age: String::new(), project: None } }
        };
        assert_eq!(row(managed("m", "/a", 1)).activation(), Activation::FocusPane(1));
        let container = session("e", "/a", SessionState::External { pid: None }, 1);
        assert_eq!(row(container).activation(), Activation::ToggleDetails("e".into()));
        assert_eq!(row(closed("c", "/a", 1)).activation(), Activation::Resume { session: "c".into(), cwd: "/a".into() });
        let missing = SessionInfo { cwd_exists: false, ..closed("gone", "/deleted", 1) };
        assert_eq!(row(missing).activation(), Activation::Nothing);
        let shell = Row { kind: RowKind::Shell { entry: ShellEntry { leaf: 9, cwd: "/".into(), title: "sh".into() }, project: None } };
        assert_eq!(shell.activation(), Activation::FocusLeaf(9));
        let project = Row { kind: RowKind::Project { path: "/a".into(), label: "a".into(), expanded: true, counts: ToneCounts::default() } };
        assert_eq!((project.activation(), project.selectable()), (Activation::ToggleProject("/a".into()), true));
        let group = Row { kind: RowKind::Group { label: "Recent", count: 1 } };
        assert_eq!((group.activation(), group.selectable()), (Activation::Nothing, false));
    }

    #[test]
    fn rows_are_found_by_identity_and_typing_selects_the_first_entry() {
        let mut fixture = Fixture::of(vec![closed("new", "/r/p", NOW - 1), closed("old", "/r/q", NOW - 2)]);
        fixture.filter = "title".into();
        let rows = fixture.rows(projects);
        // Project headings come first; the first entry is a session.
        let first = first_entry(&rows).unwrap();
        assert!(matches!(&rows[first].kind, RowKind::Session { session, .. } if session.id == "new"));
        let key = RowKey::Session("old".into());
        let index = find(&rows, &key).unwrap();
        assert_eq!(rows[index].key(), key);
        // A row that disappeared (or moved) is looked up again, never taken by its old index.
        assert_eq!(find(&rows[..index], &key), None);
        assert_eq!(first_entry(&rows[..1]), None);
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
