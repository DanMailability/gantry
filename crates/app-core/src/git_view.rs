//! The git view's pure side: chip text, change counts, relative ages and how often to ask the daemon.

use std::time::Duration;

use protocol::message::{GitFileStatus, GitState};

/// How often a repo is asked for while a Claude session in it is busy or the panel shows it.
pub const POLL_FAST_MS: u64 = 5_000;
/// How often any other repo on screen is asked for.
pub const POLL_QUIET_MS: u64 = 60_000;

/// What the chip shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// Branch name, else the short hash of a detached `HEAD`, else "no commits".
    pub branch: String,
    /// Something is changed.
    pub dirty: bool,
    /// Changed files, including those beyond the listed ones.
    pub changed: usize,
    /// Commits ahead of the upstream.
    pub ahead: u32,
    /// Commits behind the upstream.
    pub behind: u32,
}

/// The chip for `state`.
pub fn chip(state: &GitState) -> Chip {
    let branch = state.branch.clone().or_else(|| state.head.clone()).unwrap_or_else(|| "no commits".to_string());
    let changed = state.files.len() + state.more as usize;
    Chip { branch, dirty: changed > 0, changed, ahead: state.ahead, behind: state.behind }
}

/// Changed files by kind, for the hover card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Modified, renamed and conflicted.
    pub modified: usize,
    /// New and staged.
    pub added: usize,
    /// Removed.
    pub deleted: usize,
    /// Not tracked.
    pub untracked: usize,
}

/// The listed files of `state` by kind.
pub fn counts(state: &GitState) -> Counts {
    let mut counts = Counts::default();
    for file in &state.files {
        match file.status {
            GitFileStatus::Modified | GitFileStatus::Renamed | GitFileStatus::Conflicted => counts.modified += 1,
            GitFileStatus::Added => counts.added += 1,
            GitFileStatus::Deleted => counts.deleted += 1,
            GitFileStatus::Untracked => counts.untracked += 1,
        }
    }
    counts
}

/// How long to wait before asking again after a run of `took_ms`: the poll interval (`fast` or quiet), or ten times
/// the run, whichever is longer, so a slow repo is never hammered.
pub fn next_poll(took_ms: u32, fast: bool) -> Duration {
    let base = if fast { POLL_FAST_MS } else { POLL_QUIET_MS };
    Duration::from_millis(base.max(10 * u64::from(took_ms)))
}

/// `then` as a short age relative to `now` (both seconds since the Unix epoch): "just now", "12 min ago", "3 h ago",
/// "yesterday", "5 d ago".
pub fn age(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".to_string(),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// The panel's width `started_ms` after it began opening (`opening`) or closing, `full` being its width when open:
/// 0 to `full` over `PANE_ANIM_MS` with ease-out, backwards when closing; clamped outside that span.
pub fn panel_width(full: f32, started_ms: f32, opening: bool) -> f32 {
    let t = crate::split_tree::ease_out_cubic(started_ms / crate::split_tree::PANE_ANIM_MS as f32);
    if opening { full * t } else { full * (1.0 - t) }
}

#[cfg(test)]
mod tests {
    use protocol::message::GitFile;

    use super::*;

    fn state(branch: Option<&str>, head: Option<&str>) -> GitState {
        GitState {
            root: "/r".into(),
            branch: branch.map(String::from),
            head: head.map(String::from),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Vec::new(),
            more: 0,
            commits: None,
            took_ms: 0,
            last_commit: None,
            filters_skipped: false,
            own_commits: false,
        }
    }

    fn file(status: GitFileStatus) -> GitFile {
        GitFile { path: "p".into(), old_path: None, status, added: None, removed: None }
    }

    #[test]
    fn chip_uses_branch_then_head_then_no_commits() {
        assert_eq!(chip(&state(Some("main"), Some("abc1234"))).branch, "main");
        assert_eq!(chip(&state(None, Some("abc1234"))).branch, "abc1234");
        assert_eq!(chip(&state(Some("main"), None)).branch, "main");
        assert_eq!(chip(&state(None, None)).branch, "no commits");
    }

    #[test]
    fn chip_changed_includes_more() {
        let mut s = state(Some("main"), Some("abc1234"));
        assert_eq!((chip(&s).changed, chip(&s).dirty), (0, false));
        s.files = vec![file(GitFileStatus::Modified), file(GitFileStatus::Untracked)];
        s.more = 5;
        s.ahead = 2;
        s.behind = 1;
        let chip = chip(&s);
        assert_eq!((chip.changed, chip.dirty, chip.ahead, chip.behind), (7, true, 2, 1));
    }

    #[test]
    fn counts_group_statuses() {
        let mut s = state(Some("main"), Some("abc1234"));
        s.files = [
            GitFileStatus::Modified,
            GitFileStatus::Renamed,
            GitFileStatus::Conflicted,
            GitFileStatus::Added,
            GitFileStatus::Deleted,
            GitFileStatus::Untracked,
            GitFileStatus::Untracked,
        ]
        .map(file)
        .to_vec();
        assert_eq!(counts(&s), Counts { modified: 3, added: 1, deleted: 1, untracked: 2 });
    }

    #[test]
    fn next_poll_fast_is_five_seconds() {
        assert_eq!(next_poll(40, true), Duration::from_secs(5));
    }

    #[test]
    fn next_poll_quiet_is_a_minute() {
        assert_eq!(next_poll(40, false), Duration::from_secs(60));
    }

    #[test]
    fn next_poll_waits_ten_times_a_slow_run() {
        assert_eq!(next_poll(2500, true), Duration::from_secs(25));
        assert_eq!(next_poll(7000, false), Duration::from_secs(70));
    }

    #[test]
    fn age_buckets() {
        let now = 1_000_000;
        assert_eq!(age(now, now - 20), "just now");
        assert_eq!(age(now, now - 12 * 60), "12 min ago");
        assert_eq!(age(now, now - 3 * 3600), "3 h ago");
        assert_eq!(age(now, now - 30 * 3600), "yesterday");
        assert_eq!(age(now, now - 5 * 86_400), "5 d ago");
        assert_eq!(age(now, now + 100), "just now");
    }

    #[test]
    fn panel_width_opening_starts_at_zero_and_ends_full() {
        assert_eq!(panel_width(290.0, 0.0, true), 0.0);
        assert_eq!(panel_width(290.0, 300.0, true), 290.0);
        assert_eq!(panel_width(290.0, 900.0, true), 290.0);
    }

    #[test]
    fn panel_width_closing_runs_backwards() {
        assert_eq!(panel_width(290.0, 0.0, false), 290.0);
        assert_eq!(panel_width(290.0, 300.0, false), 0.0);
        assert!(panel_width(290.0, 100.0, false) < panel_width(290.0, 50.0, false));
    }

    #[test]
    fn panel_width_is_eased() {
        assert!(panel_width(290.0, 150.0, true) > 145.0);
        assert_eq!(panel_width(290.0, -20.0, true), 0.0);
    }
}
