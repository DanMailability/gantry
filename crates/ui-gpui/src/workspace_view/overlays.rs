use super::{OpenLauncher, OpenRecents, OpenSettings, WorkspaceView};
use std::collections::HashSet;
use std::path::PathBuf;

use app_core::host_state::{HostDot, HostState};
use app_core::launcher::{self, OpenFolder, OpenPane};
use app_core::recents;
use app_core::release::CheckStatus;
use app_core::search_merge::{self};
use app_core::split_tree::NodeId;
use app_core::theme_colors::terminal_palette;
use app_core::workspace::ClaudeDirs;
use daemon::askpass::AskPrompt;
use gpui::{Context, Focusable, PathPromptOptions, Window, prelude::*};
use protocol::host::HostId;
use protocol::message::{PaletteColors, SessionInfo};

use app_core::session_tree::status_tone;

use crate::askpass_view::{AskpassEvent, AskpassView};
use crate::launcher_view::{LauncherEvent, LauncherHost, LauncherView};
use crate::recents_view::{RecentsEvent, RecentsView, SEARCH_LIMIT};
use crate::release_check::CheckRow;
use crate::settings_store::SettingsStore;
use crate::settings_view::{SettingsEvent, SettingsView};
use crate::setup_view::{Mode as SetupMode, SetupEvent, SetupView};
use crate::ssh_wizard::WizardStart;
use crate::theme::theme;

impl WorkspaceView {
    /// Take over what the settings say that is not drawn by `render`: pane colors, the daemon's palette, and the
    /// alert threshold.
    pub(super) fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let preset = theme(cx).preset;
        for slot in self.slots.values() {
            slot.view.update(cx, |view, cx| view.apply_theme(cx));
        }
        if self.sent_palette != Some(preset) {
            for conn in self.hosts.values() {
                conn.client.set_palette(PaletteColors::from(terminal_palette(preset)));
            }
            self.sent_palette = Some(preset);
        }
        let settings = cx.global::<SettingsStore>().settings().clone();
        self.ssh_hosts = settings.ssh_hosts.clone();
        if settings.check_for_updates != self.check_updates {
            self.check_updates = settings.check_for_updates;
            if self.check_updates {
                self.check_releases(false, cx);
            } else {
                self.manual_found = false;
                self.releases.clear();
                self.release_etag = None;
                self.release_card = None;
                self.set_check_row(CheckStatus::Idle, cx);
                cx.notify();
            }
        }
        // A host that left the settings is forgotten: no connection, no timer, no attempt, and its panes say so.
        let listed: HashSet<String> = self.ssh_hosts.iter().map(|host| host.name.clone()).collect();
        let removed: Vec<HostId> = self
            .host_states
            .iter()
            .filter(|(host, state)| {
                !listed.contains(host.name()) && !self.seam_hosts.contains(*host) && **state != HostState::NotConnected
            })
            .map(|(host, _)| host.clone())
            .collect();
        for host in removed {
            self.forget_host(&host, cx);
        }
        self.retries.retain(|host, _| listed.contains(host.name()));
        // Added or removed hosts change what the cards of their restored leaves say, whatever their state.
        let remote: HashSet<HostId> =
            self.workspace.leaves.values().filter(|leaf| !leaf.host.is_local()).map(|leaf| leaf.host.clone()).collect();
        for host in remote {
            self.refresh_host_cards(&host, cx);
        }
        self.dirs = ClaudeDirs::from_settings(&settings, self.daemon_dir.as_deref(), std::env::home_dir().as_deref());
        let configured: Vec<PathBuf> = if settings.claude_dirs.is_empty() {
            Vec::new()
        } else {
            self.dirs.dirs.iter().map(|dir| dir.path.clone()).collect()
        };
        if self.sent_dirs.as_ref() != Some(&configured) {
            self.local_client().set_claude_dirs(configured.clone());
            self.sent_dirs = Some(configured);
        }
        let seconds = cx.global::<SettingsStore>().settings().alerts.finished_after_seconds;
        self.attention.set_finished_after_ms(u64::from(seconds) * 1000);
        self.publish_hosts(cx);
        cx.notify();
    }

    /// Tell an open launcher where the hosts' connections stand.
    pub(super) fn push_launcher_hosts(&self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.launcher {
            let hosts = self.launcher_hosts();
            view.update(cx, |view, cx| view.set_hosts(hosts, cx));
        }
    }

    pub(super) fn open_launcher(&mut self, _: &OpenLauncher, window: &mut Window, cx: &mut Context<Self>) {
        self.show_launcher(true, HostId::local(), window, cx);
    }

    /// The folders with live panes, in sidebar order, for the launcher's "Open now".
    fn open_folders(&self, cx: &Context<Self>) -> Vec<OpenFolder> {
        let sessions = self.sidebar.read(cx).sessions();
        let order = self.sidebar.read(cx).leaf_order();
        let mut ids: Vec<NodeId> =
            self.workspace.tabs.iter().filter(|tab| !tab.settings).flat_map(|tab| tab.root.leaves()).collect();
        ids.sort_by_key(|id| order.iter().position(|ordered| ordered == id).unwrap_or(usize::MAX));
        let panes: Vec<OpenPane> = ids
            .into_iter()
            .filter_map(|id| {
                let leaf = self.workspace.leaves.get(&id).filter(|leaf| leaf.pane.is_some())?;
                let status = leaf
                    .session
                    .as_deref()
                    .and_then(|session| sessions.get(&leaf.host, session))
                    .and_then(|info| info.status.as_ref());
                Some(OpenPane {
                    host: leaf.host.clone(),
                    cwd: leaf.launch.cwd.clone(),
                    claude: leaf.launch.is_claude(),
                    tone: status_tone(status, self.unseen_of(id)),
                })
            })
            .collect();
        launcher::open_folders(&panes)
    }

    /// This machine and the SSH hosts as the launcher's host chip lists them.
    fn launcher_hosts(&self) -> Vec<LauncherHost> {
        let local = LauncherHost {
            id: HostId::local(),
            label: "Local".into(),
            dot: HostDot::Ok,
            connected: true,
            state: "connected".into(),
        };
        let hosts = self.ssh_hosts.iter().map(|entry| {
            let (id, state) = (HostId::named(&entry.name), self.host_state(&HostId::named(&entry.name)));
            LauncherHost {
                label: format!("{} · {}", entry.name, state.label()),
                dot: state.dot(),
                connected: self.client(&id).is_some(),
                state: state.label(),
                id,
            }
        });
        if self.ssh_hosts.is_empty() { vec![local] } else { std::iter::once(local).chain(hosts).collect() }
    }

    /// Open the launcher on `host`; `claude_first`: Enter starts Claude and Alt+Enter a shell, false swaps them.
    pub(super) fn show_launcher(
        &mut self,
        claude_first: bool,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.setup.is_some() || self.ssh_wizard.is_some() {
            return;
        }
        self.recents = None;
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let open = self.open_folders(cx);
        let hosts = self.launcher_hosts();
        let view = cx.new(|cx| LauncherView::new(open, sessions, claude_first, hosts, host, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &LauncherEvent, window, cx| {
            this.on_launcher_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.launcher = Some((view, subscription));
        cx.notify();
    }

    /// A question from ssh: show it now, or after the ones before it.
    pub(super) fn ask(&mut self, prompt: AskPrompt, window: &mut Window, cx: &mut Context<Self>) {
        self.askpass_queue.push_back(prompt);
        if self.askpass.is_none() {
            self.show_next_question(window, cx);
        }
    }

    fn show_next_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.askpass_queue.pop_front() else {
            self.askpass = None;
            self.focus_current(window, cx);
            cx.notify();
            return;
        };
        let view = cx.new(|cx| AskpassView::new(prompt, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, _: &AskpassEvent, window, cx| this.show_next_question(window, cx));
        view.focus_handle(cx).focus(window, cx);
        self.askpass = Some((view, subscription));
        cx.notify();
    }

    /// Open the setup wizard; `mode`: it opened by itself on the first start, or from Settings.
    pub(super) fn show_setup(&mut self, mode: SetupMode, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        self.recents = None;
        let view = cx.new(|cx| SetupView::new(mode, self.daemon_dir.clone(), cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &SetupEvent, window, cx| match event {
            SetupEvent::Close => {
                this.setup = None;
                this.focus_current(window, cx);
                cx.notify();
            }
        });
        view.focus_handle(cx).focus(window, cx);
        self.setup = Some((view, subscription));
        cx.notify();
    }

    pub(super) fn open_recents(&mut self, _: &OpenRecents, window: &mut Window, cx: &mut Context<Self>) {
        self.show_recents(None, window, cx);
    }

    /// Open the recent sessions pop-up, on `project`'s sessions when given.
    pub(super) fn show_recents(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.is_some() || self.ssh_wizard.is_some() {
            return;
        }
        self.launcher = None;
        let sessions = self.recents_sessions(cx);
        let offline = self.offline_hosts();
        let open: HashSet<(HostId, String)> = self
            .workspace
            .leaves
            .values()
            .filter_map(|leaf| Some((leaf.host.clone(), leaf.session.clone()?)))
            .collect();
        let view = cx.new(|cx| RecentsView::new(sessions, open, project, offline, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &RecentsEvent, window, cx| {
            this.on_recents_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.search_query.clear();
        self.recents = Some((view, subscription));
        self.push_search_state(cx);
        cx.notify();
    }

    /// The sessions of this machine and of the connected hosts.
    fn recents_sessions(&self, cx: &Context<Self>) -> Vec<SessionInfo> {
        recents::listed_sessions(self.sidebar.read(cx).sessions().all(), |host| self.client(host).is_some())
    }

    /// Names of the SSH hosts that are not connected.
    fn offline_hosts(&self) -> Vec<String> {
        let configured: Vec<String> = self.ssh_hosts.iter().map(|entry| entry.name.clone()).collect();
        recents::offline_hosts(&configured, |host| self.client(host).is_some())
    }

    /// Give an open recent sessions pop-up the sessions of the hosts that are connected now.
    pub(super) fn push_recents(&self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.recents {
            let (sessions, offline) = (self.recents_sessions(cx), self.offline_hosts());
            view.update(cx, |view, cx| view.set_sessions(sessions, offline, cx));
        }
    }

    /// The hosts conversation search asks: this machine, then the connected SSH hosts in settings order.
    fn search_order(&self) -> Vec<HostId> {
        let configured: Vec<String> = self.ssh_hosts.iter().map(|entry| entry.name.clone()).collect();
        search_merge::query_order(&configured, |host| self.client(host).is_some())
    }

    /// Show the merged hits of every host in an open recent sessions pop-up.
    pub(super) fn push_search_hits(&self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.recents {
            let merged = search_merge::merge(&self.search_order(), &self.search_answers);
            let request = self.search_request;
            view.update(cx, |view, cx| view.set_hits(request, merged.hits, merged.pending, cx));
        }
    }

    /// Tell an open recent sessions pop-up where conversation search stands.
    pub(super) fn push_search_state(&self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.recents {
            let merged = search_merge::merge(&self.search_order(), &self.search_answers);
            view.update(cx, |view, cx| {
                view.set_index_state(merged.progress, cx);
                view.set_search_unavailable(merged.unavailable, cx);
            });
        }
    }

    fn on_recents_event(&mut self, event: RecentsEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RecentsEvent::Search { request, query } => {
                let order = self.search_order();
                self.search_request = request;
                self.search_query = query.clone();
                self.search_answers.retain(|host, _| order.contains(host));
                for answer in self.search_answers.values_mut() {
                    answer.hits.clear();
                    answer.answered = false;
                }
                for host in &order {
                    if let Some(client) = self.client(host) {
                        client.search(request, &query, SEARCH_LIMIT);
                    }
                }
                return;
            }
            RecentsEvent::Connect(host) => {
                self.connect_host(&host, true, window, cx);
                return;
            }
            RecentsEvent::Resume { host, session, cwd, split } => {
                self.recents = None;
                self.resume_session(host, session, cwd, split, window, cx);
            }
            RecentsEvent::Dismiss => {
                self.recents = None;
                self.focus_current(window, cx);
            }
        }
        cx.notify();
    }

    fn on_launcher_event(&mut self, event: LauncherEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let LauncherEvent::ListDir { host, request, path } = &event {
            if let Some(client) = self.client(host) {
                client.list_dir(*request, path);
            }
            return;
        }
        self.launcher = None;
        match event {
            LauncherEvent::Open { host, dir, claude, .. } if !host.is_local() => {
                self.open_on_host(&host, claude, dir, window, cx)
            }
            LauncherEvent::Open { dir, claude, dir_pick, .. } => self.open_launcher_pick(claude, dir, dir_pick, cx),
            LauncherEvent::ListDir { .. } => {}
            LauncherEvent::Browse { claude, dir_pick } => {
                let picked = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: None,
                });
                cx.spawn_in(window, async move |this, cx| {
                    let dir = match picked.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| match dir {
                        Some(dir) => this.open_launcher_pick(claude, dir, dir_pick, cx),
                        None => this.focus_current(window, cx),
                    });
                })
                .detach();
            }
            LauncherEvent::Dismiss => self.focus_current(window, cx),
        }
        cx.notify();
    }

    pub(super) fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(leaf) = self.workspace.focused() {
            self.before_settings = Some(leaf);
        }
        self.workspace.open_settings();
        if self.settings.is_none() {
            let view = cx.new(|cx| SettingsView::new(self.daemon_dir.clone(), window, cx));
            self._settings_events =
                Some(cx.subscribe_in(&view, window, |this, _, event: &SettingsEvent, window, cx| match event {
                    SettingsEvent::DetectClaudeDirs => this.show_setup(SetupMode::Rerun, window, cx),
                    SettingsEvent::AddSshHost => this.open_ssh_wizard(WizardStart::Add, window, cx),
                    SettingsEvent::CheckNow => this.check_releases(true, cx),
                    SettingsEvent::HostRenamed { from, to, reconnect } => {
                        this.host_renamed(&HostId::named(from), &HostId::named(to), *reconnect, window, cx)
                    }
                    SettingsEvent::HostRemoved(name) => this.host_removed(&HostId::named(name), window, cx),
                    SettingsEvent::ReconnectHost(name) => {
                        let host = HostId::named(name);
                        this.forget_host(&host, cx);
                        this.connect_host(&host, true, window, cx);
                    }
                }));
            self.settings = Some(view);
        }
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Close the Settings tab and drop its page; no confirmation, it runs nothing.
    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.settings_active();
        self.workspace.close_settings();
        if let Some(leaf) = self.before_settings.take().filter(|_| showing) {
            self.workspace.focus(leaf);
        }
        self.settings = None;
        if cx.global::<CheckRow>().status != CheckStatus::Checking {
            self.set_check_row(CheckStatus::Idle, cx);
        }
        self._settings_events = None;
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
    }
}
