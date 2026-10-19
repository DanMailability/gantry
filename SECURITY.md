# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use this repository's private reporting instead: open the
Security tab, choose "Report a vulnerability", and describe what you found.

Include the Gantry version (`gantry --version`), your operating system, and steps that reproduce the problem. If a
proof of concept is involved, a short one is enough.

## What to expect

- An acknowledgement within 7 days.
- A fix or a plan within 30 days for confirmed issues, depending on severity. This is a small project with one
  maintainer, so these are goals, not guarantees.
- Credit in the release notes if you want it.

## Scope

Gantry runs your shell and your own `claude` command, reads Claude's session files read-only, and can connect to SSH
hosts you add. Problems in those areas are in scope: privilege or file access beyond what is described, sign-in
prompts that leak what you type, remote execution through the daemon socket, and unsafe handling of terminal output.
Problems in Claude Code itself, in your shell, or in third-party libraries belong to their own projects, but tell us if
Gantry makes them worse.

Only the latest release is supported.
