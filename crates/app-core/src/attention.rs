//! Attention: which status changes of ccm's own sessions deserve the user's notice (spec §5.7).

use std::collections::HashMap;

use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus};

/// A turn counts as finished only after this much busy time; quicker turns and Busy↔Idle flapping are ignored.
pub const FINISHED_MIN_BUSY_MS: u64 = 3_000;

/// Why a session wants attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// It is blocked on the user.
    NeedsInput {
        /// Claude's reason, if it gave one.
        reason: Option<String>,
    },
    /// It finished a turn.
    Finished,
}

/// One status change worth telling the user about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// Session id.
    pub session: String,
    /// The pane running it.
    pub pane: PaneId,
    /// What happened.
    pub kind: AlertKind,
}

/// The last status seen per session, to tell transitions from rewrites.
#[derive(Default)]
pub struct AttentionTracker {
    seen: HashMap<String, Seen>,
}

struct Seen {
    status: Option<SessionStatus>,
    /// When the current busy run started (first Busy seen), while busy.
    busy_since_ms: Option<u64>,
}

impl AttentionTracker {
    /// Start over from a full list. Nothing in it alerts: it is the baseline.
    pub fn reset(&mut self, sessions: &[SessionInfo]) {
        self.seen.clear();
        for session in sessions {
            self.observe(session);
        }
    }

    /// Apply a change and return the alerts it causes (managed sessions only).
    pub fn update(&mut self, upserted: &[SessionInfo], removed: &[String]) -> Vec<Alert> {
        for id in removed {
            self.seen.remove(id);
        }
        let mut alerts = Vec::new();
        for session in upserted {
            if let Some(alert) = alert(self.seen.get(&session.id), session) {
                alerts.push(alert);
            }
            self.observe(session);
        }
        alerts
    }

    fn observe(&mut self, session: &SessionInfo) {
        let still_busy = self.seen.get(&session.id).filter(|seen| seen.status == Some(SessionStatus::Busy));
        let busy_since_ms = match session.status {
            Some(SessionStatus::Busy) => Some(still_busy.and_then(|seen| seen.busy_since_ms).unwrap_or(session.status_since_ms)),
            _ => None,
        };
        self.seen.insert(session.id.clone(), Seen { status: session.status.clone(), busy_since_ms });
    }
}

fn alert(before: Option<&Seen>, now: &SessionInfo) -> Option<Alert> {
    let SessionState::Managed { pane } = now.state else { return None };
    let kind = match (before.and_then(|seen| seen.status.as_ref()), &now.status) {
        (Some(SessionStatus::NeedsInput { .. }), Some(SessionStatus::NeedsInput { .. })) => return None,
        (_, Some(SessionStatus::NeedsInput { reason })) => AlertKind::NeedsInput { reason: reason.clone() },
        (Some(SessionStatus::Busy), Some(SessionStatus::Idle)) => {
            let started = before.and_then(|seen| seen.busy_since_ms)?;
            if now.status_since_ms.saturating_sub(started) < FINISHED_MIN_BUSY_MS {
                return None;
            }
            AlertKind::Finished
        }
        _ => return None,
    };
    Some(Alert { session: now.id.clone(), pane, kind })
}

fn rank(status: &SessionStatus) -> u8 {
    match status {
        SessionStatus::NeedsInput { .. } => 3,
        SessionStatus::Busy => 2,
        SessionStatus::Idle => 1,
    }
}

/// The most urgent of `statuses`: NeedsInput > Busy > Idle.
pub fn most_urgent<'a>(statuses: impl IntoIterator<Item = &'a SessionStatus>) -> Option<&'a SessionStatus> {
    statuses.into_iter().max_by_key(|status| rank(status))
}

/// Notification summary and body for `kind` on the session titled `title` in `dir` (already shortened for display).
pub fn describe(kind: &AlertKind, title: &str, dir: &str) -> (String, String) {
    match kind {
        AlertKind::NeedsInput { reason: Some(reason) } => (format!("Needs input: {title}"), format!("{reason} · {dir}")),
        AlertKind::NeedsInput { reason: None } => (format!("Needs input: {title}"), dir.to_string()),
        AlertKind::Finished => (format!("Finished: {title}"), dir.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::SessionState;

    fn at(id: &str, state: SessionState, status: Option<SessionStatus>, since: u64) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            cwd: "/r".into(),
            title: String::new(),
            last_active_ms: 0,
            state,
            cwd_exists: true,
            status,
            status_since_ms: since,
        }
    }

    fn mine(status: Option<SessionStatus>, since: u64) -> SessionInfo {
        at("s", SessionState::Managed { pane: 7 }, status, since)
    }

    fn asks() -> Option<SessionStatus> {
        Some(SessionStatus::NeedsInput { reason: Some("dialog open".into()) })
    }

    fn kinds(alerts: Vec<Alert>) -> Vec<AlertKind> {
        alerts.into_iter().map(|alert| alert.kind).collect()
    }

    #[test]
    fn needs_input_alerts_once_per_episode() {
        let mut tracker = AttentionTracker::default();
        tracker.reset(&[mine(Some(SessionStatus::Busy), 1)]);
        let alerts = tracker.update(&[mine(asks(), 2)], &[]);
        assert_eq!(alerts, vec![Alert { session: "s".into(), pane: 7, kind: AlertKind::NeedsInput { reason: Some("dialog open".into()) } }]);
        // Claude rewrites the file while still waiting: no second alert.
        assert!(tracker.update(&[mine(asks(), 3)], &[]).is_empty());
        // Answered, then asks again: a new episode.
        tracker.update(&[mine(Some(SessionStatus::Busy), 4)], &[]);
        assert_eq!(kinds(tracker.update(&[mine(asks(), 5)], &[])).len(), 1);
    }

    #[test]
    fn finished_needs_a_busy_run_of_three_seconds() {
        let mut tracker = AttentionTracker::default();
        tracker.reset(&[mine(Some(SessionStatus::Idle), 0)]);
        tracker.update(&[mine(Some(SessionStatus::Busy), 10_000)], &[]);
        // Busy for 2.9 s: a quick turn, or flapping.
        assert!(tracker.update(&[mine(Some(SessionStatus::Idle), 12_900)], &[]).is_empty());
        tracker.update(&[mine(Some(SessionStatus::Busy), 20_000)], &[]);
        assert_eq!(kinds(tracker.update(&[mine(Some(SessionStatus::Idle), 23_000)], &[])), vec![AlertKind::Finished]);
    }

    #[test]
    fn busy_time_counts_from_the_start_of_the_run() {
        let mut tracker = AttentionTracker::default();
        tracker.reset(&[mine(Some(SessionStatus::Idle), 0)]);
        tracker.update(&[mine(Some(SessionStatus::Busy), 1_000)], &[]);
        // A rewrite while still busy moves `statusUpdatedAt`; the run still started at 1 000.
        tracker.update(&[mine(Some(SessionStatus::Busy), 3_500)], &[]);
        assert_eq!(kinds(tracker.update(&[mine(Some(SessionStatus::Idle), 4_200)], &[])), vec![AlertKind::Finished]);
    }

    #[test]
    fn the_initial_list_sets_a_baseline_without_alerts() {
        let mut tracker = AttentionTracker::default();
        tracker.reset(&[mine(asks(), 1)]);
        assert!(tracker.update(&[mine(asks(), 2)], &[]).is_empty());
        // A session first seen in a change (a new pane, or a new id after /clear) can alert right away.
        let fresh = at("new", SessionState::Managed { pane: 8 }, asks(), 3);
        assert_eq!(tracker.update(&[fresh], &[]).len(), 1);
    }

    #[test]
    fn sessions_outside_ccm_never_alert() {
        let mut tracker = AttentionTracker::default();
        let outside = |status, since| at("e", SessionState::External { pid: Some(9) }, status, since);
        tracker.reset(&[outside(Some(SessionStatus::Busy), 0)]);
        assert!(tracker.update(&[outside(asks(), 1)], &[]).is_empty());
        assert!(tracker.update(&[outside(Some(SessionStatus::Idle), 9_000)], &[]).is_empty());
    }

    #[test]
    fn removed_sessions_are_forgotten() {
        let mut tracker = AttentionTracker::default();
        tracker.reset(&[mine(asks(), 1)]);
        tracker.update(&[], &["s".to_string()]);
        // Back with the same status: no baseline any more, so it counts as new.
        assert_eq!(tracker.update(&[mine(asks(), 2)], &[]).len(), 1);
    }

    #[test]
    fn needs_input_outranks_busy_outranks_idle() {
        let (idle, busy, needs) = (SessionStatus::Idle, SessionStatus::Busy, SessionStatus::NeedsInput { reason: None });
        assert_eq!(most_urgent([&idle, &needs, &busy]), Some(&needs));
        assert_eq!(most_urgent([&idle, &busy]), Some(&busy));
        assert_eq!(most_urgent([]), None);
    }

    #[test]
    fn notification_text_names_the_session_and_why() {
        let needs = AlertKind::NeedsInput { reason: Some("dialog open".into()) };
        assert_eq!(describe(&needs, "Fix CI", "~/repo"), ("Needs input: Fix CI".into(), "dialog open · ~/repo".into()));
        assert_eq!(describe(&AlertKind::NeedsInput { reason: None }, "Fix CI", "~/repo").1, "~/repo");
        assert_eq!(describe(&AlertKind::Finished, "Fix CI", "~/repo"), ("Finished: Fix CI".into(), "~/repo".into()));
    }
}
