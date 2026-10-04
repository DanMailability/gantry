//! The recent sessions pop-up (`Ctrl+Shift+R`): closed sessions by project, newest first, a few per project
//! unless searching or looking at one project; chips for the most recently used projects.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use protocol::message::{SearchHit, SessionInfo, SessionState, Speaker};

use crate::fuzzy::Fuzzy;
use crate::session_tree::{age, display_path, keep, labels, newest_first, session_title};

/// Everything the pop-up is built from.
pub struct RecentsInput<'a> {
    /// Claude sessions.
    pub sessions: &'a [&'a SessionInfo],
    /// Sessions a leaf of the layout shows (they are in the Active list, not here).
    pub open: &'a HashSet<String>,
    /// Home directory, shown as `~`.
    pub home: Option<&'a Path>,
    /// Search text (blank = none).
    pub query: &'a str,
    /// The picked project (`None` = all).
    pub project: Option<&'a Path>,
    /// Projects whose "Show N more" was used.
    pub expanded: &'a HashSet<PathBuf>,
    /// The project a directory belongs to.
    pub project_of: &'a dyn Fn(&Path) -> PathBuf,
    /// Current time, ms since the Unix epoch.
    pub now_ms: u64,
    /// Sessions shown per project before "Show N more" (no search, all projects).
    pub per_project: usize,
    /// Project chips shown before "More…".
    pub chips: usize,
    /// Conversation search hits for `query`, best first (the latest reply; may be for a slightly older query).
    pub hits: &'a [SearchHit],
    /// Where conversation search stands.
    pub content: ContentState,
}

/// Fewest non-space characters that also search inside conversations.
pub const CONTENT_MIN_CHARS: usize = 3;

/// Whether `query` also searches inside conversations.
pub fn searches_content(query: &str) -> bool {
    query.trim().chars().count() >= CONTENT_MIN_CHARS
}

/// Where conversation search stands for the pop-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentState {
    /// No reply for the current query yet.
    Waiting,
    /// The reply for the current query arrived.
    Ready,
    /// The daemon is still building the index (percent done).
    Indexing(u8),
    /// The daemon cannot search.
    Unavailable,
}

/// The matching message a conversation hit shows under its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// Who wrote it.
    pub speaker: Speaker,
    /// One line of it.
    pub text: String,
    /// Byte ranges of `text` to draw in bold: on character boundaries, in order, not overlapping.
    pub highlights: Vec<Range<usize>>,
}

impl Snippet {
    /// The snippet of `hit`, keeping only highlight ranges that can be drawn (a bad range would panic the renderer).
    pub fn from_hit(hit: &SearchHit) -> Snippet {
        let text = hit.snippet.clone();
        let mut ranges: Vec<Range<usize>> = hit
            .highlights
            .iter()
            .map(|&(start, end)| start as usize..end as usize)
            .filter(|r| r.start < r.end && r.end <= text.len())
            .filter(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
            .collect();
        ranges.sort_by_key(|r| r.start);
        let mut highlights: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            match highlights.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => highlights.push(range),
            }
        }
        Snippet { speaker: hit.speaker, text, highlights }
    }
}

/// One row of the pop-up's list.
#[derive(Clone, Debug, PartialEq)]
pub enum RecentItem {
    /// A project heading.
    Heading {
        /// Project directory.
        path: PathBuf,
        /// Displayed name.
        label: String,
        /// Its directory for display (`~/…`).
        display: String,
    },
    /// A closed session.
    Session {
        /// The session (boxed: it is far bigger than the other variants).
        session: Box<SessionInfo>,
        /// Time since last activity.
        age: String,
        /// The project it is listed under.
        project: PathBuf,
        /// The matching message, for a conversation hit (`None` = a title match).
        snippet: Option<Snippet>,
        /// Whether a leaf of the layout shows it (Enter focuses it).
        open: bool,
    },
    /// "Show N more" under a project.
    More {
        /// The project.
        project: PathBuf,
        /// Sessions not shown.
        hidden: usize,
    },
    /// The heading of the conversation hits.
    Section {
        /// Its text.
        label: String,
    },
    /// A line about the conversation search's state.
    Note {
        /// Its text.
        text: String,
    },
}

impl RecentItem {
    /// Whether the selection can land here (headings are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self, RecentItem::Heading { .. } | RecentItem::Section { .. } | RecentItem::Note { .. })
    }
}

/// What a project chip selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipTarget {
    /// Every project.
    All,
    /// One project.
    Project(PathBuf),
    /// Choose among all projects.
    More,
}

/// A project chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// What it selects.
    pub target: ChipTarget,
    /// Its text.
    pub label: String,
    /// Sessions in it (0 for More…).
    pub count: usize,
}

/// A project in the "More…" chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectItem {
    /// Project directory.
    pub path: PathBuf,
    /// Displayed name.
    pub label: String,
    /// Its directory for display.
    pub display: String,
    /// Its closed sessions.
    pub count: usize,
}

/// Closed sessions not shown by a leaf, by project: projects by their newest session, sessions newest first.
fn grouped<'a>(input: &RecentsInput<'a>) -> Vec<(PathBuf, Vec<&'a SessionInfo>)> {
    let mut by_project: HashMap<PathBuf, Vec<&'a SessionInfo>> = HashMap::new();
    for session in input.sessions.iter().copied() {
        if session.state == SessionState::Closed && !input.open.contains(&session.id) {
            by_project.entry((input.project_of)(&session.cwd)).or_default().push(session);
        }
    }
    let mut projects: Vec<(PathBuf, Vec<&'a SessionInfo>)> = by_project.into_iter().collect();
    for (_, sessions) in &mut projects {
        sessions.sort_by(newest_first);
    }
    projects.sort_by(|(a_path, a), (b_path, b)| {
        b[0].last_active_ms.cmp(&a[0].last_active_ms).then_with(|| a_path.cmp(b_path))
    });
    projects
}

fn in_scope(input: &RecentsInput<'_>, path: &Path) -> bool {
    input.project.is_none_or(|picked| picked == path)
}

/// The pop-up's list: title matches by project, then (for a long enough query) the conversation hits. Merging the
/// two later only changes this function.
pub fn items(input: &RecentsInput<'_>) -> Vec<RecentItem> {
    let mut items = title_items(input);
    if searches_content(input.query) {
        let rows = content_items(input, &items);
        items.extend(rows);
    }
    items
}

/// "Found in conversations": one row per hit not already listed, in the picked project, closed or open in a leaf
/// (sessions running outside Gantry are in the Active list), then a note on the search's state.
fn content_items(input: &RecentsInput<'_>, listed: &[RecentItem]) -> Vec<RecentItem> {
    let mut shown: HashSet<String> = listed
        .iter()
        .filter_map(|item| match item {
            RecentItem::Session { session, .. } => Some(session.id.clone()),
            _ => None,
        })
        .collect();
    let by_id: HashMap<&str, &SessionInfo> = input.sessions.iter().map(|s| (s.id.as_str(), *s)).collect();
    let mut rows = Vec::new();
    for hit in input.hits {
        let Some(session) = by_id.get(hit.session_id.as_str()).copied() else { continue };
        let open = input.open.contains(&session.id);
        if !(open || session.state == SessionState::Closed) || !shown.insert(session.id.clone()) {
            continue;
        }
        let project = (input.project_of)(&session.cwd);
        if !in_scope(input, &project) {
            continue;
        }
        rows.push(RecentItem::Session {
            session: Box::new(session.clone()),
            age: age(input.now_ms, session.last_active_ms),
            project,
            snippet: Some(Snippet::from_hit(hit)),
            open,
        });
    }
    let note = match input.content {
        ContentState::Indexing(percent) => Some(format!("Indexing conversations… {percent}%")),
        ContentState::Unavailable => Some("Conversation search is unavailable — see the daemon log".to_string()),
        // Hits that are all listed above (title matches, another project) leave no note: saying no conversation
        // mentions the query would be false.
        ContentState::Ready if input.hits.is_empty() => Some("No conversations mention this".to_string()),
        _ => None,
    };
    if rows.is_empty() && note.is_none() {
        return Vec::new();
    }
    let mut items = vec![RecentItem::Section { label: "Found in conversations".into() }];
    items.extend(rows);
    items.extend(note.map(|text| RecentItem::Note { text }));
    items
}

/// The title matches, by project.
fn title_items(input: &RecentsInput<'_>) -> Vec<RecentItem> {
    let grouped = grouped(input);
    let paths: Vec<PathBuf> = grouped.iter().map(|(path, _)| path.clone()).collect();
    let labels = labels(&paths, input.home);
    let mut fuzzy = Fuzzy::new(input.query);
    let limited = fuzzy.is_none() && input.project.is_none();
    let mut items = Vec::new();
    for (path, sessions) in grouped {
        if !in_scope(input, &path) {
            continue;
        }
        let matching: Vec<&SessionInfo> =
            sessions.into_iter().filter(|s| keep(&mut fuzzy, session_title(s), &s.cwd, input.home)).collect();
        if matching.is_empty() {
            continue;
        }
        let cap = if limited && !input.expanded.contains(&path) { input.per_project } else { usize::MAX };
        let display = display_path(&path, input.home);
        items.push(RecentItem::Heading { label: labels[&path].clone(), display, path: path.clone() });
        for session in matching.iter().take(cap) {
            items.push(RecentItem::Session {
                session: Box::new((*session).clone()),
                age: age(input.now_ms, session.last_active_ms),
                project: path.clone(),
                snippet: None,
                open: false,
            });
        }
        if matching.len() > cap {
            items.push(RecentItem::More { project: path, hidden: matching.len() - cap });
        }
    }
    items
}

/// The project chips: All, the `input.chips` projects with the newest sessions (plus the picked one), then More….
pub fn chips(input: &RecentsInput<'_>) -> Vec<Chip> {
    let grouped = grouped(input);
    let mut paths: Vec<PathBuf> = grouped.iter().map(|(path, _)| path.clone()).collect();
    if let Some(picked) = input.project
        && !paths.iter().any(|path| path == picked)
    {
        paths.push(picked.to_path_buf());
    }
    let labels = labels(&paths, input.home);
    let count = |path: &Path| grouped.iter().find(|(p, _)| p == path).map_or(0, |(_, sessions)| sessions.len());
    let total = grouped.iter().map(|(_, sessions)| sessions.len()).sum();
    let mut chips = vec![Chip { target: ChipTarget::All, label: "All projects".into(), count: total }];
    let mut shown: Vec<&PathBuf> = paths.iter().take(input.chips).collect();
    if let Some(picked) = input.project
        && !shown.iter().any(|path| path.as_path() == picked)
    {
        shown.push(paths.iter().find(|path| path.as_path() == picked).expect("added above"));
    }
    for path in shown {
        chips.push(Chip { target: ChipTarget::Project(path.clone()), label: labels[path].clone(), count: count(path) });
    }
    if grouped.len() > input.chips {
        chips.push(Chip { target: ChipTarget::More, label: "More…".into(), count: 0 });
    }
    chips
}

/// Every project, for "More…", filtered by the search (name and directory).
pub fn projects(input: &RecentsInput<'_>) -> Vec<ProjectItem> {
    let grouped = grouped(input);
    let paths: Vec<PathBuf> = grouped.iter().map(|(path, _)| path.clone()).collect();
    let labels = labels(&paths, input.home);
    let mut fuzzy = Fuzzy::new(input.query);
    grouped
        .into_iter()
        .filter(|(path, _)| keep(&mut fuzzy, &labels[path], path, input.home))
        .map(|(path, sessions)| ProjectItem {
            label: labels[&path].clone(),
            display: display_path(&path, input.home),
            count: sessions.len(),
            path,
        })
        .collect()
}

/// Sessions matching the search, and sessions in scope (the picked project, else all).
pub fn counts(input: &RecentsInput<'_>) -> (usize, usize) {
    let mut fuzzy = Fuzzy::new(input.query);
    let scoped: Vec<&SessionInfo> = grouped(input)
        .into_iter()
        .filter(|(path, _)| in_scope(input, path))
        .flat_map(|(_, sessions)| sessions)
        .collect();
    let matching = scoped.iter().filter(|s| keep(&mut fuzzy, session_title(s), &s.cwd, input.home)).count();
    (matching, scoped.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;
    const HOUR: u64 = 60 * 60 * 1000;

    fn closed(id: &str, cwd: &str, ago_hours: u64) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            cwd: cwd.into(),
            title: format!("title {id}"),
            last_active_ms: NOW - ago_hours * HOUR,
            state: SessionState::Closed,
            cwd_exists: true,
            status: None,
            status_since_ms: 0,
            container: None,
            claude_dir: None,
        }
    }

    fn project_of(cwd: &Path) -> PathBuf {
        cwd.ancestors().find(|dir| dir.parent() == Some(Path::new("/r"))).unwrap_or(cwd).to_path_buf()
    }

    struct Fixture {
        sessions: Vec<SessionInfo>,
        open: HashSet<String>,
        query: String,
        project: Option<PathBuf>,
        expanded: HashSet<PathBuf>,
        per_project: usize,
        chips: usize,
        hits: Vec<SearchHit>,
        content: ContentState,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Fixture {
                sessions: Vec::new(),
                open: HashSet::new(),
                query: String::new(),
                project: None,
                expanded: HashSet::new(),
                per_project: 5,
                chips: 6,
                hits: Vec::new(),
                content: ContentState::Waiting,
            }
        }
    }

    impl Fixture {
        fn of(sessions: Vec<SessionInfo>) -> Fixture {
            Fixture { sessions, ..Fixture::default() }
        }

        fn with<T>(&self, f: impl FnOnce(&RecentsInput<'_>) -> T) -> T {
            let refs: Vec<&SessionInfo> = self.sessions.iter().collect();
            f(&RecentsInput {
                sessions: &refs,
                open: &self.open,
                home: Some(Path::new("/home/me")),
                query: &self.query,
                project: self.project.as_deref(),
                expanded: &self.expanded,
                project_of: &project_of,
                now_ms: NOW,
                per_project: self.per_project,
                chips: self.chips,
                hits: &self.hits,
                content: self.content,
            })
        }

        fn outline(&self) -> Vec<String> {
            self.with(items)
                .iter()
                .map(|item| match item {
                    RecentItem::Heading { label, .. } => format!("# {label}"),
                    RecentItem::Session { session, snippet: Some(_), open: true, .. } => {
                        format!("~{} open", session.id)
                    }
                    RecentItem::Session { session, snippet: Some(_), .. } => format!("~{}", session.id),
                    RecentItem::Section { label } => format!("## {label}"),
                    RecentItem::Note { text } => format!("! {text}"),
                    RecentItem::Session { session, .. } => session.id.clone(),
                    RecentItem::More { hidden, .. } => format!("+{hidden}"),
                })
                .collect()
        }

        fn chip_labels(&self) -> Vec<String> {
            self.with(chips).into_iter().map(|chip| format!("{} {}", chip.label, chip.count)).collect()
        }
    }

    fn many(project: &str, n: u64, start_hour: u64) -> Vec<SessionInfo> {
        (0..n).map(|i| closed(&format!("{project}{i}"), &format!("/r/{project}"), start_hour + i)).collect()
    }

    #[test]
    fn closed_sessions_by_project_newest_first_without_live_or_open_ones() {
        let mut live = closed("live", "/r/a", 0);
        live.state = SessionState::Managed { pane: 3 };
        let mut fixture = Fixture::of(vec![
            closed("a-old", "/r/a", 5),
            closed("b", "/r/b", 1),
            closed("a-new", "/r/a", 2),
            closed("tab", "/r/b", 0),
            live,
        ]);
        fixture.open.insert("tab".into());
        assert_eq!(fixture.outline(), vec!["# b", "b", "# a", "a-new", "a-old"]);
        assert_eq!(fixture.with(counts), (3, 3));
    }

    #[test]
    fn a_session_item_names_its_project() {
        let fixture = Fixture::of(vec![closed("a", "/r/a", 1), closed("b", "/r/b", 2)]);
        let items = fixture.with(items);
        let mut heading = None;
        for item in &items {
            match item {
                RecentItem::Heading { path, .. } => heading = Some(path.clone()),
                RecentItem::Session { project, .. } => assert_eq!(Some(project), heading.as_ref()),
                RecentItem::More { .. } | RecentItem::Section { .. } | RecentItem::Note { .. } => {}
            }
        }
        assert!(
            items
                .iter()
                .any(|item| matches!(item, RecentItem::Session { project, .. } if project == Path::new("/r/a")))
        );
    }

    #[test]
    fn five_per_project_with_show_more_until_expanded() {
        let mut fixture = Fixture::of(many("a", 8, 1));
        assert_eq!(fixture.outline(), vec!["# a", "a0", "a1", "a2", "a3", "a4", "+3"]);
        fixture.expanded.insert("/r/a".into());
        assert_eq!(fixture.outline().len(), 9);
    }

    #[test]
    fn searching_reaches_every_session_without_the_limit() {
        let mut sessions = many("a", 8, 1);
        sessions.push(closed("zzz", "/r/b", 1));
        let mut fixture = Fixture::of(sessions);
        fixture.query = "title a".into();
        let outline = fixture.outline();
        assert_eq!((outline[0].as_str(), outline.len()), ("# a", 9), "{outline:?}");
        assert_eq!(fixture.with(counts), (8, 9));
    }

    #[test]
    fn a_picked_project_shows_all_its_sessions_and_scopes_the_search() {
        let mut sessions = many("a", 8, 1);
        sessions.extend(many("b", 2, 0));
        let mut fixture = Fixture::of(sessions);
        fixture.project = Some("/r/a".into());
        assert_eq!(fixture.outline().len(), 9);
        assert!(!fixture.outline().iter().any(|row| row.starts_with('+')));
        fixture.query = "b".into();
        assert_eq!(fixture.outline(), Vec::<String>::new(), "b's sessions are outside the picked project");
        assert_eq!(fixture.with(counts), (0, 8));
    }

    #[test]
    fn chips_are_all_then_the_six_newest_projects_then_more() {
        let sessions: Vec<SessionInfo> = (0..8).map(|i| closed(&format!("s{i}"), &format!("/r/p{i}"), i)).collect();
        let mut fixture = Fixture::of(sessions);
        assert_eq!(
            fixture.chip_labels(),
            vec!["All projects 8", "p0 1", "p1 1", "p2 1", "p3 1", "p4 1", "p5 1", "More… 0"]
        );
        // A picked project beyond the six still gets its chip.
        fixture.project = Some("/r/p7".into());
        assert!(fixture.chip_labels().contains(&"p7 1".to_string()));
        // With six projects or fewer there is no More….
        fixture.sessions.truncate(6);
        fixture.project = None;
        assert!(!fixture.chip_labels().iter().any(|chip| chip.starts_with("More")));
    }

    #[test]
    fn a_picked_project_without_sessions_keeps_its_chip() {
        let mut fixture = Fixture::of(vec![closed("b", "/r/b", 1)]);
        fixture.project = Some("/r/a".into());
        assert_eq!(fixture.outline(), Vec::<String>::new());
        let picked = fixture.with(chips).into_iter().find(|chip| chip.target == ChipTarget::Project("/r/a".into()));
        assert_eq!(picked.map(|chip| chip.count), Some(0));
    }

    #[test]
    fn the_project_chooser_lists_every_project_filtered_by_the_search() {
        let mut fixture =
            Fixture::of(vec![closed("x", "/r/api", 1), closed("y", "/r/web", 2), closed("z", "/r/api", 3)]);
        let names = |fixture: &Fixture| {
            fixture.with(projects).into_iter().map(|p| format!("{} {}", p.label, p.count)).collect::<Vec<_>>()
        };
        assert_eq!(names(&fixture), vec!["api 2", "web 1"]);
        fixture.query = "web".into();
        assert_eq!(names(&fixture), vec!["web 1"]);
    }

    #[test]
    fn sessions_whose_folder_is_gone_are_listed() {
        let gone = SessionInfo { cwd_exists: false, ..closed("gone", "/r/a", 1) };
        let fixture = Fixture::of(vec![gone]);
        assert_eq!(fixture.outline(), vec!["# a", "gone"]);
    }

    #[test]
    fn sizes_come_from_the_input() {
        let sessions: Vec<SessionInfo> =
            (0..4).map(|n| closed(&format!("s{n}"), "/r/a", n + 1)).chain([closed("t", "/r/b", 9)]).collect();
        let one = Fixture { per_project: 1, ..Fixture::of(sessions.clone()) };
        assert_eq!(one.outline(), vec!["# a", "s0", "+3", "# b", "t"]);
        let no_chips = Fixture { chips: 0, ..Fixture::of(sessions) };
        assert_eq!(no_chips.chip_labels(), vec!["All projects 5", "More… 0"]);
    }

    fn hit(id: &str, text: &str, highlights: Vec<(u32, u32)>) -> SearchHit {
        SearchHit {
            session_id: id.into(),
            claude_dir: "/home/me/.claude".into(),
            speaker: Speaker::You,
            snippet: text.into(),
            highlights,
        }
    }

    #[test]
    fn content_hits_follow_the_title_matches_without_repeating_them() {
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1), closed("b1", "/r/b", 2), closed("b2", "/r/b", 3)]);
        fixture.query = "title a1".into();
        fixture.hits = vec![hit("b2", "x", vec![]), hit("a1", "y", vec![]), hit("b1", "z", vec![])];
        fixture.content = ContentState::Ready;
        assert_eq!(fixture.outline(), vec!["# a", "a1", "## Found in conversations", "~b2", "~b1"]);
    }

    #[test]
    fn no_content_section_for_short_queries() {
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1)]);
        fixture.query = "a".into();
        fixture.hits = vec![hit("a1", "x", vec![])];
        fixture.content = ContentState::Ready;
        assert!(!fixture.outline().iter().any(|row| row.starts_with("##")));
        assert!(!searches_content(" a "));
        assert!(!searches_content("in"), "two letters would list half the history");
        assert!(searches_content("abc"));
    }

    #[test]
    fn content_hits_respect_the_picked_project() {
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1), closed("b1", "/r/b", 2)]);
        fixture.query = "zzz".into();
        fixture.project = Some("/r/b".into());
        fixture.hits = vec![hit("a1", "x", vec![]), hit("b1", "y", vec![])];
        fixture.content = ContentState::Ready;
        assert_eq!(fixture.outline(), vec!["## Found in conversations", "~b1"]);
    }

    #[test]
    fn open_sessions_are_hits_and_external_ones_are_not() {
        let mut tab = closed("tab", "/r/a", 1);
        tab.state = SessionState::Managed { pane: 2 };
        let mut elsewhere = closed("elsewhere", "/r/a", 1);
        elsewhere.state = SessionState::External { pid: Some(9) };
        let mut fixture = Fixture::of(vec![tab, elsewhere, closed("unknown-to-hits", "/r/a", 1)]);
        fixture.open.insert("tab".into());
        fixture.query = "zzz".into();
        fixture.hits = vec![hit("tab", "x", vec![]), hit("elsewhere", "y", vec![]), hit("not-listed", "z", vec![])];
        fixture.content = ContentState::Ready;
        assert_eq!(fixture.outline(), vec!["## Found in conversations", "~tab open"]);
    }

    #[test]
    fn hits_that_are_all_title_matches_leave_no_section() {
        // "No conversations mention this" would be false: the conversation is listed above.
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1)]);
        fixture.query = "title a1".into();
        fixture.hits = vec![hit("a1", "x", vec![])];
        fixture.content = ContentState::Ready;
        assert_eq!(fixture.outline(), vec!["# a", "a1"]);
    }

    #[test]
    fn the_section_explains_its_state() {
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1)]);
        fixture.query = "zzz".into();
        fixture.content = ContentState::Waiting;
        assert!(fixture.outline().is_empty(), "nothing until the first reply");
        fixture.content = ContentState::Ready;
        assert_eq!(fixture.outline(), vec!["## Found in conversations", "! No conversations mention this"]);
        fixture.content = ContentState::Indexing(40);
        fixture.hits = vec![hit("a1", "x", vec![])];
        assert_eq!(fixture.outline(), vec!["## Found in conversations", "~a1", "! Indexing conversations… 40%"]);
        fixture.content = ContentState::Unavailable;
        fixture.hits.clear();
        assert_eq!(
            fixture.outline(),
            vec!["## Found in conversations", "! Conversation search is unavailable — see the daemon log"]
        );
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn title_matches_have_no_snippet_and_hits_carry_theirs() {
        let mut fixture = Fixture::of(vec![closed("a1", "/r/a", 1), closed("b1", "/r/b", 2)]);
        fixture.query = "title a1".into();
        fixture.hits = vec![hit("b1", "the fallback", vec![(4, 12)])];
        fixture.content = ContentState::Ready;
        let items = fixture.with(items);
        let snippets: Vec<Option<&Snippet>> = items
            .iter()
            .filter_map(|item| match item {
                RecentItem::Session { snippet, .. } => Some(snippet.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(snippets[0], None);
        let snippet = snippets[1].unwrap();
        assert_eq!(
            (snippet.speaker, snippet.text.as_str(), snippet.highlights.clone()),
            (Speaker::You, "the fallback", vec![4..12])
        );
    }

    #[test]
    fn highlights_off_char_boundaries_are_dropped() {
        // "שלום" is 8 bytes; 1..3 cuts a character, 0..8 does not; 20..30 is past the end; 6..2 is backwards.
        let snippet = Snippet::from_hit(&hit("s", "שלום fallback", vec![(1, 3), (0, 8), (20, 30), (6, 2), (9, 17)]));
        assert_eq!(snippet.highlights, vec![0..8, 9..17]);
    }

    #[test]
    fn overlapping_highlights_are_merged_in_order() {
        let snippet = Snippet::from_hit(&hit("s", "abcdefghij", vec![(5, 8), (0, 2), (1, 4)]));
        assert_eq!(snippet.highlights, vec![0..4, 5..8]);
    }
}
