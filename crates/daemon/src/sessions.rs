//! The daemon's view of Claude sessions: the index's records plus which pane runs which session.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use claude_index::host::LocalHost;
use claude_index::index::{Index, IndexState, SessionRecord};
use claude_index::live::Activity;
use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus};

/// Full rescan interval (also catches processes that died without removing their pid file).
const RESCAN: Duration = Duration::from_secs(5);
/// Quiet time after a change notification before rescanning (writes come in bursts).
const DEBOUNCE: Duration = Duration::from_millis(200);

/// A live pane, as session matching sees it.
pub struct PaneSession {
    /// Pane id.
    pub pane: PaneId,
    /// Its process.
    pub pid: Option<u32>,
    /// Session its command line names (`claude --session-id X` / `--resume X`).
    pub session_arg: Option<String>,
}

/// A process the daemon saw exit (a pane's, or one it took over), identified by pid and start time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gone {
    /// Its pid.
    pub pid: u32,
    /// Its start time, as `/proc` reported it while it ran.
    pub start: Option<String>,
}

/// Whether the index still says `gone` runs (its pid file has not been re-checked yet).
pub fn still_claimed(gone: &Gone, records: &[SessionRecord]) -> bool {
    records.iter().any(|r| r.state == IndexState::Running { pid: gone.pid } && r.proc_start == gone.start)
}

/// Combine index records with panes. A running record whose process is a pane's process is managed by
/// that pane. The session a pane was started with is managed by it while that pane has no running
/// record of its own (Claude has not written its pid file yet); once it has one (e.g. after `/clear`),
/// the start session is closed. A record whose process is `gone` counts as closed: the index has not
/// noticed the exit yet.
pub fn resolve(records: &[SessionRecord], panes: &[PaneSession], gone: &[Gone]) -> Vec<SessionInfo> {
    let index_state = |r: &SessionRecord| match r.state {
        IndexState::Running { pid } if gone.iter().any(|g| g.pid == pid && g.start == r.proc_start) => IndexState::Closed,
        state => state,
    };
    let by_pid: HashMap<u32, PaneId> = panes.iter().filter_map(|p| Some((p.pid?, p.pane))).collect();
    let reporting: HashSet<PaneId> = records
        .iter()
        .filter_map(|r| match index_state(r) {
            IndexState::Running { pid } => by_pid.get(&pid).copied(),
            _ => None,
        })
        .collect();
    let by_arg: HashMap<&str, PaneId> = panes
        .iter()
        .filter(|p| !reporting.contains(&p.pane))
        .filter_map(|p| Some((p.session_arg.as_deref()?, p.pane)))
        .collect();
    records
        .iter()
        .map(|r| {
            let state = match index_state(r) {
                IndexState::Running { pid } => match by_pid.get(&pid) {
                    Some(pane) => SessionState::Managed { pane: *pane },
                    None => SessionState::External { pid: Some(pid) },
                },
                IndexState::InContainer { pid } => SessionState::External { pid },
                IndexState::Closed => match by_arg.get(r.id.as_str()) {
                    Some(pane) => SessionState::Managed { pane: *pane },
                    None => SessionState::Closed,
                },
            };
            // A gone process's last status is not the new pane's.
            let status = if index_state(r) == r.state { status(r, state) } else { None };
            SessionInfo {
                id: r.id.clone(),
                cwd: r.cwd.clone(),
                title: r.title.clone(),
                last_active_ms: r.last_active_ms,
                state,
                cwd_exists: r.cwd_exists,
                status_since_ms: if status.is_some() { r.activity_since_ms } else { 0 },
                status,
                container: None,
            }
        })
        .collect()
}

/// The status a session shows: what its process reports, while something runs it.
fn status(record: &SessionRecord, state: SessionState) -> Option<SessionStatus> {
    if state == SessionState::Closed {
        return None;
    }
    record.activity.as_ref().map(|activity| match activity {
        Activity::Busy => SessionStatus::Busy,
        Activity::Idle => SessionStatus::Idle,
        Activity::Waiting { reason } => SessionStatus::NeedsInput { reason: reason.clone() },
    })
}

/// What changed from `old` to `new`: sessions added or changed, and ids that disappeared.
pub fn diff(old: &[SessionInfo], new: &[SessionInfo]) -> (Vec<SessionInfo>, Vec<String>) {
    let before: HashMap<&str, &SessionInfo> = old.iter().map(|s| (s.id.as_str(), s)).collect();
    let after: HashSet<&str> = new.iter().map(|s| s.id.as_str()).collect();
    let upserted = new.iter().filter(|s| before.get(s.id.as_str()).copied() != Some(*s)).cloned().collect();
    let removed = old.iter().filter(|s| !after.contains(s.id.as_str())).map(|s| s.id.clone()).collect();
    (upserted, removed)
}

/// Index `config` on a thread of its own: rescan on change notifications (debounced) and every
/// `RESCAN`, publishing the first list and then each changed one to `tx`. Stops once `tx` has no receivers.
pub fn spawn_indexer(config: PathBuf, tx: tokio::sync::watch::Sender<Option<Arc<Vec<SessionRecord>>>>) {
    let spawned = std::thread::Builder::new().name("claude-index".into()).spawn(move || {
        let (wake_tx, wake) = std::sync::mpsc::channel();
        let _watcher = claude_index::watch::watch(&config, move || {
            let _ = wake_tx.send(());
        })
        .map_err(|err| tracing::warn!("watching {} failed: {err}", config.display()))
        .ok();
        let mut index = Index::new(config, LocalHost::new());
        loop {
            let records = index.scan();
            let changed = tx.borrow().as_deref().is_none_or(|old| *old != records);
            if changed && tx.send(Some(Arc::new(records))).is_err() {
                return;
            }
            if tx.is_closed() {
                return;
            }
            match wake.recv_timeout(RESCAN) {
                Ok(()) => {
                    std::thread::sleep(DEBOUNCE);
                    while wake.try_recv().is_ok() {}
                }
                Err(RecvTimeoutError::Timeout) => {}
                // No watcher: rescan on the timer only.
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(RESCAN),
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("could not start the session index: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, state: IndexState) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            cwd: "/r".into(),
            title: id.into(),
            last_active_ms: 1,
            state,
            cwd_exists: true,
            activity: None,
            activity_since_ms: 0,
            proc_start: None,
            container_id: None,
        }
    }

    fn pane(pane: PaneId, pid: u32, session_arg: Option<&str>) -> PaneSession {
        PaneSession { pane, pid: Some(pid), session_arg: session_arg.map(Into::into) }
    }

    fn states(sessions: &[SessionInfo]) -> Vec<(&str, SessionState)> {
        sessions.iter().map(|s| (s.id.as_str(), s.state)).collect()
    }

    #[test]
    fn a_pane_process_makes_its_session_managed_and_others_external() {
        let records = [
            record("mine", IndexState::Running { pid: 100 }),
            record("other", IndexState::Running { pid: 200 }),
            record("boxed", IndexState::InContainer { pid: None }),
            record("old", IndexState::Closed),
        ];
        let sessions = resolve(&records, &[pane(7, 100, None)], &[]);
        assert_eq!(states(&sessions), vec![
            ("mine", SessionState::Managed { pane: 7 }),
            ("other", SessionState::External { pid: Some(200) }),
            ("boxed", SessionState::External { pid: None }),
            ("old", SessionState::Closed),
        ]);
        assert_eq!((sessions[0].title.as_str(), sessions[0].cwd_exists), ("mine", true));
    }

    #[test]
    fn a_pane_is_managed_by_its_launch_session_until_its_process_reports() {
        let records = [record("launched", IndexState::Closed)];
        assert_eq!(states(&resolve(&records, &[pane(7, 100, Some("launched"))], &[])), vec![("launched", SessionState::Managed { pane: 7 })]);
    }

    #[test]
    fn after_clear_the_launch_session_is_closed() {
        let records = [record("launched", IndexState::Closed), record("after-clear", IndexState::Running { pid: 100 })];
        assert_eq!(states(&resolve(&records, &[pane(7, 100, Some("launched"))], &[])), vec![
            ("launched", SessionState::Closed),
            ("after-clear", SessionState::Managed { pane: 7 }),
        ]);
    }

    #[test]
    fn live_sessions_carry_their_status_and_closed_ones_none() {
        let mut busy = record("busy", IndexState::Running { pid: 100 });
        (busy.activity, busy.activity_since_ms) = (Some(Activity::Busy), 50);
        let mut asks = record("asks", IndexState::InContainer { pid: None });
        (asks.activity, asks.activity_since_ms) = (Some(Activity::Waiting { reason: Some("dialog open".into()) }), 60);
        let mut stale = record("old", IndexState::Closed);
        (stale.activity, stale.activity_since_ms) = (Some(Activity::Idle), 70);
        let sessions = resolve(&[busy, asks, stale], &[pane(7, 100, None)], &[]);
        let status: Vec<(&str, Option<SessionStatus>, u64)> =
            sessions.iter().map(|s| (s.id.as_str(), s.status.clone(), s.status_since_ms)).collect();
        assert_eq!(status, vec![
            ("busy", Some(SessionStatus::Busy), 50),
            ("asks", Some(SessionStatus::NeedsInput { reason: Some("dialog open".into()) }), 60),
            ("old", None, 0),
        ]);
    }

    #[test]
    fn a_process_seen_exiting_is_closed_until_the_index_catches_up() {
        let mut running = record("s", IndexState::Running { pid: 100 });
        running.proc_start = Some("5".into());
        let records = [running];
        let gone = |start: &str| vec![Gone { pid: 100, start: Some(start.into()) }];
        assert_eq!(states(&resolve(&records, &[], &gone("5"))), vec![("s", SessionState::Closed)]);
        // Same pid, other start time: a new process that reused the pid; not hidden.
        assert_eq!(states(&resolve(&records, &[], &gone("6"))), vec![("s", SessionState::External { pid: Some(100) })]);
        assert!(still_claimed(&gone("5")[0], &records));
        assert!(!still_claimed(&gone("6")[0], &records));
    }

    #[test]
    fn a_closed_by_exit_session_goes_to_a_pane_started_with_it() {
        let mut old = record("s", IndexState::Running { pid: 100 });
        old.proc_start = Some("5".into());
        let gone = [Gone { pid: 100, start: Some("5".into()) }];
        // After a take over, the new pane runs `claude --resume s` before Claude rewrites its pid file.
        let sessions = resolve(&[old], &[pane(9, 300, Some("s"))], &gone);
        assert_eq!(states(&sessions), vec![("s", SessionState::Managed { pane: 9 })]);
        assert_eq!(sessions[0].status, None);
    }

    #[test]
    fn diff_reports_new_changed_and_removed_sessions() {
        let infos = |records: &[SessionRecord]| resolve(records, &[], &[]);
        let old = infos(&[record("same", IndexState::Closed), record("changes", IndexState::Closed), record("goes", IndexState::Closed)]);
        let new = infos(&[record("same", IndexState::Closed), record("changes", IndexState::Running { pid: 5 }), record("arrives", IndexState::Closed)]);
        let (upserted, removed) = diff(&old, &new);
        assert_eq!(upserted.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["changes", "arrives"]);
        assert_eq!(removed, vec!["goes".to_string()]);
        assert_eq!(diff(&new, &new), (vec![], vec![]));
    }
}
