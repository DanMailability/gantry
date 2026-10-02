//! Attention: which status changes of Gantry's own sessions deserve the user's notice (spec §5.7).

use std::collections::HashMap;

use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus};

use crate::settings::{AlertSettings, AlertTone};

/// A turn counts as finished only after this much busy time unless the settings say otherwise.
const DEFAULT_FINISHED_AFTER_MS: u64 = 3_000;

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
pub struct AttentionTracker {
    seen: HashMap<String, Seen>,
    finished_after_ms: u64,
}

impl Default for AttentionTracker {
    fn default() -> Self {
        Self { seen: HashMap::new(), finished_after_ms: DEFAULT_FINISHED_AFTER_MS }
    }
}

struct Seen {
    status: Option<SessionStatus>,
    /// When the current busy run started (first Busy seen), while busy.
    busy_since_ms: Option<u64>,
}

impl AttentionTracker {
    /// Turns shorter than this are not "finished" (from `alerts.finishedAfterSeconds`); quicker turns and
    /// Busy↔Idle flapping are ignored.
    pub fn set_finished_after_ms(&mut self, ms: u64) {
        self.finished_after_ms = ms;
    }

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
            if let Some(alert) = alert(self.seen.get(&session.id), session, self.finished_after_ms) {
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

fn alert(before: Option<&Seen>, now: &SessionInfo, finished_after_ms: u64) -> Option<Alert> {
    let SessionState::Managed { pane } = now.state else { return None };
    let kind = match (before.and_then(|seen| seen.status.as_ref()), &now.status) {
        (Some(SessionStatus::NeedsInput { .. }), Some(SessionStatus::NeedsInput { .. })) => return None,
        (_, Some(SessionStatus::NeedsInput { reason })) => AlertKind::NeedsInput { reason: reason.clone() },
        (Some(SessionStatus::Busy), Some(SessionStatus::Idle)) => {
            let started = before.and_then(|seen| seen.busy_since_ms)?;
            if now.status_since_ms.saturating_sub(started) < finished_after_ms {
                return None;
            }
            AlertKind::Finished
        }
        _ => return None,
    };
    Some(Alert { session: now.id.clone(), pane, kind })
}

/// Which channels an alert uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Channels {
    /// Show the unseen badge.
    pub badge: bool,
    /// Raise a native notification.
    pub notification: bool,
    /// Play this sound.
    pub sound: Option<AlertTone>,
}

/// Decide the channels for `kind`. `visible`: the pane is in the visible tab of its window; `window_active`:
/// that window is the active one.
pub fn alert_channels(alerts: &AlertSettings, kind: &AlertKind, visible: bool, window_active: bool) -> Channels {
    if !alerts.when_visible && visible && window_active {
        return Channels::default();
    }
    let event = match kind {
        AlertKind::NeedsInput { .. } => &alerts.needs_input,
        AlertKind::Finished => &alerts.finished,
    };
    Channels {
        badge: event.badge,
        notification: event.notification && !(alerts.only_when_unfocused && window_active),
        sound: (event.sound && alerts.play_sounds).then_some(event.tone),
    }
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

/// `title` without the status glyph Claude puts in front of its terminal title (◐/◑ while busy, ✳ when idle);
/// Gantry shows its own status next to the title.
pub fn without_claude_glyph(title: &str) -> &str {
    title.strip_prefix(['◐', '◑', '✳']).map_or(title, str::trim_start)
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
    use crate::settings::EventAlerts;
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
            container: None, claude_dir: None,
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
    fn claudes_own_status_glyph_is_dropped_from_titles() {
        assert_eq!(without_claude_glyph("◐ Poem about terminals"), "Poem about terminals");
        assert_eq!(without_claude_glyph("◑ Poem"), "Poem");
        assert_eq!(without_claude_glyph("✳ Poem"), "Poem");
        assert_eq!(without_claude_glyph("Claude Code"), "Claude Code");
        assert_eq!(without_claude_glyph("◐"), "");
    }

    #[test]
    fn notification_text_names_the_session_and_why() {
        let needs = AlertKind::NeedsInput { reason: Some("dialog open".into()) };
        assert_eq!(describe(&needs, "Fix CI", "~/repo"), ("Needs input: Fix CI".into(), "dialog open · ~/repo".into()));
        assert_eq!(describe(&AlertKind::NeedsInput { reason: None }, "Fix CI", "~/repo").1, "~/repo");
        assert_eq!(describe(&AlertKind::Finished, "Fix CI", "~/repo"), ("Finished: Fix CI".into(), "~/repo".into()));
    }

    #[test]
    fn threshold_comes_from_settings() {
        let mut tracker = AttentionTracker::default();
        tracker.set_finished_after_ms(10_000);
        tracker.reset(&[mine(Some(SessionStatus::Idle), 0)]);
        tracker.update(&[mine(Some(SessionStatus::Busy), 10_000)], &[]);
        assert!(tracker.update(&[mine(Some(SessionStatus::Idle), 15_000)], &[]).is_empty());
        tracker.update(&[mine(Some(SessionStatus::Busy), 20_000)], &[]);
        assert_eq!(kinds(tracker.update(&[mine(Some(SessionStatus::Idle), 31_000)], &[])), vec![AlertKind::Finished]);
    }

    fn all_on() -> EventAlerts {
        EventAlerts { badge: true, notification: true, sound: true, tone: AlertTone::Beep }
    }

    fn settings() -> AlertSettings {
        AlertSettings {
            needs_input: all_on(),
            finished: all_on(),
            finished_after_seconds: 3,
            when_visible: false,
            only_when_unfocused: false,
            play_sounds: true,
            volume: 1.0,
        }
    }

    fn finished() -> AlertKind {
        AlertKind::Finished
    }

    #[test]
    fn visible_pane_is_quiet_by_default() {
        assert_eq!(alert_channels(&settings(), &finished(), true, true), Channels::default());
        // Visible tab of a window that is not active still alerts.
        assert!(alert_channels(&settings(), &finished(), true, false).badge);
    }

    #[test]
    fn when_visible_alerts_a_visible_pane() {
        let alerts = AlertSettings { when_visible: true, ..settings() };
        assert_eq!(alert_channels(&alerts, &finished(), true, true), Channels { badge: true, notification: true, sound: Some(AlertTone::Beep) });
    }

    #[test]
    fn only_when_unfocused_drops_notification_but_keeps_badge_and_sound() {
        let alerts = AlertSettings { only_when_unfocused: true, ..settings() };
        assert_eq!(alert_channels(&alerts, &finished(), false, true), Channels { badge: true, notification: false, sound: Some(AlertTone::Beep) });
        assert!(alert_channels(&alerts, &finished(), false, false).notification);
    }

    #[test]
    fn play_sounds_off_mutes_every_event() {
        let alerts = AlertSettings { play_sounds: false, ..settings() };
        assert_eq!(alert_channels(&alerts, &finished(), false, false).sound, None);
        assert_eq!(alert_channels(&alerts, &AlertKind::NeedsInput { reason: None }, false, false).sound, None);
    }

    #[test]
    fn grid_cells_are_independent() {
        let alerts = AlertSettings {
            needs_input: EventAlerts { badge: false, notification: false, sound: true, tone: AlertTone::Beep },
            finished: EventAlerts { badge: false, notification: true, sound: false, tone: AlertTone::Blip },
            ..settings()
        };
        assert_eq!(
            alert_channels(&alerts, &AlertKind::NeedsInput { reason: None }, false, false),
            Channels { badge: false, notification: false, sound: Some(AlertTone::Beep) }
        );
        assert_eq!(alert_channels(&alerts, &finished(), false, false), Channels { badge: false, notification: true, sound: None });
    }
}
