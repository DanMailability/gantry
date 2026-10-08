//! The SSH hosts section: the hosts in `sshHosts` with their live state, rename, edit target, remove, and the way into
//! the Add SSH host wizard.

use std::collections::{HashMap, HashSet};

use app_core::host_state::HostDot;
use app_core::settings::{Settings, SshAuth, SshHost, replace_ssh_host};
use gpui::{Context, Div, Global, Window, div, px, rgb};

use super::*;
use crate::context_menu::host_dot_color;
use crate::theme::Theme;

/// A host's name field on this page (the row index is the host's place in `sshHosts`).
pub(super) const HOST_NAME: &str = "ssh-name";
/// A host's target field.
pub(super) const HOST_TARGET: &str = "ssh-target";

/// What the page shows of one host's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostStatus {
    /// The state's short label.
    pub(crate) label: String,
    /// The dot beside it.
    pub(crate) dot: HostDot,
    /// How many panes the host has in the layout.
    pub(crate) panes: usize,
}

/// The state of every listed host by name, kept up to date by the workspace.
#[derive(Default, PartialEq, Eq)]
pub(crate) struct HostStatuses {
    /// Each listed host's state.
    pub(crate) hosts: HashMap<String, HostStatus>,
    /// Names of hosts that are not listed but still have panes or a daemon instance in the layout: no host may take
    /// such a name.
    pub(crate) taken: HashSet<String>,
}

impl Global for HostStatuses {}

/// A field of a host's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostField {
    Name,
    Target,
}

/// What an edit of a host's row did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HostEdit {
    /// The trimmed value is the one it had.
    Unchanged,
    /// The host has a new name.
    Renamed {
        /// The old name.
        from: String,
        /// The new name.
        to: String,
    },
    /// The host (by name) has a new target.
    Retargeted(String),
}

/// Apply the text typed into one field of host `index`: nothing when it is unchanged, an error (and no change) when
/// the value is not allowed or `taken` names a host that still has panes in the layout.
pub(super) fn edit_ssh_host(
    hosts: &mut [SshHost],
    index: usize,
    field: HostField,
    text: &str,
    taken: &HashSet<String>,
) -> Result<HostEdit, String> {
    let old = hosts.get(index).cloned().ok_or_else(|| "That host is gone".to_string())?;
    let text = text.trim();
    let mut new = old.clone();
    match field {
        HostField::Name => new.name = text.to_string(),
        HostField::Target => new.target = text.to_string(),
    }
    if new == old {
        return Ok(HostEdit::Unchanged);
    }
    if field == HostField::Name && taken.contains(text) {
        return Err(format!("{text} still has panes in the layout; remove it first"));
    }
    replace_ssh_host(hosts, &old.name, new.clone())?;
    Ok(match field {
        HostField::Name => HostEdit::Renamed { from: old.name, to: new.name },
        HostField::Target => HostEdit::Retargeted(new.name),
    })
}

/// The sign-in method after `auth` when its link is clicked.
fn next_auth(auth: SshAuth) -> SshAuth {
    match auth {
        SshAuth::Automatic => SshAuth::Password,
        SshAuth::Password => SshAuth::Key,
        SshAuth::Key => SshAuth::Automatic,
    }
}

/// The text of a host's sign-in link.
fn auth_label(auth: SshAuth) -> &'static str {
    match auth {
        SshAuth::Automatic => "Sign in: automatic",
        SshAuth::Password => "Sign in: password",
        SshAuth::Key => "Sign in: SSH key",
    }
}

/// The question before a host is removed.
pub(super) fn remove_text(name: &str, panes: usize) -> String {
    let panes = match panes {
        0 => String::new(),
        1 => " and close its 1 pane".to_string(),
        n => format!(" and close its {n} panes"),
    };
    format!("Remove {name}{panes}? Nothing on the remote is deleted.")
}

impl SettingsView {
    pub(super) fn ssh_hosts_page(
        &mut self,
        theme: &Theme,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let statuses: HashMap<String, HostStatus> =
            cx.try_global::<HostStatuses>().map(|statuses| statuses.hosts.clone()).unwrap_or_default();
        let mut rows: Vec<Div> = Vec::new();
        for (index, host) in settings.ssh_hosts.iter().enumerate() {
            let status = statuses.get(&host.name);
            let name =
                self.text_field((HOST_NAME, index), &host.name, "Name", false, theme, window, cx).w(theme.ui(200.));
            let target = self
                .text_field((HOST_TARGET, index), &host.target, "user@host", true, theme, window, cx)
                .w(theme.ui(260.));
            let (dot, label) =
                status.map_or((HostDot::None, "not connected".to_string()), |s| (s.dot, s.label.clone()));
            let state = div()
                .flex()
                .items_center()
                .gap(theme.ui(8.))
                .child(div().flex_none().size(theme.ui(8.)).rounded_full().bg(rgb(host_dot_color(theme, dot))))
                .child(div().text_sm().text_color(rgb(theme.tokens.muted)).child(label));
            let name_for_remove = host.name.clone();
            let remove = self.link(
                "ssh-remove",
                index,
                "Remove",
                true,
                handler(move |this, _, cx| {
                    this.confirm_remove = Some(name_for_remove.clone());
                    cx.notify();
                }),
                window,
                cx,
            );
            let (name_for_auth, next_auth) = (host.name.clone(), next_auth(host.auth));
            let auth = self.link(
                "ssh-auth",
                index,
                auth_label(host.auth),
                true,
                handler(move |this, _, cx| {
                    let name = name_for_auth.clone();
                    SettingsStore::update(cx, |settings| {
                        if let Some(host) = settings.ssh_hosts.iter_mut().find(|host| host.name == name) {
                            host.auth = next_auth;
                        }
                    });
                    this.edited_targets.insert(name);
                    cx.notify();
                }),
                window,
                cx,
            );
            let mut trailing = actions_slot(theme).child(auth).child(remove);
            if self.edited_targets.contains(&host.name) && dot == HostDot::Ok {
                let name_for_reconnect = host.name.clone();
                trailing = trailing.child(self.link(
                    "ssh-reconnect",
                    index,
                    "Reconnect to apply",
                    true,
                    handler(move |this, _, cx| {
                        this.edited_targets.remove(&name_for_reconnect);
                        cx.emit(SettingsEvent::ReconnectHost(name_for_reconnect.clone()));
                        cx.notify();
                    }),
                    window,
                    cx,
                ));
            }
            rows.push(row_shell(
                theme,
                div().flex().flex_col().gap(theme.ui(6.)).child(name).child(target),
                state,
                trailing.justify_end(),
            ));
            if self.confirm_remove.as_deref() == Some(host.name.as_str()) {
                let (name_to_remove, panes) = (host.name.clone(), status.map_or(0, |s| s.panes));
                let confirm = self.button(
                    "ssh-remove-confirm",
                    index,
                    "Remove",
                    window,
                    cx,
                    handler(move |this, _, cx| {
                        this.confirm_remove = None;
                        let name = name_to_remove.clone();
                        SettingsStore::update(cx, |settings| {
                            remove_ssh_host(&mut settings.ssh_hosts, &name);
                        });
                        cx.emit(SettingsEvent::HostRemoved(name_to_remove.clone()));
                        cx.notify();
                    }),
                );
                let cancel = self.link(
                    "ssh-remove-cancel",
                    index,
                    "Cancel",
                    true,
                    handler(|this, _, cx| {
                        this.confirm_remove = None;
                        cx.notify();
                    }),
                    window,
                    cx,
                );
                rows.push(row_shell(
                    theme,
                    muted(theme, remove_text(&host.name, panes)),
                    confirm,
                    actions_slot(theme).child(cancel),
                ));
            }
        }
        if settings.ssh_hosts.is_empty() {
            rows.push(row_shell(theme, muted(theme, "No SSH hosts yet."), div(), empty_slot(theme)));
        }
        let add =
            self.button("ssh-add", 0, "Add host…", window, cx, handler(|_, _, cx| cx.emit(SettingsEvent::AddSshHost)));
        rows.push(row_shell(
            theme,
            muted(theme, "Pick a host from your ssh config, and Gantry installs itself there."),
            add,
            empty_slot(theme),
        ));
        if let Some(text) = self.ssh_message.clone() {
            let colors = theme.tone(Tone::NeedsInput);
            rows.push(
                div().px(theme.ui(16.)).py(theme.ui(12.)).child(
                    div()
                        .px(theme.ui(10.))
                        .py(theme.ui(6.))
                        .rounded(px(RADIUS_SMALL))
                        .bg(rgb(colors.fill.unwrap_or(theme.tokens.raised)))
                        .text_color(rgb(colors.ink))
                        .text_sm()
                        .child(text),
                ),
            );
        }
        group(
            theme,
            "SSH hosts",
            Some(
                "Machines that run Gantry sessions over ssh. Rename or edit a target here; nothing on the remote is changed.",
            ),
            rows,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_link_cycles_through_every_method() {
        let mut auth = SshAuth::Automatic;
        let mut seen = Vec::new();
        for _ in 0..3 {
            auth = next_auth(auth);
            seen.push(auth);
        }
        assert_eq!(seen, [SshAuth::Password, SshAuth::Key, SshAuth::Automatic]);
    }

    fn host(name: &str, target: &str) -> SshHost {
        SshHost {
            name: name.into(),
            target: target.into(),
            command: "~/.local/bin/gantry".into(),
            auth: Default::default(),
        }
    }

    #[test]
    fn an_unchanged_value_does_nothing_even_with_spaces_around_it() {
        let mut hosts = vec![host("lab", "me@lab")];
        let taken = HashSet::new();
        assert_eq!(edit_ssh_host(&mut hosts, 0, HostField::Name, " lab ", &taken), Ok(HostEdit::Unchanged));
        assert_eq!(edit_ssh_host(&mut hosts, 0, HostField::Target, "me@lab ", &taken), Ok(HostEdit::Unchanged));
        assert_eq!(hosts, vec![host("lab", "me@lab")]);
    }

    #[test]
    fn a_rename_or_retarget_is_reported_and_bad_values_are_refused() {
        let mut hosts = vec![host("lab", "me@lab"), host("dev", "dev")];
        let taken: HashSet<String> = ["ghost".to_string()].into();
        assert_eq!(
            edit_ssh_host(&mut hosts, 0, HostField::Name, "lab2", &taken),
            Ok(HostEdit::Renamed { from: "lab".into(), to: "lab2".into() })
        );
        assert_eq!(
            edit_ssh_host(&mut hosts, 0, HostField::Target, "me@lab2", &taken),
            Ok(HostEdit::Retargeted("lab2".into()))
        );
        assert!(edit_ssh_host(&mut hosts, 0, HostField::Name, "dev", &taken).is_err());
        let error = edit_ssh_host(&mut hosts, 0, HostField::Name, "ghost", &taken).unwrap_err();
        assert!(error.contains("ghost"), "{error}");
        assert!(edit_ssh_host(&mut hosts, 0, HostField::Target, "a b", &taken).is_err());
        assert_eq!(hosts[0], host("lab2", "me@lab2"));
    }

    #[test]
    fn the_remove_question_counts_the_panes() {
        assert_eq!(remove_text("lab", 0), "Remove lab? Nothing on the remote is deleted.");
        assert_eq!(remove_text("lab", 1), "Remove lab and close its 1 pane? Nothing on the remote is deleted.");
        assert_eq!(remove_text("lab", 3), "Remove lab and close its 3 panes? Nothing on the remote is deleted.");
    }
}
