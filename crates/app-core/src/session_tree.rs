//! The sidebar's session tree as plain rows: the Folders view (directories with path compression) and
//! the Status view (sessions grouped by state), both narrowed by a fuzzy filter.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use protocol::message::{PaneId, SessionInfo, SessionState};

use crate::fuzzy::Fuzzy;
use crate::split_tree::NodeId;

/// Closed sessions shown per directory before "… N more".
pub const CLOSED_SHOWN: usize = 5;
/// The Status view lists closed sessions active within this many days.
pub const CLOSED_DAYS: u64 = 30;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// A shell pane, for the tree (shells are not in the daemon's session list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEntry {
    /// Its leaf.
    pub leaf: NodeId,
    /// Directory it was started in.
    pub cwd: PathBuf,
    /// Pane title.
    pub title: String,
}

/// Everything the tree is built from.
pub struct TreeInput<'a> {
    /// Claude sessions.
    pub sessions: &'a [&'a SessionInfo],
    /// Shell panes.
    pub shells: &'a [ShellEntry],
    /// Home directory, shown as `~`.
    pub home: Option<&'a Path>,
    /// Fuzzy filter text (blank = everything).
    pub filter: &'a str,
    /// Directories whose expansion the user flipped from the default.
    pub toggled: &'a HashSet<PathBuf>,
    /// Directories showing all their closed sessions.
    pub show_all: &'a HashSet<PathBuf>,
    /// External session whose details are open.
    pub details: Option<&'a str>,
    /// Current time, ms since the Unix epoch.
    pub now_ms: u64,
}

/// One sidebar row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Indentation level.
    pub depth: usize,
    /// What it shows.
    pub kind: RowKind,
}

/// What a row shows.
#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// Host root ("Local" until WSL/SSH hosts exist).
    Host {
        /// Host name.
        name: String,
    },
    /// Status-view group heading.
    Group {
        /// Heading.
        label: &'static str,
        /// Entries in the group.
        count: usize,
    },
    /// A directory (possibly several compressed path segments).
    Dir {
        /// Full path.
        path: PathBuf,
        /// Displayed path.
        label: String,
        /// Children shown.
        expanded: bool,
        /// Live sessions and shells in its subtree.
        live: usize,
        /// Closed sessions in its subtree.
        closed: usize,
    },
    /// A Claude session.
    Session {
        /// The session.
        session: SessionInfo,
        /// Time since last activity.
        age: String,
        /// Its directory (Status view only).
        path: Option<String>,
    },
    /// A shell pane.
    Shell {
        /// The shell.
        entry: ShellEntry,
        /// Its directory (Status view only).
        path: Option<String>,
    },
    /// "… N more" closed sessions of `dir`.
    More {
        /// Directory.
        dir: PathBuf,
        /// Sessions not shown.
        hidden: usize,
    },
    /// Details of an external session.
    Detail {
        /// Session id.
        session: String,
        /// Text.
        text: String,
    },
}

/// Identifies a row across rebuilds (for keeping the selection).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// Host row.
    Host(String),
    /// Group heading.
    Group(&'static str),
    /// Directory.
    Dir(PathBuf),
    /// Session.
    Session(String),
    /// Shell leaf.
    Shell(NodeId),
    /// "… N more" of a directory.
    More(PathBuf),
    /// Details of a session.
    Detail(String),
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
    /// Expand or collapse a directory.
    ToggleDir(PathBuf),
    /// Show all closed sessions of a directory.
    ShowAll(PathBuf),
    /// Nothing (headings; a closed session whose directory is gone).
    Nothing,
}

impl Row {
    /// Identity of this row.
    pub fn key(&self) -> RowKey {
        match &self.kind {
            RowKind::Host { name } => RowKey::Host(name.clone()),
            RowKind::Group { label, .. } => RowKey::Group(label),
            RowKind::Dir { path, .. } => RowKey::Dir(path.clone()),
            RowKind::Session { session, .. } => RowKey::Session(session.id.clone()),
            RowKind::Shell { entry, .. } => RowKey::Shell(entry.leaf),
            RowKind::More { dir, .. } => RowKey::More(dir.clone()),
            RowKind::Detail { session, .. } => RowKey::Detail(session.clone()),
        }
    }

    /// Whether keyboard selection can land here (headings and details are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Host { .. } | RowKind::Group { .. } | RowKind::Detail { .. })
    }

    /// What activating this row does.
    pub fn activation(&self) -> Activation {
        match &self.kind {
            RowKind::Dir { path, .. } => Activation::ToggleDir(path.clone()),
            RowKind::More { dir, .. } => Activation::ShowAll(dir.clone()),
            RowKind::Shell { entry, .. } => Activation::FocusLeaf(entry.leaf),
            RowKind::Session { session, .. } => match session.state {
                SessionState::Managed { pane } => Activation::FocusPane(pane),
                SessionState::External { .. } => Activation::ToggleDetails(session.id.clone()),
                SessionState::Closed if session.cwd_exists => {
                    Activation::Resume { session: session.id.clone(), cwd: session.cwd.clone() }
                }
                SessionState::Closed => Activation::Nothing,
            },
            RowKind::Host { .. } | RowKind::Group { .. } | RowKind::Detail { .. } => Activation::Nothing,
        }
    }
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

/// The Folders view.
pub fn folders(input: &TreeInput<'_>) -> Vec<Row> {
    let mut fuzzy = Fuzzy::new(input.filter);
    let filtering = fuzzy.is_some();
    let mut root = DirNode::default();
    for session in input.sessions {
        if keep(&mut fuzzy, session_title(session), &session.cwd, input.home) {
            root.insert(&session.cwd).sessions.push(session);
        }
    }
    for shell in input.shells {
        if keep(&mut fuzzy, &shell.title, &shell.cwd, input.home) {
            root.insert(&shell.cwd).shells.push(shell);
        }
    }
    let mut starts = Vec::new();
    top_level(&root, &mut starts);
    let mut emitter = Emitter { input, filtering, rows: vec![Row { depth: 0, kind: RowKind::Host { name: "Local".into() } }] };
    for (node, with_children) in starts {
        emitter.dir(node.chain_end(), None, 1, with_children);
    }
    emitter.rows
}

/// The Status view.
pub fn status(input: &TreeInput<'_>) -> Vec<Row> {
    let mut fuzzy = Fuzzy::new(input.filter);
    let mut sessions: Vec<&SessionInfo> =
        input.sessions.iter().copied().filter(|s| keep(&mut fuzzy, session_title(s), &s.cwd, input.home)).collect();
    sessions.sort_by(newest_first);
    let shells: Vec<&ShellEntry> = input.shells.iter().filter(|s| keep(&mut fuzzy, &s.title, &s.cwd, input.home)).collect();
    let cutoff = input.now_ms.saturating_sub(CLOSED_DAYS * DAY_MS);
    let in_state = |pick: fn(&SessionInfo) -> bool| sessions.iter().copied().filter(|s| pick(s)).collect::<Vec<_>>();
    let in_ccm = in_state(|s| matches!(s.state, SessionState::Managed { .. }));
    let external = in_state(|s| matches!(s.state, SessionState::External { .. }));
    let closed: Vec<&SessionInfo> =
        in_state(|s| s.state == SessionState::Closed).into_iter().filter(|s| s.last_active_ms >= cutoff).collect();
    let mut emitter = Emitter { input, filtering: true, rows: Vec::new() };
    if !in_ccm.is_empty() || !shells.is_empty() {
        emitter.rows.push(Row { depth: 0, kind: RowKind::Group { label: "In ccm", count: in_ccm.len() + shells.len() } });
        for session in in_ccm {
            emitter.session(session, 1, true);
        }
        for shell in shells {
            let path = Some(display_path(&shell.cwd, input.home));
            emitter.rows.push(Row { depth: 1, kind: RowKind::Shell { entry: shell.clone(), path } });
        }
    }
    for (label, group) in [("External", external), ("Closed (last 30 days)", closed)] {
        if group.is_empty() {
            continue;
        }
        emitter.rows.push(Row { depth: 0, kind: RowKind::Group { label, count: group.len() } });
        for session in group {
            emitter.session(session, 1, true);
        }
    }
    emitter.rows
}

fn is_live(session: &SessionInfo) -> bool {
    session.state != SessionState::Closed
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
        SessionState::External { pid: Some(pid) } => Some(format!("Running outside ccm (pid {pid})")),
        SessionState::External { pid: None } => Some("Running in a container".into()),
        _ => None,
    }
}

/// One directory of the path trie.
#[derive(Default)]
struct DirNode<'a> {
    path: PathBuf,
    /// A filesystem root or drive prefix, not a directory row of its own.
    anchor: bool,
    children: BTreeMap<OsString, DirNode<'a>>,
    sessions: Vec<&'a SessionInfo>,
    shells: Vec<&'a ShellEntry>,
}

impl<'a> DirNode<'a> {
    fn insert(&mut self, path: &Path) -> &mut DirNode<'a> {
        let mut node = self;
        for component in path.components().filter(|c| *c != Component::CurDir) {
            let child_path = node.path.join(component.as_os_str());
            let anchor = !matches!(component, Component::Normal(_));
            node = node
                .children
                .entry(component.as_os_str().to_owned())
                .or_insert_with(|| DirNode { path: child_path, anchor, ..DirNode::default() });
        }
        node
    }

    fn has_entries(&self) -> bool {
        !self.sessions.is_empty() || !self.shells.is_empty()
    }

    /// (live, closed) entries of this directory itself.
    fn own_counts(&self) -> (usize, usize) {
        let live = self.sessions.iter().filter(|s| is_live(s)).count() + self.shells.len();
        (live, self.sessions.iter().filter(|s| !is_live(s)).count())
    }

    /// (live, closed) entries of this subtree.
    fn counts(&self) -> (usize, usize) {
        self.children.values().map(DirNode::counts).fold(self.own_counts(), |(l, c), (cl, cc)| (l + cl, c + cc))
    }

    /// Follow directories that have no entries and a single child (path compression).
    fn chain_end(&self) -> &DirNode<'a> {
        let mut node = self;
        while !node.has_entries() && node.children.len() == 1 {
            node = node.children.values().next().expect("one child");
        }
        node
    }
}

/// Where top-level rows start: the children of filesystem roots. A root with entries of its own gets
/// a row (`false` = without children) for just those.
fn top_level<'n, 'a>(node: &'n DirNode<'a>, out: &mut Vec<(&'n DirNode<'a>, bool)>) {
    for child in node.children.values() {
        if child.anchor {
            if child.has_entries() {
                out.push((child, false));
            }
            top_level(child, out);
        } else {
            out.push((child, true));
        }
    }
}

struct Emitter<'i, 'a> {
    input: &'i TreeInput<'a>,
    filtering: bool,
    rows: Vec<Row>,
}

impl Emitter<'_, '_> {
    fn dir(&mut self, node: &DirNode<'_>, parent: Option<&Path>, depth: usize, with_children: bool) {
        let label = match parent.and_then(|parent| node.path.strip_prefix(parent).ok()) {
            Some(relative) => relative.display().to_string(),
            None => display_path(&node.path, self.input.home),
        };
        let (live, closed) = if with_children { node.counts() } else { node.own_counts() };
        let default_open = live > 0 || !node.has_entries();
        let expanded = self.filtering || default_open != self.input.toggled.contains(&node.path);
        self.rows.push(Row { depth, kind: RowKind::Dir { path: node.path.clone(), label, expanded, live, closed } });
        if !expanded {
            return;
        }
        let mut live_sessions: Vec<&SessionInfo> = node.sessions.iter().copied().filter(|s| is_live(s)).collect();
        let mut closed_sessions: Vec<&SessionInfo> = node.sessions.iter().copied().filter(|s| !is_live(s)).collect();
        live_sessions.sort_by(newest_first);
        closed_sessions.sort_by(newest_first);
        for session in live_sessions {
            self.session(session, depth + 1, false);
        }
        for shell in &node.shells {
            self.rows.push(Row { depth: depth + 1, kind: RowKind::Shell { entry: (*shell).clone(), path: None } });
        }
        let all = self.filtering || self.input.show_all.contains(&node.path);
        let shown = if all { closed_sessions.len() } else { closed_sessions.len().min(CLOSED_SHOWN) };
        for session in &closed_sessions[..shown] {
            self.session(session, depth + 1, false);
        }
        if shown < closed_sessions.len() {
            self.rows.push(Row { depth: depth + 1, kind: RowKind::More { dir: node.path.clone(), hidden: closed_sessions.len() - shown } });
        }
        if with_children {
            for child in node.children.values() {
                self.dir(child.chain_end(), Some(&node.path), depth + 1, true);
            }
        }
    }

    fn session(&mut self, session: &SessionInfo, depth: usize, with_path: bool) {
        let path = with_path.then(|| display_path(&session.cwd, self.input.home));
        let age = age(self.input.now_ms, session.last_active_ms);
        self.rows.push(Row { depth, kind: RowKind::Session { session: session.clone(), age, path } });
        if self.input.details == Some(session.id.as_str())
            && let Some(text) = detail_text(session)
        {
            self.rows.push(Row { depth: depth + 1, kind: RowKind::Detail { session: session.id.clone(), text } });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 100 * DAY_MS;

    fn session(id: &str, cwd: &str, state: SessionState, last_active_ms: u64) -> SessionInfo {
        SessionInfo { id: id.into(), cwd: cwd.into(), title: format!("title {id}"), last_active_ms, state, cwd_exists: true }
    }

    fn managed(id: &str, cwd: &str, last: u64) -> SessionInfo {
        session(id, cwd, SessionState::Managed { pane: 1 }, last)
    }

    fn closed(id: &str, cwd: &str, last: u64) -> SessionInfo {
        session(id, cwd, SessionState::Closed, last)
    }

    #[derive(Default)]
    struct Fixture {
        sessions: Vec<SessionInfo>,
        shells: Vec<ShellEntry>,
        filter: String,
        toggled: HashSet<PathBuf>,
        show_all: HashSet<PathBuf>,
        details: Option<String>,
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
                show_all: &self.show_all,
                details: self.details.as_deref(),
                now_ms: NOW,
            })
        }

        fn outline(&self, view: fn(&TreeInput<'_>) -> Vec<Row>) -> Vec<String> {
            outline(&self.rows(view))
        }
    }

    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let text = match &row.kind {
                    RowKind::Host { name } => name.clone(),
                    RowKind::Group { label, count } => format!("{label} ({count})"),
                    RowKind::Dir { label, expanded, live, closed, .. } => {
                        format!("{} {label} [{live}/{closed}]", if *expanded { "▾" } else { "▸" })
                    }
                    RowKind::Session { session, .. } => session.id.clone(),
                    RowKind::Shell { entry, .. } => format!("shell {}", entry.leaf),
                    RowKind::More { hidden, .. } => format!("… {hidden} more"),
                    RowKind::Detail { text, .. } => format!("({text})"),
                };
                format!("{}{text}", "  ".repeat(row.depth))
            })
            .collect()
    }

    #[test]
    fn consecutive_segments_merge_until_a_branch_or_own_sessions() {
        let fixture = Fixture::of(vec![
            managed("a", "/home/me/code/repositories/devops", 5),
            closed("b", "/home/me/code/repositories/agentic-plugins", 4),
        ]);
        assert_eq!(fixture.outline(folders), vec![
            "Local",
            "  ▾ ~/code/repositories [1/1]",
            "    ▸ agentic-plugins [0/1]",
            "    ▾ devops [1/0]",
            "      a",
        ]);
    }

    #[test]
    fn home_itself_shows_as_tilde() {
        let fixture = Fixture::of(vec![closed("s", "/home/me", 4), managed("t", "/home/me/x", 5)]);
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ ~ [1/1]", "    s", "    ▾ x [1/0]", "      t"]);
    }

    #[test]
    fn compression_splits_on_components_not_string_prefixes() {
        let fixture = Fixture::of(vec![managed("m1", "/a/b", 5), managed("m2", "/a/bc", 4)]);
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ /a [2/0]", "    ▾ b [1/0]", "      m1", "    ▾ bc [1/0]", "      m2"]);
    }

    #[test]
    fn directories_outside_home_start_at_their_full_path() {
        let fixture = Fixture::of(vec![managed("h", "/home/me/r", 5), managed("t", "/tmp", 4), closed("root", "/", 3)]);
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▸ / [0/1]", "  ▾ ~/r [1/0]", "    h", "  ▾ /tmp [1/0]", "    t"]);
    }

    #[test]
    fn closed_sessions_are_newest_first_and_capped_with_more() {
        let mut fixture = Fixture::of((1..=7).map(|n| closed(&format!("c{n}"), "/w", n)).collect());
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▸ /w [0/7]"]);
        fixture.toggled.insert("/w".into());
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ /w [0/7]", "    c7", "    c6", "    c5", "    c4", "    c3", "    … 2 more"]);
        fixture.show_all.insert("/w".into());
        assert_eq!(fixture.outline(folders).len(), 2 + 7);
    }

    #[test]
    fn live_entries_come_before_closed_and_directories_count_them() {
        let mut fixture = Fixture::of(vec![
            managed("m", "/p", 5),
            session("e", "/p", SessionState::External { pid: Some(42) }, 9),
            closed("c", "/p", 20),
            closed("q1", "/q", 1),
        ]);
        fixture.shells.push(ShellEntry { leaf: 3, cwd: "/p".into(), title: "zsh".into() });
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ /p [3/1]", "    e", "    m", "    shell 3", "    c", "  ▸ /q [0/1]"]);
        fixture.toggled.extend([PathBuf::from("/p"), PathBuf::from("/q")]);
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▸ /p [3/1]", "  ▾ /q [0/1]", "    q1"]);
    }

    #[test]
    fn filter_keeps_matching_sessions_with_their_directories_expanded() {
        let mut fixture = Fixture::of(vec![closed("j", "/home/me/devops/jenkins-lib", 5), closed("k", "/home/me/devops/k8s", 4)]);
        fixture.sessions[0].title = "Fix Jenkins lib".into();
        fixture.sessions[1].title = "Bump chart".into();
        fixture.filter = "jenkins".into();
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ ~/devops/jenkins-lib [0/1]", "    j"]);
        fixture.filter = "k8s".into();
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ ~/devops/k8s [0/1]", "    k"]);
    }

    #[test]
    fn status_view_groups_live_external_and_recent_closed() {
        let mut fixture = Fixture::of(vec![
            managed("m", "/a", NOW - 50),
            session("e", "/b", SessionState::External { pid: Some(42) }, NOW - 60),
            closed("c", "/home/me/c", NOW - 2 * DAY_MS),
            closed("old", "/c", NOW - 40 * DAY_MS),
        ]);
        fixture.shells.push(ShellEntry { leaf: 4, cwd: "/a".into(), title: "zsh".into() });
        let rows = fixture.rows(status);
        assert_eq!(outline(&rows), vec!["In ccm (2)", "  m", "  shell 4", "External (1)", "  e", "Closed (last 30 days) (1)", "  c"]);
        let RowKind::Session { path, age, .. } = &rows[6].kind else { panic!("{rows:?}") };
        assert_eq!((path.as_deref(), age.as_str()), (Some("~/c"), "2d"));
    }

    #[test]
    fn activation_follows_the_session_state() {
        let row = |session: SessionInfo| Row { depth: 1, kind: RowKind::Session { session, age: String::new(), path: None } };
        assert_eq!(row(managed("m", "/a", 1)).activation(), Activation::FocusPane(1));
        let external = session("e", "/a", SessionState::External { pid: None }, 1);
        assert_eq!(row(external).activation(), Activation::ToggleDetails("e".into()));
        assert_eq!(row(closed("c", "/a", 1)).activation(), Activation::Resume { session: "c".into(), cwd: "/a".into() });
        let missing = SessionInfo { cwd_exists: false, ..closed("gone", "/deleted", 1) };
        assert_eq!(row(missing).activation(), Activation::Nothing);
        let shell = Row { depth: 1, kind: RowKind::Shell { entry: ShellEntry { leaf: 9, cwd: "/".into(), title: "sh".into() }, path: None } };
        assert_eq!(shell.activation(), Activation::FocusLeaf(9));
        let dir = Row { depth: 1, kind: RowKind::Dir { path: "/a".into(), label: "/a".into(), expanded: true, live: 0, closed: 1 } };
        assert_eq!((dir.activation(), dir.selectable()), (Activation::ToggleDir("/a".into()), true));
        let more = Row { depth: 2, kind: RowKind::More { dir: "/a".into(), hidden: 3 } };
        assert_eq!(more.activation(), Activation::ShowAll("/a".into()));
        let group = Row { depth: 0, kind: RowKind::Group { label: "External", count: 1 } };
        assert_eq!((group.activation(), group.selectable()), (Activation::Nothing, false));
    }

    #[test]
    fn details_appear_under_their_external_session() {
        let mut fixture = Fixture::of(vec![
            session("e", "/p", SessionState::External { pid: Some(42) }, 2),
            session("box", "/p", SessionState::External { pid: None }, 1),
        ]);
        fixture.details = Some("e".into());
        assert_eq!(fixture.outline(folders), vec!["Local", "  ▾ /p [2/0]", "    e", "      (Running outside ccm (pid 42))", "    box"]);
        fixture.details = Some("box".into());
        assert_eq!(fixture.outline(status)[2..], ["  box".to_string(), "    (Running in a container)".to_string()]);
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
