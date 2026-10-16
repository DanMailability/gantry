# Gantry

A cross-platform terminal for managing many [Claude Code](https://claude.com/claude-code) sessions at once.

Gantry is a GPU-rendered terminal (built on [GPUI](https://www.gpui.rs/)) that knows what Claude is doing. Every pane
that runs Claude shows its status, so you can see at a glance which sessions are working, which are done and which are
waiting for you, and jump straight to the one that needs input.

## Features

- **Sessions at a glance.** A sidebar lists every active session by project. A session's status colors its row, its
  title-bar chip and its pane header: working, idle, needs input, done. Working rows glow.
- **Alerts.** A native notification and a chime when a session finishes or needs input while you are looking elsewhere.
  Per-event badge, notification, sound, tone and volume are configurable.
- **Tabs and splits.** Split any pane right or down, with Claude or a plain shell. Layouts are saved and restored.
- **Sessions that outlive the window.** A per-profile daemon owns every PTY, so closing the GUI does not end your
  sessions. Reopen it and it reattaches. Finished panes offer Continue (`claude --resume`) or Restart.
- **Recent sessions and search.** Find a past session by title, or search inside conversations (your prompts and
  Claude's replies, indexed with Tantivy).
- **Last prompt card.** Each Claude pane shows its newest prompt above the terminal, so you remember what you asked.
- **Several Claude config dirs.** Work with more than one `CLAUDE_CONFIG_DIR` (personal, work, ...), each with its own
  badge and color. A first-run wizard finds them for you.
- **Dev containers.** Sessions that run in a dev container are listed under their host folder and resumed inside it.
- **Take over.** Sessions running outside Gantry show up too, and can be taken over into a pane.
- **Right-to-left text.** Hebrew and Arabic display in the right order, as a display-only layer (selection, copy and
  links stay logical).
- **Everything rebindable.** Shortcuts, theme (Mist, Mist light, follow system), fonts, status colors and project
  colors live in a Settings tab and in a hand-editable `settings.json` that applies live.

Gantry only reads Claude's config directory. It never writes to it, and it uses no Claude hooks: status comes from the
pid files Claude already writes.

## Install

### Linux

Download the tarball from a release, unpack it and run its installer. It installs into `~/.local`:

```sh
tar xf gantry-<version>-x86_64-linux.tar.gz
cd gantry-<version>-x86_64-linux
./install.sh              # ./install.sh --uninstall to remove
```

The binary needs glibc 2.35 or newer (Ubuntu 22.04+, Fedora). On Ubuntu you also need `libasound2`
(`libasound2t64` on 24.04), `libxkbcommon0`, `libxkbcommon-x11-0` and `libxcb1`.

An RPM can be built with `packaging/linux/build-rpm.sh`.

### macOS

Apple Silicon only. Build the DMG on a Mac with `packaging/macos/build-dmg.sh`. The app is ad-hoc signed and not
notarized, so macOS blocks it the first time ("Apple can't verify..."). To open it:

1. Open the DMG and drag `Gantry.app` into Applications. Don't run it from inside the DMG.
2. macOS 15 Sequoia or newer: double-click it and dismiss the dialog with Done. Then open System Settings > Privacy &
   Security, scroll to the Security section, click Open Anyway next to Gantry, authenticate and confirm. Once only.
3. macOS 14 or older: right-click it in Applications, choose Open, then click Open.

Or, on any version, remove the quarantine flag instead of steps 2-3:

```sh
xattr -dr com.apple.quarantine /Applications/Gantry.app
```

#### Permission prompts

macOS may ask whether Gantry can access files in Desktop, Documents, Downloads, removable volumes or other apps' data.
Gantry itself does not read those folders: the shells and `claude` it starts do, and macOS attributes their access to
the app that launched them. Allow it once per folder, or change it later in System Settings > Privacy & Security >
Files and Folders. Granting Full Disk Access is not needed.

This build has not been tried on a Mac yet.

### Windows

Builds in CI, with some known gaps (see the open issues).

## Usage

```sh
gantry                              # a shell in a new window
gantry --cwd ~/code/project -- claude   # Claude in a folder
gantry daemon                       # run the daemon in the foreground (normally started for you)
```

Default shortcuts on Linux and Windows (macOS uses `Cmd`). All are rebindable in Settings.

| Action | Shortcut |
| --- | --- |
| New Claude tab | `Ctrl+Shift+T` |
| Launcher (open a folder) | `Ctrl+Shift+O` |
| Find session | `Ctrl+Shift+F` |
| Recent sessions | `Ctrl+Shift+R` |
| Split right / down (Claude) | `Ctrl+Shift+D` / `Ctrl+Shift+E` |
| Split with a shell | add `Alt` |
| Move focus between panes | `Ctrl+Shift+Arrows` |
| Next / previous tab | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Close pane | `Ctrl+Shift+W` |
| Toggle sidebar | `Ctrl+Shift+B` |
| Settings | `Ctrl+,` |
| Zoom in / out / reset | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |

## Configuration

Settings live in `~/.config/gantry/settings.json` (the platform config dir). Edit it by hand or use the Settings tab;
Gantry watches the file and applies changes live. Unknown keys are kept, and a bad value falls back to its default with
a banner in Settings.

Gantry asks GitHub once a day whether a newer release exists (a plain request to the GitHub API that sends only a
`gantry/<version>` User-Agent) and shows a title-bar notice with the release notes. It never downloads or installs
anything. Turn it off in Settings > Window > Updates, or with `"checkForUpdates": false`.

Layouts, logs and the search index are kept per profile under `~/.local/share/gantry/<profile>/`. Debug builds use the
`dev` profile and release builds `default`; set `GANTRY_PROFILE` to override.

## Building from source

You need a recent stable Rust toolchain (see `rust-toolchain.toml`) and, on Linux, `alsa-lib-devel` for the alert sounds.

```sh
cargo run -p gantry
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

GPUI is a large dependency. In this repo, compiling commands go through `scripts/cargo`, which serialises builds with a
machine-wide lock and caps their CPU and memory; see `CLAUDE.md`.

## Repository layout

| Crate | Role |
| --- | --- |
| `crates/gantry` | The binary: argument parsing, then the GUI |
| `crates/ui-gpui` | The GUI. The only crate that depends on `gpui` |
| `crates/app-core` | GPUI-free logic: terminal state, workspace and split tree, settings, shortcuts, bidi |
| `crates/claude-index` | Read-only index of Claude's config dir: transcripts, live pid files, dev containers |
| `crates/protocol` | Wire types between the GUI and the daemon |
| `crates/daemon` | PTYs, the session list, the conversation search index, container support |

The design spec is in `docs/superpowers/specs/` and the implementation plans in `docs/superpowers/plans/`. Older
documents call the project `ccm`, its former name.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
