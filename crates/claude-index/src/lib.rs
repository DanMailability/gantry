//! Discovers Claude Code sessions from Claude's config directory (read-only): transcripts for history,
//! pid files for live instances. Formats are undocumented, so every reader is defensive.

pub mod config;
pub mod live;
pub mod transcript;

