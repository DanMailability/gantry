# Changelog

User-facing changes to Gantry. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the version follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (`0.MINOR.PATCH`).

## [Unreleased]

### Added
- Settings shows which version of Gantry is running, at the bottom of its menu.
- When a newer Gantry is installed while it is open, the title bar offers to restart into it; your sessions resume after the restart.
- `gantry --version`.

### Changed
- Rows marked as unread now look different from Done rows: a quieter green fill and a pin before the title, also on the title-bar chip and the pane header, and "Unread" in the hover card.

Needs a daemon restart (the daemon now reports its version).

## [0.2.0] - 2026-10-05

First versioned state: everything built up to this point.
