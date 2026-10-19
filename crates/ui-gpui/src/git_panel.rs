//! The git panel's rows: changed files with their +/- counts, and recent commits. `workspace_view` lays the panel out
//! beside the panes and wires the clicks.

use app_core::git_view::age;
use app_core::session_tree::Tone;
use gpui::{FontWeight, Stateful, div, prelude::*, px, rgb, rgba, svg};
use protocol::message::{GitCommit, GitCommitDetail, GitFile, GitFileStatus, GitState};

use crate::assets::{BRANCH_ICON, CLOSE_ICON};
use crate::theme::{RADIUS_SMALL, Theme};

/// The panel's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelTab {
    /// Changed files.
    Changes,
    /// Recent commits.
    Commits,
}

/// The letter before a changed file: M, A, D, R, ? or U (conflict).
pub(crate) fn status_letter(status: GitFileStatus) -> char {
    match status {
        GitFileStatus::Modified => 'M',
        GitFileStatus::Added => 'A',
        GitFileStatus::Deleted => 'D',
        GitFileStatus::Renamed => 'R',
        GitFileStatus::Untracked => '?',
        GitFileStatus::Conflicted => 'U',
    }
}

/// What follows a file's name: `+28 −7`, `+13` or `−2` when only one side changed, "new" for an untracked file, "" when
/// git has no counts (binary, no commits yet).
pub(crate) fn count_text(file: &GitFile) -> String {
    if file.status == GitFileStatus::Untracked {
        return "new".to_string();
    }
    let mut parts = Vec::new();
    if let Some(added) = file.added.filter(|count| *count > 0) {
        parts.push(format!("+{added}"));
    }
    if let Some(removed) = file.removed.filter(|count| *count > 0) {
        parts.push(format!("−{removed}"));
    }
    parts.join(" ")
}

/// The commit's second line: its author and how long ago.
pub(crate) fn commit_meta(commit: &GitCommit, now: i64) -> String {
    format!("{} · {}", commit.author, age(now, commit.time))
}

/// The color of a file's status letter.
fn status_color(status: GitFileStatus, theme: &Theme) -> u32 {
    match status {
        GitFileStatus::Added => theme.tone(Tone::Done).dot,
        GitFileStatus::Deleted | GitFileStatus::Conflicted => theme.tokens.danger,
        GitFileStatus::Untracked => theme.tokens.muted,
        GitFileStatus::Modified | GitFileStatus::Renamed => theme.tone(Tone::NeedsInput).dot,
    }
}

/// A tab button of the panel's header.
pub(crate) fn tab_button(id: &'static str, label: String, active: bool, theme: &Theme) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px(theme.ui(12.))
        .py(theme.ui(8.))
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(if active { theme.tokens.accent } else { theme.tokens.muted }))
        .when(active, |tab| tab.border_b_2().border_color(rgb(theme.tokens.accent)))
        .child(label)
}

/// The muted red × that closes the panel and the diff page, like the pane's close button.
pub(crate) fn close_button(id: &'static str, theme: &Theme) -> Stateful<gpui::Div> {
    let danger = theme.tokens.danger;
    div()
        .id(id)
        .flex_none()
        .size(theme.ui(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SMALL - 2.))
        .cursor_pointer()
        .hover(|style| style.bg(rgba((danger << 8) | 0x24)))
        .child(svg().path(CLOSE_ICON).size(theme.ui(14.)).text_color(rgba((danger << 8) | 0xc7)))
}

/// The panel's top line: the branch (or the short hash of a detached `HEAD`), `↑a ↓b` and the upstream, then `close`.
pub(crate) fn branch_line(state: Option<&GitState>, close: Stateful<gpui::Div>, theme: &Theme) -> gpui::Div {
    let line = div()
        .flex_none()
        .h(theme.ui(30.))
        .px(theme.ui(12.))
        .flex()
        .items_center()
        .gap(theme.ui(6.))
        .border_b_1()
        .border_color(rgb(theme.tokens.line))
        .text_sm();
    let Some(state) = state else {
        return line.child(div().flex_1()).child(close);
    };
    let chip = app_core::git_view::chip(state);
    let mut sync = Vec::new();
    if chip.ahead > 0 {
        sync.push(format!("↑{}", chip.ahead));
    }
    if chip.behind > 0 {
        sync.push(format!("↓{}", chip.behind));
    }
    line.child(svg().path(BRANCH_ICON).flex_none().size(theme.ui(12.)).text_color(rgb(theme.tokens.accent)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.tokens.text))
                .child(chip.branch),
        )
        .children(
            (!sync.is_empty())
                .then(|| div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(sync.join(" "))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(theme.tokens.muted))
                .children(state.upstream.clone()),
        )
        .child(close)
}

/// A small section heading with an optional note on the right.
pub(crate) fn heading(label: &str, note: Option<String>, theme: &Theme) -> gpui::Div {
    div()
        .px(theme.ui(12.))
        .pt(theme.ui(8.))
        .pb(theme.ui(4.))
        .flex()
        .justify_between()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme.tokens.muted))
        .child(label.to_uppercase())
        .children(note)
}

/// A file row's height; the lists of files are virtualized, so every row has the same one.
pub(crate) const FILE_ROW_HEIGHT: f32 = 24.0;

/// One changed file: status letter, path, counts.
pub(crate) fn file_row(id: usize, file: &GitFile, selected: bool, theme: &Theme) -> Stateful<gpui::Div> {
    let counts = count_text(file);
    div()
        .id(("git-file", id))
        .w_full()
        .h(theme.ui(FILE_ROW_HEIGHT))
        .flex_none()
        .px(theme.ui(12.))
        .flex()
        .items_center()
        .gap(theme.ui(8.))
        .cursor_pointer()
        .text_xs()
        .when(selected, |row| row.bg(rgb(theme.tokens.raised)))
        .hover(|row| row.bg(rgb(theme.tokens.raised)))
        .child(
            div()
                .flex_none()
                .w(theme.ui(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(status_color(file.status, theme)))
                .child(status_letter(file.status).to_string()),
        )
        .child(div().flex_1().min_w_0().truncate().text_color(rgb(theme.tokens.text)).child(file.path.clone()))
        .child(div().flex_none().text_color(rgb(theme.tokens.muted)).child(counts))
}

/// One commit: subject, then author and age; unpushed ones carry an up arrow.
pub(crate) fn commit_row(id: usize, commit: &GitCommit, now: i64, theme: &Theme) -> Stateful<gpui::Div> {
    div()
        .id(("git-commit", id))
        .cursor_pointer()
        .hover(|row| row.bg(rgb(theme.tokens.raised)))
        .px(theme.ui(12.))
        .py(theme.ui(4.))
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .gap(theme.ui(6.))
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(theme.tokens.text))
                        .child(commit.subject.clone()),
                )
                .when(commit.unpushed, |row| {
                    row.child(div().flex_none().text_color(rgb(theme.tokens.accent)).child("↑"))
                }),
        )
        .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(commit_meta(commit, now)))
}

/// The commits heading: the branch's own commits are named for it, else they are the latest ones of `HEAD`.
pub(crate) fn commits_heading(state: &GitState) -> String {
    match (&state.branch, state.own_commits) {
        (Some(branch), true) => format!("Commits on {branch}"),
        (None, true) => "Commits on this checkout".to_string(),
        _ => "Recent commits".to_string(),
    }
}

/// The first line of a commit message.
pub(crate) fn subject(message: &str) -> String {
    message.lines().next().unwrap_or("").to_string()
}

/// The top of the git page's list while a commit is shown: hash, author and age, then the whole message. `failed` is
/// why the commit could not be read; with neither it is still loading.
pub(crate) fn commit_head(
    detail: Option<&GitCommitDetail>,
    failed: Option<&str>,
    now: i64,
    theme: &Theme,
) -> gpui::Div {
    let head = div().flex_none().flex().flex_col().gap(theme.ui(6.)).px(theme.ui(12.)).py(theme.ui(10.));
    let Some(detail) = detail else {
        let text = failed.map_or_else(|| "Loading…".to_string(), |reason| format!("git said: {reason}"));
        return head.text_sm().text_color(rgb(theme.tokens.muted)).child(text);
    };
    let meta =
        format!("{} · {} · {}", detail.hash.chars().take(7).collect::<String>(), detail.author, age(now, detail.time));
    head.child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(meta)).child(
        div()
            .id("git-commit-message")
            .max_h(theme.ui(180.))
            .overflow_y_scroll()
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(detail.message.clone()),
    )
}

/// A line of plain text for a panel with nothing to list.
pub(crate) fn message(text: String, theme: &Theme) -> gpui::Div {
    div().px(theme.ui(12.)).py(theme.ui(10.)).text_sm().text_color(rgb(theme.tokens.muted)).child(text)
}

/// The panel's container look: a quiet surface with a rule on its left.
pub(crate) fn frame(theme: &Theme) -> gpui::Div {
    div().bg(rgb(theme.tokens.surface)).border_l_1().border_color(rgb(theme.tokens.line)).rounded_l(px(RADIUS_SMALL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(status: GitFileStatus, added: Option<u32>, removed: Option<u32>) -> GitFile {
        GitFile { path: "p".into(), old_path: None, status, added, removed }
    }

    #[test]
    fn the_commits_heading_names_the_branch_when_they_are_its_own() {
        let mut state = GitState {
            root: "/r".into(),
            branch: Some("feat/x".into()),
            head: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Vec::new(),
            more: 0,
            commits: None,
            took_ms: 0,
            last_commit: None,
            filters_skipped: false,
            own_commits: true,
        };
        assert_eq!(commits_heading(&state), "Commits on feat/x");
        state.own_commits = false;
        assert_eq!(commits_heading(&state), "Recent commits");
    }

    #[test]
    fn subject_is_the_first_line() {
        assert_eq!(subject("fix: a\n\nbody"), "fix: a");
        assert_eq!(subject(""), "");
    }

    #[test]
    fn status_letters() {
        let letters: Vec<char> = [
            GitFileStatus::Modified,
            GitFileStatus::Added,
            GitFileStatus::Deleted,
            GitFileStatus::Renamed,
            GitFileStatus::Untracked,
            GitFileStatus::Conflicted,
        ]
        .into_iter()
        .map(status_letter)
        .collect();
        assert_eq!(letters, ['M', 'A', 'D', 'R', '?', 'U']);
    }

    #[test]
    fn count_text_shows_each_side_that_changed() {
        assert_eq!(count_text(&file(GitFileStatus::Modified, Some(28), Some(7))), "+28 −7");
        assert_eq!(count_text(&file(GitFileStatus::Modified, Some(13), Some(0))), "+13");
        assert_eq!(count_text(&file(GitFileStatus::Modified, Some(0), Some(2))), "−2");
        assert_eq!(count_text(&file(GitFileStatus::Untracked, None, None)), "new");
        assert_eq!(count_text(&file(GitFileStatus::Modified, None, None)), "");
        assert_eq!(count_text(&file(GitFileStatus::Renamed, Some(0), Some(0))), "");
    }

    #[test]
    fn commit_meta_names_author_and_age() {
        let commit = GitCommit {
            hash: "abc1234".into(),
            subject: "s".into(),
            author: "Ido".into(),
            time: 1_000_000 - 3600,
            unpushed: false,
        };
        assert_eq!(commit_meta(&commit, 1_000_000), "Ido · 1 h ago");
    }
}
