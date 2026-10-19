//! What the GUI knows about each repo it is showing, and when to ask the daemon again (the git view's polling).
//!
//! Pure bookkeeping: `workspace_view` says which repos are on screen, asks for whatever `due` returns and feeds the
//! answers to `apply`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use app_core::git_view::next_poll;
use protocol::host::HostId;
use protocol::message::{GitCommit, GitProblem, GitState};

/// A request with no answer for this long is forgotten, so the next ask can go out.
const LOST_AFTER: Duration = Duration::from_secs(30);

/// What is known about one repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoView {
    /// Asked, no answer yet.
    Loading,
    /// The latest state.
    Repo(GitState),
    /// The folder is not in a git repo.
    NotRepo,
    /// `git` is not installed on that host.
    NoGit,
    /// Anything else, readable.
    Failed(String),
}

struct Entry {
    view: RepoView,
    /// A Claude session in it is busy, or the panel shows it.
    fast: bool,
    /// The request waiting for its answer, and when it was sent.
    in_flight: Option<(u64, Instant)>,
    /// When the last answer came and how long git took; `None` = ask at once.
    answered: Option<(Instant, u32)>,
    /// Look again soon (window focused, panel toggled, a turn ended): sooner than the poll rate, but never sooner than
    /// ten times the last run.
    force: bool,
    /// The commits of the last state that had them.
    commits: Option<Vec<GitCommit>>,
    /// `own_commits` of the state that had them.
    own_commits: bool,
}

/// The repos on screen, keyed by host and folder.
#[derive(Default)]
pub struct GitRepos {
    repos: HashMap<(HostId, String), Entry>,
    next_request: u64,
}

impl GitRepos {
    /// A fresh request id, for a diff asked for outside the polling (ids never repeat with the polling's).
    pub fn new_request(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    /// What is known about `folder` on `host`.
    pub fn view(&self, host: &HostId, folder: &str) -> Option<&RepoView> {
        self.repos.get(&(host.clone(), folder.to_string())).map(|entry| &entry.view)
    }

    /// Replace the set of repos on screen: new ones start out due, ones no longer listed are dropped, the rest keep
    /// their state and take the new `fast` flag. The flag is true for a repo with a busy Claude session or in the panel.
    pub fn set_wanted(&mut self, wanted: impl IntoIterator<Item = (HostId, String, bool)>) {
        let mut next: HashMap<(HostId, String), Entry> = HashMap::new();
        for (host, folder, fast) in wanted {
            let key = (host, folder);
            if let Some(entry) = next.get_mut(&key) {
                // Another pane of the same repo: the faster rate wins.
                entry.fast |= fast;
                continue;
            }
            let entry = match self.repos.remove(&key) {
                Some(mut entry) => {
                    if entry.fast && !fast {
                        // Left the fast rate (a Claude turn ended, the panel closed): look again now.
                        entry.force = true;
                    }
                    entry.fast = fast;
                    entry
                }
                None => Entry {
                    view: RepoView::Loading,
                    fast,
                    in_flight: None,
                    answered: None,
                    force: false,
                    commits: None,
                    own_commits: false,
                },
            };
            next.insert(key, entry);
        }
        self.repos = next;
    }

    /// Make every repo due now (panel opened, window focused, a Claude turn finished).
    pub fn refresh_now(&mut self) {
        for entry in self.repos.values_mut() {
            entry.force = true;
        }
    }

    /// The repos to ask for now: host, folder, the `HEAD` whose commits are already known, and the request id. Nothing
    /// while the window is inactive.
    pub fn due(&mut self, now: Instant, window_active: bool) -> Vec<(HostId, String, Option<String>, u64)> {
        let mut asks = Vec::new();
        if !window_active {
            return asks;
        }
        for ((host, folder), entry) in &mut self.repos {
            if let Some((_, sent)) = entry.in_flight {
                if now.duration_since(sent) < LOST_AFTER {
                    continue;
                }
                entry.in_flight = None;
            }
            let ready = match entry.answered {
                None => true,
                Some((at, took_ms)) => {
                    let wait = if entry.force {
                        Duration::from_millis(10 * u64::from(took_ms))
                    } else {
                        next_poll(took_ms, entry.fast)
                    };
                    now.duration_since(at) >= wait
                }
            };
            if !ready {
                continue;
            }
            entry.force = false;
            self.next_request += 1;
            entry.in_flight = Some((self.next_request, now));
            let known_head = match &entry.view {
                RepoView::Repo(state) => state.head.clone(),
                _ => None,
            };
            asks.push((host.clone(), folder.clone(), known_head, self.next_request));
        }
        asks
    }

    /// Take an answer. Returns whether the view changed (a reply nobody waits for changes nothing).
    pub fn apply(&mut self, host: &HostId, request: u64, result: Result<GitState, GitProblem>, now: Instant) -> bool {
        let Some(entry) = self
            .repos
            .iter_mut()
            .find(|((entry_host, _), entry)| entry_host == host && entry.in_flight.is_some_and(|(id, _)| id == request))
            .map(|(_, entry)| entry)
        else {
            return false;
        };
        let took_here = entry
            .in_flight
            .map_or(0, |(_, sent)| now.duration_since(sent).as_millis().min(u128::from(u32::MAX)) as u32);
        entry.in_flight = None;
        let view = match result {
            Ok(mut state) => {
                if let Some(commits) = state.commits.take() {
                    entry.commits = Some(commits);
                    entry.own_commits = state.own_commits;
                }
                state.commits = entry.commits.clone();
                state.own_commits = entry.own_commits;
                entry.answered = Some((now, state.took_ms));
                RepoView::Repo(state)
            }
            Err(problem) => {
                // The call's own time is not known: how long it took to come back is the best guess.
                entry.answered = Some((now, took_here));
                match problem {
                    GitProblem::NotRepo => RepoView::NotRepo,
                    GitProblem::NoGit => RepoView::NoGit,
                    GitProblem::Failed { reason } => RepoView::Failed(reason),
                }
            }
        };
        let changed = entry.view != view;
        entry.view = view;
        changed
    }

    /// Forget everything about `host` (it disconnected).
    pub fn forget_host(&mut self, host: &HostId) {
        self.repos.retain(|(entry_host, _), _| entry_host != host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> HostId {
        HostId::local()
    }

    fn repo_state(took_ms: u32) -> GitState {
        GitState {
            root: "/r".into(),
            branch: Some("main".into()),
            head: Some("abc1234".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Vec::new(),
            more: 0,
            commits: Some(Vec::new()),
            took_ms,
            last_commit: None,
            filters_skipped: false,
            own_commits: false,
        }
    }

    /// One repo on screen, asked for once and answered at `t0`.
    fn answered(fast: bool, took_ms: u32) -> (GitRepos, Instant) {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), fast)]);
        let asks = repos.due(t0, true);
        assert!(repos.apply(&local(), asks[0].3, Ok(repo_state(took_ms)), t0));
        (repos, t0)
    }

    #[test]
    fn busy_repo_is_asked_every_five_seconds() {
        let (mut repos, t0) = answered(true, 40);
        assert!(repos.due(t0 + Duration::from_millis(4900), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(5), true).len(), 1);
    }

    #[test]
    fn quiet_repo_is_asked_once_a_minute() {
        let (mut repos, t0) = answered(false, 40);
        assert!(repos.due(t0 + Duration::from_secs(59), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(60), true).len(), 1);
    }

    #[test]
    fn panel_repo_is_fast_even_when_quiet() {
        let (mut repos, t0) = answered(false, 40);
        repos.set_wanted([(local(), "/r".into(), true)]);
        assert_eq!(repos.due(t0 + Duration::from_secs(5), true).len(), 1);
    }

    #[test]
    fn repo_turning_busy_is_due_at_once_if_its_last_ask_is_older_than_five_seconds() {
        let (mut repos, t0) = answered(false, 40);
        let later = t0 + Duration::from_secs(20);
        assert!(repos.due(later, true).is_empty());
        repos.set_wanted([(local(), "/r".into(), true)]);
        assert_eq!(repos.due(later, true).len(), 1);
    }

    #[test]
    fn due_asks_once_then_waits_the_paced_delay() {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true)]);
        let first = repos.due(t0, true);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].2, None);
        // Still waiting for the answer: not asked again.
        assert!(repos.due(t0 + Duration::from_secs(10), true).is_empty());
        // A slow answer (3 s) makes the next ask wait 30 s.
        repos.apply(&local(), first[0].3, Ok(repo_state(3000)), t0 + Duration::from_secs(10));
        assert!(repos.due(t0 + Duration::from_secs(39), true).is_empty());
        let second = repos.due(t0 + Duration::from_secs(40), true);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].2.as_deref(), Some("abc1234"));
    }

    #[test]
    fn not_wanted_repos_are_dropped_and_never_asked() {
        let (mut repos, t0) = answered(true, 40);
        repos.set_wanted(Vec::new());
        assert_eq!(repos.view(&local(), "/r"), None);
        assert!(repos.due(t0 + Duration::from_secs(120), true).is_empty());
        assert_eq!(repos.view(&local(), "/r"), None);
    }

    #[test]
    fn inactive_window_asks_nothing() {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true)]);
        assert!(repos.due(t0, false).is_empty());
        assert_eq!(repos.due(t0, true).len(), 1);
    }

    #[test]
    fn refresh_now_makes_a_waiting_repo_due() {
        let (mut repos, t0) = answered(false, 40);
        assert!(repos.due(t0 + Duration::from_secs(1), true).is_empty());
        repos.refresh_now();
        assert_eq!(repos.due(t0 + Duration::from_secs(1), true).len(), 1);
    }

    #[test]
    fn a_reply_for_an_unknown_request_is_ignored() {
        let (mut repos, t0) = answered(true, 40);
        assert!(!repos.apply(&local(), 999, Err(GitProblem::NotRepo), t0));
        assert!(matches!(repos.view(&local(), "/r"), Some(RepoView::Repo(_))));
    }

    #[test]
    fn an_older_reply_does_not_replace_a_newer_one() {
        let (mut repos, t0) = answered(true, 40);
        let second = repos.due(t0 + Duration::from_secs(5), true);
        let mut newer = repo_state(40);
        newer.branch = Some("newer".into());
        assert!(repos.apply(&local(), second[0].3, Ok(newer), t0 + Duration::from_secs(5)));
        // The first request's answer arrives again, late.
        let mut stale = repo_state(40);
        stale.branch = Some("stale".into());
        assert!(!repos.apply(&local(), second[0].3 - 1, Ok(stale), t0 + Duration::from_secs(6)));
        let Some(RepoView::Repo(state)) = repos.view(&local(), "/r") else { panic!("no repo") };
        assert_eq!(state.branch.as_deref(), Some("newer"));
    }

    #[test]
    fn a_lost_request_is_asked_again_after_thirty_seconds() {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true)]);
        assert_eq!(repos.due(t0, true).len(), 1);
        assert!(repos.due(t0 + Duration::from_secs(29), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(31), true).len(), 1);
    }

    #[test]
    fn problems_become_views_and_commits_are_kept() {
        let (mut repos, t0) = answered(true, 40);
        let mut with_commits = repo_state(40);
        with_commits.commits = Some(vec![GitCommit {
            hash: "abc1234".into(),
            subject: "s".into(),
            author: "a".into(),
            time: 1,
            unpushed: false,
        }]);
        let ask = repos.due(t0 + Duration::from_secs(5), true);
        repos.apply(&local(), ask[0].3, Ok(with_commits), t0 + Duration::from_secs(5));
        let ask = repos.due(t0 + Duration::from_secs(10), true);
        let mut unchanged_head = repo_state(40);
        unchanged_head.commits = None;
        repos.apply(&local(), ask[0].3, Ok(unchanged_head), t0 + Duration::from_secs(10));
        let Some(RepoView::Repo(state)) = repos.view(&local(), "/r") else { panic!("no repo") };
        assert_eq!(state.commits.as_ref().map(Vec::len), Some(1));
        let ask = repos.due(t0 + Duration::from_secs(15), true);
        repos.apply(&local(), ask[0].3, Err(GitProblem::NotRepo), t0 + Duration::from_secs(15));
        assert_eq!(repos.view(&local(), "/r"), Some(&RepoView::NotRepo));
    }

    #[test]
    fn repo_leaving_busy_is_asked_at_once() {
        let (mut repos, t0) = answered(true, 40);
        assert!(repos.due(t0 + Duration::from_secs(1), true).is_empty());
        repos.set_wanted([(local(), "/r".into(), false)]);
        assert_eq!(repos.due(t0 + Duration::from_secs(1), true).len(), 1);
    }

    #[test]
    fn two_panes_in_one_repo_share_one_entry_and_fast_wins() {
        let (mut repos, t0) = answered(false, 40);
        repos.set_wanted([(local(), "/r".into(), false), (local(), "/r".into(), true)]);
        assert!(repos.view(&local(), "/r").is_some());
        assert!(repos.due(t0 + Duration::from_secs(1), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(5), true).len(), 1);
    }

    #[test]
    fn request_ids_never_repeat_between_polls_and_other_asks() {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true)]);
        let polled = repos.due(t0, true)[0].3;
        let other = repos.new_request();
        assert_ne!(polled, other);
        assert_ne!(repos.new_request(), other);
    }

    #[test]
    fn a_forced_refresh_still_waits_ten_times_the_last_run() {
        let (mut repos, t0) = answered(true, 3000);
        repos.refresh_now();
        assert!(repos.due(t0 + Duration::from_secs(20), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(30), true).len(), 1);
    }

    #[test]
    fn a_busy_idle_flap_on_a_slow_repo_does_not_stack_runs() {
        let (mut repos, t0) = answered(true, 3000);
        repos.set_wanted([(local(), "/r".into(), false)]);
        repos.set_wanted([(local(), "/r".into(), true)]);
        repos.set_wanted([(local(), "/r".into(), false)]);
        assert!(repos.due(t0 + Duration::from_secs(1), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(30), true).len(), 1);
    }

    #[test]
    fn a_slow_failure_paces_the_next_ask() {
        let t0 = Instant::now();
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true)]);
        let ask = repos.due(t0, true);
        // The call failed after 4 s: the next ask waits 10x that, not the 5 s of the fast rate.
        repos.apply(
            &local(),
            ask[0].3,
            Err(GitProblem::Failed { reason: "timed out".into() }),
            t0 + Duration::from_secs(4),
        );
        assert!(repos.due(t0 + Duration::from_secs(30), true).is_empty());
        assert_eq!(repos.due(t0 + Duration::from_secs(45), true).len(), 1);
    }

    #[test]
    fn host_disconnect_clears_its_repos() {
        let other = HostId::named("box");
        let mut repos = GitRepos::default();
        repos.set_wanted([(local(), "/r".into(), true), (other.clone(), "/r".into(), true)]);
        repos.forget_host(&other);
        assert!(repos.view(&other, "/r").is_none());
        assert!(repos.view(&local(), "/r").is_some());
    }
}
