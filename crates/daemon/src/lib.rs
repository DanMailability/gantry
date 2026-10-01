//! Session-owning side of Gantry. For now only PTY process management; the daemon server arrives in M2.

pub mod client;
pub mod launch;
pub mod outbox;
pub mod paths;
pub mod pty;
pub mod server;
pub mod sessions;
pub mod shell_env;
pub mod take_over;
