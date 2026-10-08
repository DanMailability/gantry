//! The directory launcher's list: directories from Claude history, most recently used first, fuzzy
//! filtered, open folders first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use protocol::host::{HostId, project_key, split_project_key};
use protocol::message::SessionInfo;

use crate::fuzzy::Fuzzy;
use crate::session_tree::{Tone, display_path};

/// A pane of the workspace, as the launcher sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPane {
    /// The host it runs on.
    pub host: HostId,
    /// Directory it was started in.
    pub cwd: PathBuf,
    /// It runs Claude Code (else a shell).
    pub claude: bool,
    /// Tone of its session; ignored for shells.
    pub tone: Tone,
}

/// A folder with live panes in the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFolder {
    /// The host it is on.
    pub host: HostId,
    /// The folder.
    pub path: PathBuf,
    /// The most urgent tone among its Claude panes; `None` when it has only shells.
    pub tone: Option<Tone>,
    /// Claude panes in it.
    pub claude_panes: usize,
    /// Shell panes in it.
    pub shell_panes: usize,
}

impl OpenFolder {
    /// The note on its row: "2 panes", "1 pane", "shell", "2 shells".
    pub fn note(&self) -> String {
        let total = self.claude_panes + self.shell_panes;
        match (self.claude_panes, total) {
            (0, 1) => "shell".to_string(),
            (0, count) => format!("{count} shells"),
            (_, 1) => "1 pane".to_string(),
            (_, count) => format!("{count} panes"),
        }
    }
}

fn urgency(tone: Tone) -> u8 {
    match tone {
        Tone::NeedsInput => 0,
        Tone::Working => 1,
        Tone::Done | Tone::Unread => 2,
        _ => 3,
    }
}

/// One entry per folder on a host, in the order they first appear in `panes`.
pub fn open_folders(panes: &[OpenPane]) -> Vec<OpenFolder> {
    let mut folders: Vec<OpenFolder> = Vec::new();
    for pane in panes {
        let at = match folders.iter().position(|folder| folder.host == pane.host && folder.path == pane.cwd) {
            Some(at) => at,
            None => {
                folders.push(OpenFolder {
                    host: pane.host.clone(),
                    path: pane.cwd.clone(),
                    tone: None,
                    claude_panes: 0,
                    shell_panes: 0,
                });
                folders.len() - 1
            }
        };
        let folder = &mut folders[at];
        if pane.claude {
            folder.claude_panes += 1;
            if folder.tone.is_none_or(|tone| urgency(pane.tone) < urgency(tone)) {
                folder.tone = Some(pane.tone);
            }
        } else {
            folder.shell_panes += 1;
        }
    }
    folders
}

/// One launcher entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherItem {
    /// A section heading; not selectable.
    Header {
        /// Its text.
        label: &'static str,
        /// A count shown at its right.
        count: Option<usize>,
    },
    /// A folder with panes open now.
    Open(OpenFolder),
    /// A directory to open.
    Dir {
        /// The directory.
        path: PathBuf,
        /// Pinned in the settings (drawn with a star).
        favorite: bool,
        /// Whether it exists; a favorite whose folder is gone cannot be opened.
        exists: bool,
    },
}

impl LauncherItem {
    /// Whether the highlight can rest on it.
    pub fn selectable(&self) -> bool {
        !matches!(self, LauncherItem::Header { .. })
    }

    /// The folder it opens, if it is one.
    pub fn folder(&self) -> Option<&PathBuf> {
        match self {
            LauncherItem::Open(folder) => Some(&folder.path),
            LauncherItem::Dir { path, .. } => Some(path),
            LauncherItem::Header { .. } => None,
        }
    }
}

/// The first entry the highlight can rest on (0 for an empty list).
pub fn first_selectable(items: &[LauncherItem]) -> usize {
    items.iter().position(LauncherItem::selectable).unwrap_or(0)
}

/// The next selectable entry below (`down`) or above `from`; `from` itself when there is none.
pub fn step(items: &[LauncherItem], from: usize, down: bool) -> usize {
    let found = if down {
        items.iter().enumerate().skip(from + 1).find(|(_, item)| item.selectable())
    } else {
        items.iter().enumerate().take(from).rev().find(|(_, item)| item.selectable())
    };
    found.map_or(from, |(index, _)| index)
}

/// The key a favorite on `host` is saved under in the settings: the folder itself for this machine,
/// `ssh://<host>/<folder>` otherwise.
pub fn favorite_key(host: &HostId, dir: &Path) -> PathBuf {
    project_key(host, dir)
}

/// The favorites that can be shown: those of this machine and of hosts in `known_hosts`.
pub fn visible_favorites(favorites: &[PathBuf], known_hosts: &[HostId]) -> Vec<PathBuf> {
    favorites
        .iter()
        .filter(|favorite| {
            let (host, _) = split_project_key(favorite);
            host.is_local() || known_hosts.contains(&host)
        })
        .cloned()
        .collect()
}

/// Open folders (under "Open now", filtered by the query, in the given order), then favorites (settings order,
/// filtered), then existing session directories (each once, by match score then recency), none of them an open
/// folder. The "Favorites and recent" header appears only when "Open now" is shown and other directories follow.
/// Only what belongs to `host` is listed: its open folders, its sessions' directories and the favorites saved for it
/// (see [`favorite_key`]); a remote host's folders are not checked on this machine.
pub fn items(
    open: &[OpenFolder],
    sessions: &[&SessionInfo],
    query: &str,
    home: Option<&Path>,
    favorites: &[PathBuf],
    host: &HostId,
) -> Vec<LauncherItem> {
    let remote = !host.is_local();
    let open: Vec<&OpenFolder> = open.iter().filter(|folder| folder.host == *host).collect();
    let sessions: Vec<&&SessionInfo> = sessions.iter().filter(|session| session.host == *host).collect();
    let favorites: Vec<PathBuf> = favorites
        .iter()
        .filter_map(|favorite| {
            let (favorite_host, path) = split_project_key(favorite);
            (favorite_host == *host).then_some(path)
        })
        .collect();
    let is_open = |path: &Path| open.iter().any(|folder| folder.path == path);
    let mut latest: HashMap<&Path, u64> = HashMap::new();
    for session in sessions.iter().filter(|s| s.cwd_exists) {
        let last = latest.entry(session.cwd.as_path()).or_default();
        *last = (*last).max(session.last_active_ms);
    }
    let texts = open
        .iter()
        .map(|folder| folder.path.as_path())
        .chain(favorites.iter().map(PathBuf::as_path))
        .chain(latest.keys().copied())
        .map(|path| display_path(path, home));
    let mut fuzzy = Fuzzy::new(query, texts);
    let open_items: Vec<LauncherItem> = open
        .iter()
        .filter(|folder| fuzzy.as_mut().is_none_or(|fuzzy| fuzzy.score(&display_path(&folder.path, home)).is_some()))
        .map(|folder| LauncherItem::Open((*folder).clone()))
        .collect();
    let mut pinned: Vec<LauncherItem> = Vec::new();
    for path in &favorites {
        if is_open(path) {
            continue;
        }
        if pinned.iter().any(|item| matches!(item, LauncherItem::Dir { path: seen, .. } if seen == path)) {
            continue;
        }
        if fuzzy.as_mut().is_some_and(|fuzzy| fuzzy.score(&display_path(path, home)).is_none()) {
            continue;
        }
        pinned.push(LauncherItem::Dir { path: path.clone(), favorite: true, exists: remote || path.is_dir() });
    }
    let mut ranked: Vec<(&Path, u64, u32)> = latest
        .into_iter()
        .filter(|(dir, _)| !is_open(dir))
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
    let others: Vec<LauncherItem> = pinned.into_iter().chain(recent).collect();
    let mut list = Vec::new();
    if !open_items.is_empty() {
        list.push(LauncherItem::Header { label: "Open now", count: Some(open_items.len()) });
        list.extend(open_items);
        if !others.is_empty() {
            list.push(LauncherItem::Header { label: "Favorites and recent", count: None });
        }
    }
    list.extend(others);
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_tree::Tone;
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
            last_prompt: None,
            host: HostId::local(),
        }
    }

    fn dirs(items: &[LauncherItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                LauncherItem::Header { label, .. } => format!("# {label}"),
                LauncherItem::Open(folder) => format!("open {}", folder.path.display()),
                LauncherItem::Dir { path, .. } => path.display().to_string(),
            })
            .collect()
    }

    #[test]
    fn recent_directories_come_first_once_each() {
        let sessions = [info("/a", 10, true), info("/b", 30, true), info("/a", 50, true), info("/gone", 99, false)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        assert_eq!(dirs(&items(&[], &refs, "", None, &[], &HostId::local())), vec!["/a", "/b"]);
    }

    // Unix-style paths: Windows joins them with `\`.
    #[cfg(unix)]
    #[test]
    fn a_query_filters_by_the_displayed_path() {
        let sessions = [info("/home/me/code/devops", 50, true), info("/home/me/Private/rust_terminal", 10, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let home = Some(Path::new("/home/me"));
        assert_eq!(
            dirs(&items(&[], &refs, "rust", home, &[], &HostId::local())),
            vec!["/home/me/Private/rust_terminal"]
        );
        assert_eq!(dirs(&items(&[], &refs, "~/code", home, &[], &HostId::local())), vec!["/home/me/code/devops"]);
        assert_eq!(dirs(&items(&[], &refs, "zzz", home, &[], &HostId::local())), Vec::<&str>::new());
        assert_eq!(dirs(&items(&[], &[], "", home, &[], &HostId::local())), Vec::<&str>::new());
    }

    fn favs(items: &[LauncherItem]) -> Vec<(String, bool, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                LauncherItem::Dir { path, favorite, exists } => Some((path.display().to_string(), *favorite, *exists)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn favorites_come_first_in_settings_order_without_duplicates() {
        let here = std::env::temp_dir();
        let sessions = [info("/a", 10, true), info("/b", 30, true), info(here.to_str().unwrap(), 5, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let favorites = [PathBuf::from("/b"), here.clone(), PathBuf::from("/b")];
        let list = items(&[], &refs, "", None, &favorites, &HostId::local());
        assert_eq!(
            favs(&list),
            vec![
                ("/b".to_string(), true, false),
                (here.display().to_string(), true, true),
                ("/a".to_string(), false, true)
            ]
        );
    }

    #[test]
    fn favorites_are_filtered_by_the_query() {
        let sessions = [info("/home/me/code/devops", 50, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let home = Some(Path::new("/home/me"));
        let favorites = [PathBuf::from("/home/me/Private/rust_terminal"), PathBuf::from("/home/me/work/ops")];
        assert_eq!(
            favs(&items(&[], &refs, "rust", home, &favorites, &HostId::local())),
            vec![("/home/me/Private/rust_terminal".to_string(), true, false)]
        );
        assert_eq!(favs(&items(&[], &refs, "", home, &favorites, &HostId::local())).len(), 3);
        assert_eq!(favs(&items(&[], &refs, "zzz", home, &favorites, &HostId::local())), vec![]);
    }

    fn open(path: &str, tone: Option<Tone>, claude: usize, shell: usize) -> OpenFolder {
        OpenFolder { host: HostId::local(), path: path.into(), tone, claude_panes: claude, shell_panes: shell }
    }

    fn pane(cwd: &str, claude: bool, tone: Tone) -> OpenPane {
        OpenPane { host: HostId::local(), cwd: cwd.into(), claude, tone }
    }

    #[test]
    fn open_folders_merge_panes_per_folder_in_first_seen_order() {
        let panes = [
            pane("/a", true, Tone::Idle),
            pane("/b", false, Tone::Idle),
            pane("/a", true, Tone::NeedsInput),
            pane("/a", false, Tone::Idle),
            pane("/c", true, Tone::Working),
            pane("/c", true, Tone::Done),
        ];
        assert_eq!(
            open_folders(&panes),
            vec![
                open("/a", Some(Tone::NeedsInput), 2, 1),
                open("/b", None, 0, 1),
                open("/c", Some(Tone::Working), 2, 0),
            ]
        );
    }

    #[test]
    fn open_folder_notes() {
        assert_eq!(open("/a", Some(Tone::Idle), 1, 0).note(), "1 pane");
        assert_eq!(open("/a", Some(Tone::Idle), 2, 1).note(), "3 panes");
        assert_eq!(open("/a", None, 0, 1).note(), "shell");
        assert_eq!(open("/a", None, 0, 2).note(), "2 shells");
    }

    #[test]
    fn open_folders_lead_under_a_header_and_are_not_repeated() {
        let sessions = [info("/a", 10, true), info("/b", 30, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let open = [open("/b", Some(Tone::Working), 1, 0)];
        let list = items(&open, &refs, "", None, &[PathBuf::from("/b")], &HostId::local());
        assert_eq!(dirs(&list), vec!["# Open now", "open /b", "# Favorites and recent", "/a"]);
        assert_eq!(list[0], LauncherItem::Header { label: "Open now", count: Some(1) });
    }

    #[test]
    fn headers_appear_only_for_sections_that_exist() {
        let open = [open("/b", None, 0, 1)];
        assert_eq!(dirs(&items(&open, &[], "", None, &[], &HostId::local())), vec!["# Open now", "open /b"]);
        let sessions = [info("/a", 10, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        assert_eq!(dirs(&items(&[], &refs, "", None, &[], &HostId::local())), vec!["/a"]);
        assert!(items(&[], &[], "", None, &[], &HostId::local()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_query_filters_open_folders_too() {
        let home = Some(Path::new("/home/me"));
        let open = [open("/home/me/code/devops", None, 0, 1), open("/home/me/Private/rust_terminal", None, 0, 1)];
        assert_eq!(
            dirs(&items(&open, &[], "rust", home, &[], &HostId::local())),
            vec!["# Open now", "open /home/me/Private/rust_terminal"]
        );
        assert!(items(&open, &[], "zzz", home, &[], &HostId::local()).is_empty());
    }

    #[test]
    fn selection_skips_headers() {
        let open = [open("/b", None, 0, 1)];
        let sessions = [info("/a", 10, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let list = items(&open, &refs, "", None, &[], &HostId::local()); // header, open, header, dir
        assert_eq!(first_selectable(&list), 1);
        assert_eq!(step(&list, 1, true), 3);
        assert_eq!(step(&list, 3, false), 1);
        assert_eq!(step(&list, 3, true), 3);
        assert_eq!(step(&list, 1, false), 1);
        assert_eq!(first_selectable(&[]), 0);
        assert_eq!(step(&[], 0, true), 0);
    }

    fn remote_info(cwd: &str, last_active_ms: u64) -> SessionInfo {
        SessionInfo { host: HostId::named("devbox"), ..info(cwd, last_active_ms, true) }
    }

    #[test]
    fn a_remote_launcher_lists_only_that_hosts_folders() {
        let sessions = [remote_info("/r/app", 10), info("/l/app", 20, true)];
        let refs: Vec<&SessionInfo> = sessions.iter().collect();
        let devbox = HostId::named("devbox");
        assert_eq!(dirs(&items(&[], &refs, "", None, &[], &devbox)), vec!["/r/app"]);
        assert_eq!(dirs(&items(&[], &refs, "", None, &[], &HostId::local())), vec!["/l/app"]);
        let open = [OpenFolder { host: devbox.clone(), ..open("/r/w", None, 0, 1) }, open("/l/w", None, 0, 1)];
        assert_eq!(dirs(&items(&open, &[], "", None, &[], &devbox)), vec!["# Open now", "open /r/w"]);
    }

    #[test]
    fn favorites_are_decoded_per_host() {
        let favorites = [PathBuf::from("/a"), PathBuf::from("ssh://devbox/work")];
        assert_eq!(
            favs(&items(&[], &[], "", None, &favorites, &HostId::local())),
            vec![("/a".to_string(), true, false)]
        );
        assert_eq!(
            favs(&items(&[], &[], "", None, &favorites, &HostId::named("devbox"))),
            vec![("/work".to_string(), true, true)]
        );
        assert_eq!(favorite_key(&HostId::named("devbox"), Path::new("/work")), PathBuf::from("ssh://devbox/work"));
        assert_eq!(favorite_key(&HostId::local(), Path::new("/a")), PathBuf::from("/a"));
    }

    #[test]
    fn favorites_of_removed_hosts_are_hidden() {
        let favorites = [PathBuf::from("ssh://gone/x"), PathBuf::from("/a"), PathBuf::from("ssh://devbox/y")];
        assert_eq!(
            visible_favorites(&favorites, &[HostId::named("devbox")]),
            vec![PathBuf::from("/a"), PathBuf::from("ssh://devbox/y")]
        );
    }

    #[test]
    fn open_folders_group_by_host() {
        let remote = OpenPane { host: HostId::named("devbox"), ..pane("/a", true, Tone::Idle) };
        let folders = open_folders(&[pane("/a", true, Tone::Idle), remote]);
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[1].host, HostId::named("devbox"));
    }
}
