# Changelog

User-facing changes to Gantry. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the version follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (`0.MINOR.PATCH`).

## [Unreleased]

## [0.6.0] - 2026-10-12

### Added
- SSH hosts can have a port: a Port field in the Add SSH host wizard (under Advanced) and in Settings > SSH hosts. Empty uses what ssh and `~/.ssh/config` say.
- An SSH hosts block in the sidebar, above Recent sessions: a card with every configured host and its state (green dot = connected). Click a host that is not connected to connect it (or to open Install or Update when it needs one); "Add SSH host…" sits at its end, so it is there even before any host exists. The × on a connected host asks Detach (leave its sessions running) or Close all sessions (ends them and stops the host's Gantry) when sessions run there, stops an idle host's Gantry at once, and cancels a connection that is still starting. With more than 4 hosts the card starts collapsed to its heading and the connected hosts; click the heading to expand it.

### Changed
- The Add SSH host wizard now installs `gantryd`, Gantry without its window, instead of the full app: it is smaller, uploads faster and needs no system libraries on the host. New hosts default to `~/.local/bin/gantryd`; hosts you added before keep their own command (an Update there replaces that file with `gantryd`). `install.sh` and the RPM install `gantryd` beside `gantry`.
- When Gantry is installed on a host but cannot start for missing system libraries, the wizard now lists every missing library at once with one install command, instead of one library per try.

## [0.5.0] - 2026-10-08

### Added
- SSH hosts have a sign-in method: Automatic, Password (only a password, keys are not tried) or SSH key (no password). Pick it in the Add SSH host wizard or from the host's row in Settings > SSH hosts. A server that hangs up after too many failed tries now says so and points at this setting.

### Fixed
- SSH hosts that need a password: the sign-in dialog now appears. Real ssh calls the helper with just the prompt, which Gantry did not recognise, so ssh gave up with "Permission denied" and no question.
- A host where Gantry is installed but cannot start because a system library is missing (for example `libasound2` or `libxkbcommon-x11-0`) now says which library and package, instead of "A command was not found on the host".

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
