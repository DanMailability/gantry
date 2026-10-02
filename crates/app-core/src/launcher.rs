//! The directory launcher's list: directories from Claude history, most recently used first, fuzzy
//! filtered, always ending with "Browse…".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use protocol::message::SessionInfo;

use crate::fuzzy::Fuzzy;
use crate::session_tree::display_path;

/// One launcher entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherItem {
    /// A directory to open.
    Dir(PathBuf),
    /// Pick a directory with the system dialog.
    Browse,
}

/// Existing session directories (each once), by match score then recency, then `Browse`.
pub fn items(sessions: &[&SessionInfo], query: &str, home: Option<&Path>) -> Vec<LauncherItem> {
    let mut latest: HashMap<&Path, u64> = HashMap::new();
    for session in sessions.iter().filter(|s| s.cwd_exists) {
        let last = latest.entry(session.cwd.as_path()).or_default();
        *last = (*last).max(session.last_active_ms);
    }
    let mut fuzzy = Fuzzy::new(query);
    let mut ranked: Vec<(&Path, u64, u32)> = latest
        .into_iter()
        .filter_map(|(dir, last)| {
            let score = match &mut fuzzy {
                None => 0,
                Some(fuzzy) => fuzzy.score(&display_path(dir, home))?,
            };
            Some((dir, last, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then_with(|| a.0.cmp(b.0)));
    ranked.into_iter().map(|(dir, ..)| LauncherItem::Dir(dir.to_path_buf())).chain([LauncherItem::Browse]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::SessionState;

    fn info(cwd: &str, last_active_ms: u64, cwd_exists: bool) -> SessionInfo {
        SessionInfo {
            id: format!("{cwd}{last_active_ms}"),
            cwd: cwd.into(),
            title: String::new(),
            last_active_ms,
            state: SessionState::Closed,
            cwd_exists,
            status: None,
            status_since_ms: 0,
            container: None,
        }
    }

    fn dirs(items: &[LauncherItem]) -> Vec<String> {
        items.iter().map(|item| match item {
            LauncherItem::Dir(dir) => dir.display().to_string(),
            LauncherItem::Browse => "Browse".into(),
        }).collect()
    }

    #[test]
    fn recent_directories_come_first_once_each() {
        let sessions = [info("/a", 10, true), info("/b", 30, true), info("/a", 50, true), info("/gone", 99, false)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        assert_eq!(dirs(&items(&refs, "", None)), vec!["/a", "/b", "Browse"]);
    }

    #[test]
    fn a_query_filters_by_the_displayed_path() {
        let sessions = [info("/home/me/code/devops", 50, true), info("/home/me/Private/rust_terminal", 10, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let home = Some(Path::new("/home/me"));
        assert_eq!(dirs(&items(&refs, "rust", home)), vec!["/home/me/Private/rust_terminal", "Browse"]);
        assert_eq!(dirs(&items(&refs, "~/code", home)), vec!["/home/me/code/devops", "Browse"]);
        assert_eq!(dirs(&items(&refs, "zzz", home)), vec!["Browse"]);
        assert_eq!(dirs(&items(&[], "", home)), vec!["Browse"]);
    }
}
