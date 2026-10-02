//! The GUI's copy of the daemon's session list.

use std::collections::HashMap;

use protocol::message::SessionInfo;

/// Sessions by id, kept in sync from `Sessions` / `SessionsChanged`.
#[derive(Default)]
pub struct SessionStore {
    sessions: HashMap<String, SessionInfo>,
    loaded: bool,
}

impl SessionStore {
    /// Replace everything with `sessions`.
    pub fn replace(&mut self, sessions: Vec<SessionInfo>) {
        self.sessions = sessions.into_iter().map(|s| (s.id.clone(), s)).collect();
        self.loaded = true;
    }

    /// Insert or update `upserted`, then drop `removed`.
    pub fn apply(&mut self, upserted: Vec<SessionInfo>, removed: &[String]) {
        for session in upserted {
            self.sessions.insert(session.id.clone(), session);
        }
        for id in removed {
            self.sessions.remove(id);
        }
    }

    /// Whether a full list has arrived (before that, an unknown id proves nothing).
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Session `id`.
    pub fn get(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.get(id)
    }

    /// Every session, in no particular order.
    pub fn all(&self) -> impl Iterator<Item = &SessionInfo> {
        self.sessions.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::SessionState;

    fn info(id: &str, title: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            cwd: "/".into(),
            title: title.into(),
            last_active_ms: 0,
            state: SessionState::Closed,
            cwd_exists: true,
            status: None,
            status_since_ms: 0,
            container: None, claude_dir: None,
        }
    }

    #[test]
    fn a_full_list_replaces_and_changes_apply() {
        let mut store = SessionStore::default();
        store.replace(vec![info("a", "A"), info("b", "B")]);
        store.replace(vec![info("b", "B"), info("c", "C")]);
        assert!(store.get("a").is_none());
        store.apply(vec![info("c", "C2"), info("d", "D")], &["b".to_string()]);
        let mut ids: Vec<&str> = store.all().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["c", "d"]);
        assert_eq!(store.get("c").unwrap().title, "C2");
    }

    #[test]
    fn the_store_is_loaded_once_a_full_list_arrives() {
        let mut store = SessionStore::default();
        store.apply(vec![info("a", "A")], &[]);
        assert!(!store.loaded());
        store.replace(Vec::new());
        assert!(store.loaded());
    }
}
