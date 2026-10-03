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
    Dir {
        /// The directory.
        path: PathBuf,
        /// Pinned in the settings (drawn with a star).
        favorite: bool,
        /// Whether it exists; a favorite whose folder is gone cannot be opened.
        exists: bool,
    },
    /// Pick a directory with the system dialog.
    Browse,
}

/// Favorites first, in settings order (filtered by the query), then existing session directories (each once, not a
/// favorite), by match score then recency, then `Browse`.
pub fn items(sessions: &[&SessionInfo], query: &str, home: Option<&Path>, favorites: &[PathBuf]) -> Vec<LauncherItem> {
    let mut latest: HashMap<&Path, u64> = HashMap::new();
    for session in sessions.iter().filter(|s| s.cwd_exists) {
        let last = latest.entry(session.cwd.as_path()).or_default();
        *last = (*last).max(session.last_active_ms);
    }
    let mut fuzzy = Fuzzy::new(query);
    let mut pinned: Vec<LauncherItem> = Vec::new();
    for path in favorites {
        if pinned.iter().any(|item| matches!(item, LauncherItem::Dir { path: seen, .. } if seen == path)) {
            continue;
        }
        if fuzzy.as_mut().is_some_and(|fuzzy| fuzzy.score(&display_path(path, home)).is_none()) {
            continue;
        }
        pinned.push(LauncherItem::Dir { path: path.clone(), favorite: true, exists: path.is_dir() });
    }
    let mut ranked: Vec<(&Path, u64, u32)> = latest
        .into_iter()
        .filter(|(dir, _)| !favorites.iter().any(|favorite| favorite == dir))
        .filter_map(|(dir, last)| {
            let score = match &mut fuzzy {
                None => 0,
                Some(fuzzy) => fuzzy.score(&display_path(dir, home))?,
            };
            Some((dir, last, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then_with(|| a.0.cmp(b.0)));
    let recent = ranked.into_iter().map(|(dir, ..)| LauncherItem::Dir {
        path: dir.to_path_buf(),
        favorite: false,
        exists: true,
    });
    pinned.into_iter().chain(recent).chain([LauncherItem::Browse]).collect()
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
            claude_dir: None,
        }
    }

    fn dirs(items: &[LauncherItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                LauncherItem::Dir { path, .. } => path.display().to_string(),
                LauncherItem::Browse => "Browse".into(),
            })
            .collect()
    }

    #[test]
    fn recent_directories_come_first_once_each() {
        let sessions = [info("/a", 10, true), info("/b", 30, true), info("/a", 50, true), info("/gone", 99, false)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        assert_eq!(dirs(&items(&refs, "", None, &[])), vec!["/a", "/b", "Browse"]);
    }

    #[test]
    fn a_query_filters_by_the_displayed_path() {
        let sessions = [info("/home/me/code/devops", 50, true), info("/home/me/Private/rust_terminal", 10, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let home = Some(Path::new("/home/me"));
        assert_eq!(dirs(&items(&refs, "rust", home, &[])), vec!["/home/me/Private/rust_terminal", "Browse"]);
        assert_eq!(dirs(&items(&refs, "~/code", home, &[])), vec!["/home/me/code/devops", "Browse"]);
        assert_eq!(dirs(&items(&refs, "zzz", home, &[])), vec!["Browse"]);
        assert_eq!(dirs(&items(&[], "", home, &[])), vec!["Browse"]);
    }

    fn favs(items: &[LauncherItem]) -> Vec<(String, bool, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                LauncherItem::Dir { path, favorite, exists } => Some((path.display().to_string(), *favorite, *exists)),
                LauncherItem::Browse => None,
            })
            .collect()
    }

    #[test]
    fn favorites_come_first_in_settings_order_without_duplicates() {
        let here = std::env::temp_dir();
        let sessions = [info("/a", 10, true), info("/b", 30, true), info(here.to_str().unwrap(), 5, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let favorites = [PathBuf::from("/b"), here.clone(), PathBuf::from("/b")];
        let list = items(&refs, "", None, &favorites);
        assert_eq!(
            favs(&list),
            vec![
                ("/b".to_string(), true, false),
                (here.display().to_string(), true, true),
                ("/a".to_string(), false, true)
            ]
        );
        assert_eq!(list.last(), Some(&LauncherItem::Browse));
    }

    #[test]
    fn favorites_are_filtered_by_the_query() {
        let sessions = [info("/home/me/code/devops", 50, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let home = Some(Path::new("/home/me"));
        let favorites = [PathBuf::from("/home/me/Private/rust_terminal"), PathBuf::from("/home/me/work/ops")];
        assert_eq!(
            favs(&items(&refs, "rust", home, &favorites)),
            vec![("/home/me/Private/rust_terminal".to_string(), true, false)]
        );
        assert_eq!(favs(&items(&refs, "", home, &favorites)).len(), 3);
        assert_eq!(favs(&items(&refs, "zzz", home, &favorites)), vec![]);
    }
}
