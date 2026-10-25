//! Best-effort reading of a Claude Code session's status line and background-task list from the pane's visible
//! terminal screen (the grid the GUI already holds, for local and SSH panes).
//!
//! IMPORTANT: Claude's status line / footer is undocumented and version-specific, so this is deliberately
//! conservative. Every field is optional and only set on a confident match; the UI shows a chip only when a value
//! parses, and hides it otherwise, so a wrong value is never shown. Per-task live OUTPUT is not present on the
//! status line (it lives in Claude's own task view), so it is never fabricated here — the UI offers "Open terminal".
//!
//! The exact patterns matched below are a first pass; they are unit-tested against representative samples and are
//! meant to be tightened against a real captured status line.

use std::sync::LazyLock;

use regex::Regex;

/// The read-only status chips parsed from the footer. Each is `None` until confidently parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusLine {
    /// Permission mode label, e.g. "Plan", "Accept edits", "Bypass permissions".
    pub mode: Option<String>,
    /// Model label, e.g. "Opus", "Sonnet", "Haiku".
    pub model: Option<String>,
    /// Effort/reasoning label, e.g. "high".
    pub effort: Option<String>,
    /// Context remaining, e.g. "23% context".
    pub context: Option<String>,
}

impl StatusLine {
    /// Whether anything parsed (so the toolbar shows a chip group at all).
    pub fn any(&self) -> bool {
        self.mode.is_some() || self.model.is_some() || self.effort.is_some() || self.context.is_some()
    }
}

/// One background task/subagent the session is running, as read off the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundTask {
    /// A short name/description of the task.
    pub name: String,
    /// Whether it is still running (vs. finished/failed) — defaults to running when the screen doesn't say.
    pub running: bool,
    /// Elapsed/age label if the screen prints one (e.g. "1m 20s"); `None` otherwise (never guessed).
    pub elapsed: Option<String>,
}

static CONTEXT: LazyLock<Regex> =
    // "... context ... 23% ..." or "23% context", in either order on one line.
    LazyLock::new(|| Regex::new(r"(?i)(?:context[^0-9]{0,24}(\d{1,3})%|(\d{1,3})%[^0-9a-z]{0,8}context)").unwrap());

static MODE: LazyLock<Regex> =
    // "⏵⏵ auto mode on", "plan mode on" — capture the mode word before "mode on".
    LazyLock::new(|| Regex::new(r"(?i)([a-z][a-z-]*)\s+mode\s+on").unwrap());

static MODEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(opus|sonnet|haiku)\b").unwrap());

static TASK_COUNT: LazyLock<Regex> =
    // "2 background tasks", "1 task running", "3 running tasks".
    LazyLock::new(|| Regex::new(r"(?i)(\d+)\s+(?:background\s+task|task(?:s)?\s+running|running\s+task)").unwrap());

static ELAPSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+h\s*)?(\d+m\s*)?\d+s|\d+:\d{2}(?::\d{2})?").unwrap());

/// Strip box-drawing and surrounding whitespace from a screen line.
fn clean(line: &str) -> String {
    const BORDER: &str = "│┃┆┇┊┋╎╏║╭╮╰╯─━┌┐└┘├┤┬┴┼▌▐";
    line.trim_matches(|c: char| c.is_whitespace() || BORDER.contains(c)).to_string()
}

/// Title-case a single lowercase word.
fn title(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Parse the status chips from the visible `lines`. Conservative: unmatched fields stay `None`.
pub fn parse_status(lines: &[String]) -> StatusLine {
    let mut status = StatusLine::default();
    for raw in lines {
        let line = clean(raw);
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if status.mode.is_none() {
            status.mode = if let Some(caps) = MODE.captures(&lower) {
                // "<word> mode on" — e.g. auto, plan, normal.
                caps.get(1).map(|m| title(m.as_str()))
            } else if lower.contains("accept edits") {
                Some("Accept edits".to_string())
            } else if lower.contains("bypass permissions") || lower.contains("bypassing permissions") {
                Some("Bypass permissions".to_string())
            } else {
                None
            };
        }
        if status.context.is_none()
            && let Some(caps) = CONTEXT.captures(&line)
        {
            let pct = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str());
            if let Some(pct) = pct {
                status.context = Some(format!("{pct}% context"));
            }
        }
        // Model only from a line that also looks like a status/footer (mentions "model") to avoid matching prose.
        if status.model.is_none()
            && lower.contains("model")
            && let Some(m) = MODEL.find(&lower)
        {
            status.model = Some(title(m.as_str()));
        }
        if status.effort.is_none()
            && let Some(rest) = lower.split_once("effort")
            && let Some(level) = rest.1.split_whitespace().next()
            && ["low", "medium", "high", "max", "none"].contains(&level)
        {
            status.effort = Some(title(level));
        }
    }
    status
}

/// Parse the background-task list from the visible `lines`. Returns the tasks when the screen clearly shows them,
/// else an empty list (so the UI hides the chip rather than guessing).
pub fn parse_tasks(lines: &[String]) -> Vec<BackgroundTask> {
    let cleaned: Vec<String> = lines.iter().map(|l| clean(l)).collect();

    // Find a header like "2 background tasks" / "1 task running" and, if present, take the following task-ish lines.
    let header = cleaned.iter().position(|line| TASK_COUNT.is_match(line));
    let Some(header) = header else { return Vec::new() };
    let count: usize =
        TASK_COUNT.captures(&cleaned[header]).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
    if count == 0 {
        return Vec::new();
    }

    let mut tasks = Vec::new();
    for line in cleaned.iter().skip(header + 1) {
        if line.is_empty() {
            if tasks.is_empty() {
                continue;
            }
            break;
        }
        // Strip a leading bullet/marker.
        let body = line.trim_start_matches(['•', '-', '*', '○', '●', '◦', '⎿', '└', '⏺', '↳', ' ']).trim();
        if body.is_empty() {
            continue;
        }
        let lower = body.to_lowercase();
        let running = !(lower.contains("done")
            || lower.contains("finished")
            || lower.contains("failed")
            || lower.contains("exited"));
        let elapsed = ELAPSED.find(body).map(|m| m.as_str().trim().to_string());
        // The name is the body with any trailing "(running)"/status and elapsed trimmed off.
        let name = body.split(['(', '·', '—']).next().unwrap_or(body).trim().to_string();
        if !name.is_empty() {
            tasks.push(BackgroundTask { name, running, elapsed });
        }
        if tasks.len() >= count {
            break;
        }
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_the_real_auto_mode_footer() {
        let real = "⏵⏵ auto mode on (shift+tab to cycle) · 1 feedback draft";
        let s = parse_status(&[real.to_string()]);
        assert_eq!(s.mode.as_deref(), Some("Auto"));
        // This version's footer shows no model/effort/context, so those chips stay hidden.
        assert_eq!((s.model, s.effort, s.context), (None, None, None));
        // "1 feedback draft" is not a background task.
        assert!(parse_tasks(&[real.to_string()]).is_empty());
    }

    #[test]
    fn parses_mode_and_context_from_a_footer() {
        let s = parse_status(&lines("⏵⏵ accept edits on (shift+tab to cycle)\nContext left: 23%"));
        assert_eq!(s.mode.as_deref(), Some("Accept edits"));
        assert_eq!(s.context.as_deref(), Some("23% context"));
    }

    #[test]
    fn parses_plan_mode_and_percent_before_context() {
        let s = parse_status(&lines("│ plan mode on │\n│ 7% context used │"));
        assert_eq!(s.mode.as_deref(), Some("Plan"));
        assert_eq!(s.context.as_deref(), Some("7% context"));
    }

    #[test]
    fn model_only_matches_a_model_line() {
        assert_eq!(parse_status(&lines("Model: Claude Sonnet 4.5")).model.as_deref(), Some("Sonnet"));
        // A model word in ordinary prose (no "model" on the line) is not matched.
        assert_eq!(parse_status(&lines("the opus was beautiful")).model, None);
    }

    #[test]
    fn effort_matches_a_level() {
        assert_eq!(parse_status(&lines("effort high")).effort.as_deref(), Some("High"));
        assert_eq!(parse_status(&lines("great effort today")).effort, None);
    }

    #[test]
    fn unparseable_footer_yields_nothing() {
        let s = parse_status(&lines("just some conversation text\nnothing status-like here"));
        assert!(!s.any());
    }

    #[test]
    fn parses_a_background_task_list_under_a_count_header() {
        let screen = lines(
            "2 background tasks\n\
             • npm run build (running) 1m 20s\n\
             • pytest · running 45s\n\
             (other screen content)",
        );
        let tasks = parse_tasks(&screen);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].name, "npm run build");
        assert!(tasks[0].running);
        assert_eq!(tasks[0].elapsed.as_deref(), Some("1m 20s"));
        assert_eq!(tasks[1].name, "pytest");
    }

    #[test]
    fn no_task_header_means_no_tasks() {
        assert!(parse_tasks(&lines("1. not a task\n2. also not")).is_empty());
        assert!(parse_tasks(&[]).is_empty());
    }
}
