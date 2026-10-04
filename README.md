# Gantry

A cross-platform terminal for managing many [Claude Code](https://claude.com/claude-code) sessions at once.

Gantry is a GPU-rendered terminal (built on [GPUI](https://www.gpui.rs/)) that knows what Claude is doing. Every pane
that runs Claude shows its status, so you can see at a glance which sessions are working, which are done and which are
waiting for you, and jump straight to the one that needs input.

## Features

- **Sessions at a glance.** A sidebar lists every active session by project. A session's status colours its row, its
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
  badge and colour. A first-run wizard finds them for you.
- **Dev containers.** Sessions that run in a dev container are listed under their host folder and resumed inside it.
- **Take over.** Sessions running outside Gantry show up too, and can be taken over into a pane.
- **Right-to-left text.** Hebrew and Arabic display in the right order, as a display-only layer (selection, copy and
  links stay logical).
- **Everything rebindable.** Shortcuts, theme (Mist, Mist light, follow system), fonts, status colours and project
  colours live in a Settings tab and in a hand-editable `settings.json` that applies live.

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

Apple Silicon only. Build the app bundle on a Mac with `packaging/macos/build-dmg.sh`. The app is ad-hoc signed and not
notarized: right-click it and choose Open, or run `xattr -dr com.apple.quarantine Gantry.app`. This build has not been
tried on a Mac yet.

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
