//! Session-owning side of Gantry. For now only PTY process management; the daemon server arrives in M2.

pub mod askpass;
pub mod bridge;
pub mod client;
pub mod containers;
pub mod launch;
pub mod list_dir;
pub mod outbox;
pub mod paths;
pub mod pty;
pub mod remote_install;
pub mod search;
pub mod server;
pub mod sessions;
pub mod shell_env;
pub mod ssh;
pub mod take_over;
