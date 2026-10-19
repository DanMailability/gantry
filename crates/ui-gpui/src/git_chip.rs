//! The git branch chip: in the title bar for the focused pane and in the header of each pane of a split tab, with a
//! card on hover.

use app_core::git_view::{Chip, chip as chip_of, counts};
use app_core::session_tree::Tone;
use gpui::{
    AnyView, App, Context, ElementId, FontWeight, IntoElement, Render, Stateful, Window, div, prelude::*, px, rgb, svg,
};
use protocol::message::GitState;

use crate::assets::BRANCH_ICON;
use crate::context_menu;
use crate::git_state::RepoView;
use crate::settings_store::SettingsStore;
use crate::theme::{FadeKind, RADIUS_SMALL, Theme, UI_FONT, fade_in, theme};

/// The chip's parts: the branch, `● <changed>` when something is changed, and `↑a ↓b` when out of step.
fn chip_parts(chip: &Chip) -> (String, Option<String>, Option<String>) {
    let dirty = chip.dirty.then(|| format!("● {}", chip.changed));
    let mut sync = Vec::new();
    if chip.ahead > 0 {
        sync.push(format!("↑{}", chip.ahead));
    }
    if chip.behind > 0 {
        sync.push(format!("↓{}", chip.behind));
    }
    (chip.branch.clone(), dirty, (!sync.is_empty()).then(|| sync.join(" ")))
}

/// The chip's text: the branch, then `● <changed>` when something is changed, then `↑a` and `↓b`.
#[cfg_attr(not(test), allow(dead_code))] // The chip draws the parts; this is what they read as.
pub(crate) fn chip_text(chip: &Chip) -> String {
    let (branch, dirty, sync) = chip_parts(chip);
    [Some(branch), dirty, sync].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// The hover card's label and value lines for `state`, `now` being seconds since the Unix epoch.
pub(crate) fn hover_lines(state: &GitState, now: i64) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let branch = match (&state.branch, &state.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(head)) => format!("detached at {head}"),
        (None, None) => "no commits yet".to_string(),
    };
    lines.push(("Branch".to_string(), branch));
    if let Some(upstream) = &state.upstream {
        lines.push(("Upstream".to_string(), upstream.clone()));
        lines.push(("Ahead / behind".to_string(), format!("{} / {}", state.ahead, state.behind)));
    }
    let counts = counts(state);
    let kinds = [
        ("Modified", counts.modified),
        ("Added", counts.added),
        ("Deleted", counts.deleted),
        ("Untracked", counts.untracked),
    ];
    if kinds.iter().all(|(_, count)| *count == 0) && state.more == 0 {
        lines.push(("Changes".to_string(), "clean".to_string()));
    }
    for (label, count) in kinds {
        if count > 0 {
            lines.push((label.to_string(), count.to_string()));
        }
    }
    if state.more > 0 {
        lines.push(("More files".to_string(), format!("+{}", state.more)));
    }
    if state.filters_skipped {
        lines.push(("Git filters".to_string(), "not run".to_string()));
    }
    let last = state.last_commit.map_or_else(|| "none yet".to_string(), |time| app_core::git_view::age(now, time));
    lines.push(("Last commit".to_string(), last));
    lines
}

/// The hover card of a chip.
struct GitCard {
    lines: Vec<(String, String)>,
}

impl Render for GitCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let card = div()
            .min_w(theme.ui(220.))
            .px(theme.ui(12.))
            .py(theme.ui(10.))
            .flex()
            .flex_col()
            .gap(theme.ui(2.))
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .shadow_lg()
            .font_family(UI_FONT)
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .children(self.lines.iter().map(|(label, value)| {
                div()
                    .flex()
                    .justify_between()
                    .gap(theme.ui(16.))
                    .child(div().text_color(rgb(theme.tokens.muted)).child(label.clone()))
                    .child(div().child(value.clone()))
            }));
        fade_in(card, "fade-git-card", FadeKind::Card, cx.global::<SettingsStore>().settings().animate_ui)
    }
}

/// What the card shows while a menu is open.
struct NoCard;

impl Render for NoCard {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A `.tooltip(…)` builder showing `lines`; nothing while a menu is open (GPUI's tooltips ignore occlusion).
fn tooltip(lines: Vec<(String, String)>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        if context_menu::any_open() {
            cx.new(|_| NoCard).into()
        } else {
            let lines = lines.clone();
            cx.new(|_| GitCard { lines }).into()
        }
    }
}

/// The chip of a repo git could not read: "git error", with git's own words in its card.
fn error_chip(id: impl Into<ElementId>, reason: &str, theme: &Theme) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .tooltip(tooltip(vec![("git said".to_string(), reason.to_string())]))
        .flex_none()
        .h(theme.ui(20.))
        .px(theme.ui(8.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SMALL))
        .border_1()
        .border_color(rgb(theme.tokens.danger))
        .text_xs()
        .font_weight(FontWeight::NORMAL)
        .text_color(rgb(theme.tokens.danger))
        .child("git error")
}

/// The chip for a repo, or nothing when it is not (yet) known to be a git repo; a repo git fails on gets a red one.
pub(crate) fn chip(
    id: impl Into<ElementId>,
    view: Option<&RepoView>,
    now: i64,
    theme: &Theme,
) -> Option<Stateful<gpui::Div>> {
    let state = match view {
        Some(RepoView::Repo(state)) => state,
        Some(RepoView::Failed(reason)) => return Some(error_chip(id, reason, theme)),
        _ => return None,
    };
    let (branch, dirty, sync) = chip_parts(&chip_of(state));
    Some(
        div()
            .id(id)
            .tooltip(tooltip(hover_lines(state, now)))
            .flex_none()
            .h(theme.ui(20.))
            .px(theme.ui(8.))
            .flex()
            .items_center()
            .gap(theme.ui(5.))
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .text_xs()
            .font_weight(FontWeight::NORMAL)
            .text_color(rgb(theme.tokens.muted))
            .child(svg().path(BRANCH_ICON).flex_none().size(theme.ui(12.)).text_color(rgb(theme.tokens.accent)))
            .child(div().max_w(theme.ui(160.)).truncate().text_color(rgb(theme.tokens.text)).child(branch))
            .children(dirty.map(|dirty| div().text_color(rgb(theme.tone(Tone::NeedsInput).dot)).child(dirty)))
            .children(sync.map(|sync| div().child(sync))),
    )
}

#[cfg(test)]
mod tests {
    use app_core::git_view::chip;
    use protocol::message::{GitFile, GitFileStatus};

    use super::*;

    fn state() -> GitState {
        GitState {
            root: "/r".into(),
            branch: Some("fix/x".into()),
            head: Some("abc1234".into()),
            upstream: Some("origin/fix/x".into()),
            ahead: 2,
            behind: 0,
            files: Vec::new(),
            more: 0,
            commits: None,
            took_ms: 0,
            last_commit: Some(1_000_000 - 12 * 60),
            filters_skipped: false,
            own_commits: false,
        }
    }

    fn file(status: GitFileStatus) -> GitFile {
        GitFile { path: "p".into(), old_path: None, status, added: None, removed: None }
    }

    fn value<'a>(lines: &'a [(String, String)], label: &str) -> Option<&'a str> {
        lines.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str())
    }

    #[test]
    fn chip_text_matches_state() {
        let mut s = state();
        s.ahead = 0;
        assert_eq!(chip_text(&chip(&s)), "fix/x");
        s.files = vec![file(GitFileStatus::Modified); 3];
        assert_eq!(chip_text(&chip(&s)), "fix/x ● 3");
        s.ahead = 2;
        s.behind = 1;
        assert_eq!(chip_text(&chip(&s)), "fix/x ● 3 ↑2 ↓1");
        s.files.clear();
        s.behind = 0;
        assert_eq!(chip_text(&chip(&s)), "fix/x ↑2");
    }

    #[test]
    fn hover_lines_list_counts_and_last_commit() {
        let mut s = state();
        s.files = vec![file(GitFileStatus::Modified), file(GitFileStatus::Modified), file(GitFileStatus::Untracked)];
        let lines = hover_lines(&s, 1_000_000);
        assert_eq!(value(&lines, "Branch"), Some("fix/x"));
        assert_eq!(value(&lines, "Upstream"), Some("origin/fix/x"));
        assert_eq!(value(&lines, "Ahead / behind"), Some("2 / 0"));
        assert_eq!(value(&lines, "Modified"), Some("2"));
        assert_eq!(value(&lines, "Untracked"), Some("1"));
        assert_eq!(value(&lines, "Added"), None);
        assert_eq!(value(&lines, "Last commit"), Some("12 min ago"));
    }

    #[test]
    fn hover_lines_for_clean_detached_and_commitless_repos() {
        let mut s = state();
        s.branch = None;
        s.upstream = None;
        let lines = hover_lines(&s, 1_000_000);
        assert_eq!(value(&lines, "Branch"), Some("detached at abc1234"));
        assert_eq!(value(&lines, "Changes"), Some("clean"));
        assert_eq!(value(&lines, "Upstream"), None);
        s.head = None;
        s.last_commit = None;
        let lines = hover_lines(&s, 1_000_000);
        assert_eq!(value(&lines, "Last commit"), Some("none yet"));
    }
}
