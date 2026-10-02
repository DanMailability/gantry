//! Right-click menus as plain items: what a tab, a sidebar row and a project heading offer, and the VS Code
//! command line for a folder (inside its dev container when it has one).

use std::path::{Path, PathBuf};

use protocol::message::{ContainerPlace, SessionState};

use crate::session_tree::{Row, RowKind, Tone};
use crate::split_tree::{Axis, NodeId};

/// A shortcut whose hint a menu entry shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// Split the tab right.
    SplitRight,
    /// Split the tab down.
    SplitDown,
    /// Close the pane or tab.
    Close,
    /// Recent sessions pop-up.
    Recents,
}

/// What picking a menu entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Split tab `tab` along `axis`.
    Split {
        /// The tab index.
        tab: usize,
        /// The split direction.
        axis: Axis,
    },
    /// Copy text to the clipboard.
    Copy(String),
    /// Show a folder in the system file manager.
    OpenFolder(PathBuf),
    /// Run `code` with these arguments.
    OpenInVsCode(Vec<String>),
    /// Close one pane.
    ClosePane(NodeId),
    /// Close every pane of a tab.
    CloseTab(usize),
    /// Start a new Claude session or shell in a folder.
    NewHere {
        /// The folder.
        dir: PathBuf,
        /// Claude (else a shell).
        claude: bool,
    },
    /// Open the recent sessions pop-up on a project.
    RecentsFor(PathBuf),
    /// Continue the session of an exited pane.
    Continue(NodeId),
    /// Take over a session running outside Gantry.
    TakeOver {
        /// Session id.
        session: String,
        /// Its directory.
        cwd: PathBuf,
    },
}

/// One pickable menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    /// Its text.
    pub label: &'static str,
    /// The shortcut hint shown beside it.
    pub shortcut: Option<Shortcut>,
    /// Drawn in the accent colour (the row's main action).
    pub accent: bool,
    /// What picking it does.
    pub action: MenuAction,
}

/// A menu line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// A pickable entry.
    Entry(MenuEntry),
    /// A line between groups.
    Separator,
}

fn entry(label: &'static str, action: MenuAction) -> MenuItem {
    MenuItem::Entry(MenuEntry { label, shortcut: None, accent: false, action })
}

fn with_shortcut(label: &'static str, shortcut: Shortcut, action: MenuAction) -> MenuItem {
    MenuItem::Entry(MenuEntry { label, shortcut: Some(shortcut), accent: false, action })
}

fn accented(label: &'static str, action: MenuAction) -> MenuItem {
    MenuItem::Entry(MenuEntry { label, shortcut: None, accent: true, action })
}

/// Copy folder path · Open folder · Open in VS Code.
fn folder_items(dir: &Path, vscode: Vec<String>) -> [MenuItem; 3] {
    [
        entry("Copy folder path", MenuAction::Copy(dir.display().to_string())),
        entry("Open folder", MenuAction::OpenFolder(dir.to_path_buf())),
        entry("Open in VS Code", MenuAction::OpenInVsCode(vscode)),
    ]
}

/// The arguments for `code`: inside the dev container when there is one (the session's own path there, else VS Code's
/// default `/workspaces/<name>`), else the folder.
pub fn vscode_args(folder: &Path, container: Option<&ContainerPlace>, pane_container: Option<&Path>) -> Vec<String> {
    let target = match (container, pane_container) {
        (Some(ContainerPlace { host_folder: Some(host), path }), _) => Some((host.as_path(), path.clone())),
        (_, Some(host)) => {
            let name = host.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            Some((host, PathBuf::from("/workspaces").join(name)))
        }
        _ => None,
    };
    match target {
        Some((host, path)) => {
            let hex: String = host.to_string_lossy().bytes().map(|byte| format!("{byte:02x}")).collect();
            vec!["--folder-uri".into(), format!("vscode-remote://dev-container+{hex}{}", path.display())]
        }
        None => vec![folder.display().to_string()],
    }
}

/// A tab's menu; `dir` is its focused pane's directory.
pub fn tab_menu(tab: usize, dir: &Path, vscode: Vec<String>) -> Vec<MenuItem> {
    let mut items = vec![
        with_shortcut("Split right", Shortcut::SplitRight, MenuAction::Split { tab, axis: Axis::Horizontal }),
        with_shortcut("Split down", Shortcut::SplitDown, MenuAction::Split { tab, axis: Axis::Vertical }),
        MenuItem::Separator,
    ];
    items.extend(folder_items(dir, vscode));
    items.push(MenuItem::Separator);
    items.push(with_shortcut("Close tab", Shortcut::Close, MenuAction::CloseTab(tab)));
    items
}

fn close_item(leaf: NodeId, alone: &dyn Fn(NodeId) -> bool) -> MenuItem {
    let label = if alone(leaf) { "Close tab" } else { "Close pane" };
    with_shortcut(label, Shortcut::Close, MenuAction::ClosePane(leaf))
}

/// A sidebar row's menu; `None` for rows without one (headings use [`project_menu`], details and "Take over…" rows
/// have none). `alone(leaf)` = that leaf's tab has no other pane.
pub fn row_menu(row: &Row, alone: &dyn Fn(NodeId) -> bool) -> Option<Vec<MenuItem>> {
    let mut items = Vec::new();
    match &row.kind {
        RowKind::Session { session, tone, leaf, .. } => {
            let vscode = vscode_args(&session.cwd, session.container.as_ref(), None);
            match (session.state, leaf) {
                (SessionState::External { pid }, _) => {
                    if pid.is_some() && session.cwd_exists {
                        items.push(accented("Take over…", MenuAction::TakeOver { session: session.id.clone(), cwd: session.cwd.clone() }));
                        items.push(MenuItem::Separator);
                    }
                }
                (SessionState::Closed, Some(leaf)) if *tone == Tone::Closed => {
                    items.push(accented("Continue", MenuAction::Continue(*leaf)));
                    items.push(MenuItem::Separator);
                }
                _ => {
                    items.push(entry("New Claude here", MenuAction::NewHere { dir: session.cwd.clone(), claude: true }));
                    items.push(MenuItem::Separator);
                }
            }
            items.push(entry("Copy session id", MenuAction::Copy(session.id.clone())));
            items.extend(folder_items(&session.cwd, vscode));
            if let Some(leaf) = leaf {
                items.push(MenuItem::Separator);
                items.push(close_item(*leaf, alone));
            }
        }
        RowKind::Pane { entry: pane, .. } => {
            let vscode = vscode_args(&pane.cwd, None, pane.container.as_deref());
            match (pane.claude, pane.ended) {
                (true, true) => items.push(accented("Continue", MenuAction::Continue(pane.leaf))),
                (claude, _) => {
                    let label = if claude { "New Claude here" } else { "New shell here" };
                    items.push(entry(label, MenuAction::NewHere { dir: pane.cwd.clone(), claude }));
                }
            }
            items.push(MenuItem::Separator);
            if let Some(id) = &pane.session {
                items.push(entry("Copy session id", MenuAction::Copy(id.clone())));
            }
            items.extend(folder_items(&pane.cwd, vscode));
            items.push(MenuItem::Separator);
            items.push(close_item(pane.leaf, alone));
        }
        RowKind::Project { .. } | RowKind::Tab { .. } | RowKind::Detail { .. } | RowKind::TakeOver { .. } => return None,
    }
    Some(items)
}

/// A project heading's menu; `container` = the dev container one of its sessions runs in, if any.
pub fn project_menu(path: &Path, container: Option<&ContainerPlace>) -> Vec<MenuItem> {
    let mut items = vec![
        entry("New Claude here", MenuAction::NewHere { dir: path.to_path_buf(), claude: true }),
        entry("New shell here", MenuAction::NewHere { dir: path.to_path_buf(), claude: false }),
        with_shortcut("Recent sessions here", Shortcut::Recents, MenuAction::RecentsFor(path.to_path_buf())),
        MenuItem::Separator,
    ];
    items.extend(folder_items(path, vscode_args(path, container, None)));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_tree::{PaneEntry, Row, RowKind, ToneCounts, Tone};
    use protocol::message::{SessionInfo, SessionState};

    fn labels(items: &[MenuItem]) -> Vec<&'static str> {
        items.iter().map(|item| match item { MenuItem::Entry(entry) => entry.label, MenuItem::Separator => "│" }).collect()
    }

    fn session(state: SessionState) -> SessionInfo {
        SessionInfo {
            id: "s1".into(), cwd: "/r/app".into(), title: "t".into(), last_active_ms: 0, state, cwd_exists: true,
            status: None, status_since_ms: 0, container: None,
        }
    }

    fn session_row(state: SessionState, tone: Tone, leaf: Option<NodeId>) -> Row {
        Row::new(RowKind::Session { session: session(state), tone, note: String::new(), leaf })
    }

    fn pane_row(claude: bool, ended: bool) -> Row {
        let entry = PaneEntry {
            leaf: 7, cwd: "/r/app".into(), title: "p".into(), session: claude.then(|| "new".to_string()), claude, ended,
            alone: true, container: None,
        };
        Row::new(RowKind::Pane { entry, tone: if ended { Tone::Closed } else { Tone::Idle }, note: String::new() })
    }

    const ALONE: &dyn Fn(NodeId) -> bool = &|_| true;

    #[test]
    fn a_tab_offers_splits_folder_items_and_close() {
        let items = tab_menu(2, Path::new("/r/app"), vec!["/r/app".into()]);
        assert_eq!(labels(&items), ["Split right", "Split down", "│", "Copy folder path", "Open folder", "Open in VS Code", "│", "Close tab"]);
        let MenuItem::Entry(first) = &items[0] else { panic!() };
        assert_eq!((first.action.clone(), first.shortcut), (MenuAction::Split { tab: 2, axis: Axis::Horizontal }, Some(Shortcut::SplitRight)));
        let MenuItem::Entry(last) = items.last().unwrap() else { panic!() };
        assert_eq!(last.action, MenuAction::CloseTab(2));
    }

    #[test]
    fn a_live_claude_row_offers_new_here_copy_and_close() {
        let row = session_row(SessionState::Managed { pane: 1 }, Tone::Idle, Some(4));
        let items = row_menu(&row, ALONE).unwrap();
        assert_eq!(labels(&items), ["New Claude here", "│", "Copy session id", "Copy folder path", "Open folder", "Open in VS Code", "│", "Close tab"]);
        let MenuItem::Entry(copy) = &items[2] else { panic!() };
        assert_eq!(copy.action, MenuAction::Copy("s1".into()));
    }

    #[test]
    fn close_says_pane_in_a_split_and_tab_when_alone() {
        let row = session_row(SessionState::Managed { pane: 1 }, Tone::Idle, Some(4));
        let split: &dyn Fn(NodeId) -> bool = &|_| false;
        let items = row_menu(&row, split).unwrap();
        let MenuItem::Entry(close) = items.last().unwrap() else { panic!() };
        assert_eq!((close.label, close.action.clone()), ("Close pane", MenuAction::ClosePane(4)));
    }

    #[test]
    fn an_outside_session_offers_take_over_only_with_a_host_pid_and_folder() {
        let row = session_row(SessionState::External { pid: Some(42) }, Tone::Elsewhere, None);
        let items = row_menu(&row, ALONE).unwrap();
        assert_eq!(labels(&items), ["Take over…", "│", "Copy session id", "Copy folder path", "Open folder", "Open in VS Code"]);
        let MenuItem::Entry(take) = &items[0] else { panic!() };
        assert!(take.accent);
        assert_eq!(take.action, MenuAction::TakeOver { session: "s1".into(), cwd: "/r/app".into() });
        let boxed = session_row(SessionState::External { pid: None }, Tone::Elsewhere, None);
        assert_eq!(labels(&row_menu(&boxed, ALONE).unwrap())[0], "Copy session id");
    }

    #[test]
    fn an_exited_claude_offers_continue() {
        let row = session_row(SessionState::Closed, Tone::Closed, Some(4));
        let items = row_menu(&row, ALONE).unwrap();
        assert_eq!(labels(&items), ["Continue", "│", "Copy session id", "Copy folder path", "Open folder", "Open in VS Code", "│", "Close tab"]);
        let MenuItem::Entry(cont) = &items[0] else { panic!() };
        assert_eq!((cont.accent, cont.action.clone()), (true, MenuAction::Continue(4)));
        let items = row_menu(&pane_row(true, true), ALONE).unwrap();
        assert_eq!(labels(&items)[0], "Continue");
    }

    #[test]
    fn a_new_claude_pane_and_a_shell_have_their_menus() {
        assert_eq!(labels(&row_menu(&pane_row(true, false), ALONE).unwrap()), [
            "New Claude here", "│", "Copy session id", "Copy folder path", "Open folder", "Open in VS Code", "│", "Close tab",
        ]);
        let shell = row_menu(&pane_row(false, false), ALONE).unwrap();
        assert_eq!(labels(&shell), ["New shell here", "│", "Copy folder path", "Open folder", "Open in VS Code", "│", "Close tab"]);
        let MenuItem::Entry(new) = &shell[0] else { panic!() };
        assert_eq!(new.action, MenuAction::NewHere { dir: "/r/app".into(), claude: false });
    }

    #[test]
    fn a_project_heading_offers_new_sessions_and_recents() {
        let items = project_menu(Path::new("/r/app"), None);
        assert_eq!(labels(&items), ["New Claude here", "New shell here", "Recent sessions here", "│", "Copy folder path", "Open folder", "Open in VS Code"]);
        let MenuItem::Entry(recents) = &items[2] else { panic!() };
        assert_eq!((recents.action.clone(), recents.shortcut), (MenuAction::RecentsFor("/r/app".into()), Some(Shortcut::Recents)));
        let row = Row::new(RowKind::Project { path: "/r/app".into(), label: "app".into(), expanded: true, counts: ToneCounts::default() });
        assert_eq!(row_menu(&row, ALONE), None, "headings are built with project_menu (they need the container)");
    }

    #[test]
    fn detail_rows_have_no_menu() {
        let row = Row::new(RowKind::Detail { session: "s".into(), text: "x".into() });
        assert_eq!(row_menu(&row, ALONE), None);
    }

    #[test]
    fn vscode_opens_a_container_session_inside_its_container() {
        let place = ContainerPlace { host_folder: Some("/tmp/a".into()), path: "/workspaces/a/sub".into() };
        assert_eq!(vscode_args(Path::new("/tmp/a/sub"), Some(&place), None), [
            "--folder-uri".to_string(), "vscode-remote://dev-container+2f746d702f61/workspaces/a/sub".to_string(),
        ]);
    }

    #[test]
    fn vscode_uses_the_default_workspace_folder_for_a_container_pane() {
        assert_eq!(vscode_args(Path::new("/tmp/a"), None, Some(Path::new("/tmp/a"))), [
            "--folder-uri".to_string(), "vscode-remote://dev-container+2f746d702f61/workspaces/a".to_string(),
        ]);
    }

    #[test]
    fn vscode_falls_back_to_the_folder_without_a_host_folder() {
        let unknown = ContainerPlace { host_folder: None, path: "/workspaces/x".into() };
        assert_eq!(vscode_args(Path::new("/r/app"), Some(&unknown), None), ["/r/app".to_string()]);
        assert_eq!(vscode_args(Path::new("/r/app"), None, None), ["/r/app".to_string()]);
    }

    #[test]
    fn a_container_row_opens_vscode_in_its_container() {
        let mut row = session_row(SessionState::Managed { pane: 1 }, Tone::Idle, Some(4));
        let RowKind::Session { session, .. } = &mut row.kind else { panic!() };
        session.container = Some(ContainerPlace { host_folder: Some("/tmp/a".into()), path: "/workspaces/a".into() });
        let items = row_menu(&row, ALONE).unwrap();
        let code = items.iter().find_map(|item| match item { MenuItem::Entry(e) if e.label == "Open in VS Code" => Some(e.action.clone()), _ => None });
        assert_eq!(code, Some(MenuAction::OpenInVsCode(vec!["--folder-uri".into(), "vscode-remote://dev-container+2f746d702f61/workspaces/a".into()])));
    }
}
