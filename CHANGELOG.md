# Changelog

User-facing changes to Gantry. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the version follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (`0.MINOR.PATCH`).

## [Unreleased]

## [0.4.0] - 2026-10-08

### Added
- SSH hosts: list remote machines in `sshHosts` in settings.json (name and ssh target); the New buttons then offer Local and the hosts, connect on first use with sign-in dialogs, and reconnect by themselves. Gantry can be installed on the remote from the app (see the SSH host wizard below). A connecting host can be cancelled, a host taken over by another Gantry says so instead of reconnecting, and Stop all sessions on quit only stops this machine's sessions. The remote needs lingering (`loginctl enable-linger`) to keep sessions after the last ssh session ends.
- Add SSH host wizard: pick a host from your ssh config or type one, and Gantry installs itself there (Linux x86_64 hosts, from a Linux x86_64 Gantry), checks it, offers to turn on lingering, then saves and connects the host. The upload checks its size (and hash) before it replaces anything, so a dropped connection never leaves a broken Gantry; if turning lingering on fails the wizard shows the command to run and lets you save anyway.
- Update and restart for remote hosts, and Install on a host without Gantry, from the host's card.
- Settings > SSH hosts: see each host's state, rename it, edit its target, remove it, or add one.
- Launcher host chip with remote folders and path completion: pick an SSH host in Ctrl+Shift+O, see its open folders, favorites and recent folders, and type a path that completes from the host.
- Remote sessions in recent sessions and conversation search; hosts that are not connected get a line with a Connect button.
- Take over works on SSH hosts.

### Changed
- Remote sessions no longer offer menu actions that only work on this machine (open folder, VS Code, project color, Claude config dir choice; pinning a remote project heading as a favorite works and is saved as `ssh://<host>/<folder>`).
- Needs a daemon restart (protocol 8, also needed on every SSH host): new panes starting in `~` now open in the home folder.

## [0.3.0] - 2026-10-07

### Added
- Settings shows which version of Gantry is running, at the bottom of its menu.
- When a newer Gantry is installed while it is open, the title bar offers to restart into it; your sessions resume after the restart.
- `gantry --version`.

### Changed
- Rows marked as unread now look different from Done rows: a quieter green fill and a pin before the title, also on the title-bar chip and the pane header, and "Unread" in the hover card.

Needs a daemon restart (the daemon now reports its version).

## [0.2.0] - 2026-10-05

First versioned state: everything built up to this point.
