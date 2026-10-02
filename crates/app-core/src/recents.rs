//! The recent sessions pop-up (`Ctrl+Shift+R`): closed sessions by project, newest first, a few per project
//! unless searching or looking at one project; chips for the most recently used projects.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::message::{SessionInfo, SessionState};

use crate::fuzzy::Fuzzy;
use crate::session_tree::{age, display_path, keep, labels, newest_first, session_title};

/// Sessions shown per project before "Show N more" (no search, all projects).
pub const PER_PROJECT: usize = 5;
/// Project chips shown before "More…".
pub const CHIPS: usize = 6;

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
        /// The session.
        session: SessionInfo,
        /// Time since last activity.
        age: String,
    },
    /// "Show N more" under a project.
    More {
        /// The project.
        project: PathBuf,
        /// Sessions not shown.
        hidden: usize,
    },
}

impl RecentItem {
    /// Whether the selection can land here (headings are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self, RecentItem::Heading { .. })
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
    projects.sort_by(|(a_path, a), (b_path, b)| b[0].last_active_ms.cmp(&a[0].last_active_ms).then_with(|| a_path.cmp(b_path)));
    projects
}

fn in_scope(input: &RecentsInput<'_>, path: &Path) -> bool {
    input.project.is_none_or(|picked| picked == path)
}

/// The pop-up's list.
pub fn items(input: &RecentsInput<'_>) -> Vec<RecentItem> {
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
        let cap = if limited && !input.expanded.contains(&path) { PER_PROJECT } else { usize::MAX };
        let display = display_path(&path, input.home);
        items.push(RecentItem::Heading { label: labels[&path].clone(), display, path: path.clone() });
        for session in matching.iter().take(cap) {
            items.push(RecentItem::Session { session: (*session).clone(), age: age(input.now_ms, session.last_active_ms) });
        }
        if matching.len() > cap {
            items.push(RecentItem::More { project: path, hidden: matching.len() - cap });
        }
    }
    items
}

/// The project chips: All, the [`CHIPS`] projects with the newest sessions (plus the picked one), then More….
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
    let mut shown: Vec<&PathBuf> = paths.iter().take(CHIPS).collect();
    if let Some(picked) = input.project
        && !shown.iter().any(|path| path.as_path() == picked)
    {
        shown.push(paths.iter().find(|path| path.as_path() == picked).expect("added above"));
    }
    for path in shown {
        chips.push(Chip { target: ChipTarget::Project(path.clone()), label: labels[path].clone(), count: count(path) });
    }
    if grouped.len() > CHIPS {
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
        .map(|(path, sessions)| ProjectItem { label: labels[&path].clone(), display: display_path(&path, input.home), count: sessions.len(), path })
        .collect()
}

/// Sessions matching the search, and sessions in scope (the picked project, else all).
pub fn counts(input: &RecentsInput<'_>) -> (usize, usize) {
    let mut fuzzy = Fuzzy::new(input.query);
    let scoped: Vec<&SessionInfo> =
        grouped(input).into_iter().filter(|(path, _)| in_scope(input, path)).flat_map(|(_, sessions)| sessions).collect();
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
        }
    }

    fn project_of(cwd: &Path) -> PathBuf {
        cwd.ancestors().find(|dir| dir.parent() == Some(Path::new("/r"))).unwrap_or(cwd).to_path_buf()
    }

    #[derive(Default)]
    struct Fixture {
        sessions: Vec<SessionInfo>,
        open: HashSet<String>,
        query: String,
        project: Option<PathBuf>,
        expanded: HashSet<PathBuf>,
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
            })
        }

        fn outline(&self) -> Vec<String> {
            self.with(items)
                .iter()
                .map(|item| match item {
                    RecentItem::Heading { label, .. } => format!("# {label}"),
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
        let mut fixture = Fixture::of(vec![closed("a-old", "/r/a", 5), closed("b", "/r/b", 1), closed("a-new", "/r/a", 2), closed("tab", "/r/b", 0), live]);
        fixture.open.insert("tab".into());
        assert_eq!(fixture.outline(), vec!["# b", "b", "# a", "a-new", "a-old"]);
        assert_eq!(fixture.with(counts), (3, 3));
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
        assert_eq!(fixture.chip_labels(), vec!["All projects 8", "p0 1", "p1 1", "p2 1", "p3 1", "p4 1", "p5 1", "More… 0"]);
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
        let mut fixture = Fixture::of(vec![closed("x", "/r/api", 1), closed("y", "/r/web", 2), closed("z", "/r/api", 3)]);
        let names = |fixture: &Fixture| fixture.with(projects).into_iter().map(|p| format!("{} {}", p.label, p.count)).collect::<Vec<_>>();
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
}
