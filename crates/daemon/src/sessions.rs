//! The daemon's view of Claude sessions: the index's records plus which pane runs which session.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use claude_index::host::LocalHost;
use claude_index::index::{Index, IndexState, SessionRecord};
use protocol::message::{PaneId, SessionInfo, SessionState};

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

/// Combine index records with panes. A running record whose process is a pane's process is managed by
/// that pane. The session a pane was started with is managed by it while that pane has no running
/// record of its own (Claude has not written its pid file yet); once it has one (e.g. after `/clear`),
/// the start session is closed.
pub fn resolve(records: &[SessionRecord], panes: &[PaneSession]) -> Vec<SessionInfo> {
    let by_pid: HashMap<u32, PaneId> = panes.iter().filter_map(|p| Some((p.pid?, p.pane))).collect();
    let reporting: HashSet<PaneId> = records
        .iter()
        .filter_map(|r| match r.state {
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
            let state = match r.state {
                IndexState::Running { pid } => match by_pid.get(&pid) {
                    Some(pane) => SessionState::Managed { pane: *pane },
                    None => SessionState::External { pid: Some(pid) },
                },
                IndexState::InContainer => SessionState::External { pid: None },
                IndexState::Closed => match by_arg.get(r.id.as_str()) {
                    Some(pane) => SessionState::Managed { pane: *pane },
                    None => SessionState::Closed,
                },
            };
            SessionInfo {
                id: r.id.clone(),
                cwd: r.cwd.clone(),
                title: r.title.clone(),
                last_active_ms: r.last_active_ms,
                state,
                cwd_exists: r.cwd_exists,
            }
        })
        .collect()
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
            record("boxed", IndexState::InContainer),
            record("old", IndexState::Closed),
        ];
        let sessions = resolve(&records, &[pane(7, 100, None)]);
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
        assert_eq!(states(&resolve(&records, &[pane(7, 100, Some("launched"))])), vec![("launched", SessionState::Managed { pane: 7 })]);
    }

    #[test]
    fn after_clear_the_launch_session_is_closed() {
        let records = [record("launched", IndexState::Closed), record("after-clear", IndexState::Running { pid: 100 })];
        assert_eq!(states(&resolve(&records, &[pane(7, 100, Some("launched"))])), vec![
            ("launched", SessionState::Closed),
            ("after-clear", SessionState::Managed { pane: 7 }),
        ]);
    }

    #[test]
    fn diff_reports_new_changed_and_removed_sessions() {
        let infos = |records: &[SessionRecord]| resolve(records, &[]);
        let old = infos(&[record("same", IndexState::Closed), record("changes", IndexState::Closed), record("goes", IndexState::Closed)]);
        let new = infos(&[record("same", IndexState::Closed), record("changes", IndexState::Running { pid: 5 }), record("arrives", IndexState::Closed)]);
        let (upserted, removed) = diff(&old, &new);
        assert_eq!(upserted.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["changes", "arrives"]);
        assert_eq!(removed, vec!["goes".to_string()]);
        assert_eq!(diff(&new, &new), (vec![], vec![]));
    }
}
