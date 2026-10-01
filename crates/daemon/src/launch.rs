//! Starting the daemon: one per profile (file lock), private socket, detached process.

use std::fs::{File, TryLockError};
use std::time::Duration;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::{Stream, prelude::*};

use crate::paths::Paths;
use crate::server::{ServerOptions, serve};
use crate::shell_env::{resolve_login_env, shell_quote};

/// How `run_daemon` ended.
#[derive(Debug)]
pub enum DaemonOutcome {
    /// Served and then stopped (stop-all or idle).
    Stopped,
    /// Another daemon already holds this profile's lock.
    AlreadyRunning,
}

/// Daemon configuration.
pub struct DaemonOptions {
    /// Profile locations.
    pub paths: Paths,
    /// Environment for panes; `None` resolves it from the login shell.
    pub base_env: Option<Vec<(String, String)>>,
    /// Stop if no client connects within this time.
    pub idle_timeout: Duration,
}

/// Run the daemon in the current thread until it stops.
pub fn run_daemon(options: DaemonOptions) -> anyhow::Result<DaemonOutcome> {
    let paths = options.paths;
    std::fs::create_dir_all(&paths.runtime_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths.runtime_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = File::create(&paths.lock_file)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(DaemonOutcome::AlreadyRunning),
        Err(TryLockError::Error(err)) => return Err(err.into()),
    }
    #[cfg(unix)]
    let owner_uid = {
        use std::os::unix::fs::MetadataExt;
        Some(std::fs::metadata(&paths.runtime_dir)?.uid())
    };
    #[cfg(windows)]
    let owner_uid = None;
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    runtime.block_on(async {
        // Bind first: connecting clients wait in the backlog while a slow login shell is resolved.
        let listener = listener_options(&paths)?.create_tokio()?;
        let base_env = match options.base_env {
            Some(env) => env,
            None => tokio::task::spawn_blocking(login_env_or_current).await?,
        };
        serve(listener, ServerOptions { base_env, owner_uid, idle_timeout: options.idle_timeout }).await
    })?;
    drop(lock);
    Ok(DaemonOutcome::Stopped)
}

fn listener_options(paths: &Paths) -> std::io::Result<ListenerOptions<'static>> {
    let options = ListenerOptions::new().name(paths.socket_name()?).try_overwrite(true);
    #[cfg(unix)]
    let options = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        options.mode(0o600)
    };
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        use interprocess::os::windows::security_descriptor::SecurityDescriptor;
        // Only the pipe's owner (the current user) may connect.
        options.security_descriptor(SecurityDescriptor::deserialize(widestring::u16cstr!("D:P(A;;GA;;;OW)"))?)
    };
    Ok(options)
}

fn login_env_or_current() -> Vec<(String, String)> {
    #[cfg(unix)]
    if let (Ok(shell), Ok(exe)) = (std::env::var("SHELL"), std::env::current_exe()) {
        let printer = format!("{} internal-print-env", shell_quote(&exe.to_string_lossy()));
        if let Some(env) = resolve_login_env(&shell, &printer, Duration::from_secs(5)) {
            return env;
        }
        tracing::warn!("login shell environment unavailable; using the daemon's own environment");
    }
    std::env::vars().collect()
}

/// Start `<this executable> daemon` fully detached from the caller.
pub fn spawn_detached_daemon() -> std::io::Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("daemon").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    command.spawn().map(|_| ())
}

/// Move this process into a new session with no controlling terminal, so job control on the
/// terminal it was started from (e.g. an interactive login shell taking the terminal) can never
/// stop it. Returns `Ok(false)` if that is impossible because this process leads its process group.
#[cfg(unix)]
pub fn detach_from_terminal() -> std::io::Result<bool> {
    match rustix::process::setsid() {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::PERM) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Windows has no controlling-terminal job control to escape; always succeeds.
#[cfg(windows)]
pub fn detach_from_terminal() -> std::io::Result<bool> {
    Ok(true)
}

/// Connect to the profile's daemon, starting it with `spawn` if none is running.
pub async fn connect_or_spawn(paths: &Paths, spawn: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<Stream> {
    if let Ok(stream) = Stream::connect(paths.socket_name()?).await {
        return Ok(stream);
    }
    spawn()?;
    let mut last_error = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        match Stream::connect(paths.socket_name()?).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("daemon did not start")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn options(dir: &std::path::Path, idle: Duration) -> DaemonOptions {
        DaemonOptions { paths: Paths::in_dir(dir, "t"), base_env: Some(std::env::vars().collect()), idle_timeout: idle }
    }

    #[test]
    fn a_second_daemon_for_the_same_profile_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let first = {
            let options = options(dir.path(), Duration::from_secs(3));
            std::thread::spawn(move || run_daemon(options).unwrap())
        };
        std::thread::sleep(Duration::from_millis(500));
        assert!(matches!(run_daemon(options(dir.path(), Duration::from_secs(1))).unwrap(), DaemonOutcome::AlreadyRunning));
        assert!(matches!(first.join().unwrap(), DaemonOutcome::Stopped));
    }

    #[test]
    fn stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path(), Duration::from_millis(300));
        std::fs::create_dir_all(&options.paths.runtime_dir).unwrap();
        std::fs::write(options.paths.socket_file().unwrap(), b"stale").unwrap();
        assert!(matches!(run_daemon(options).unwrap(), DaemonOutcome::Stopped));
    }

    #[test]
    fn runtime_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path(), Duration::from_millis(200));
        let runtime = options.paths.runtime_dir.clone();
        run_daemon(options).unwrap();
        assert_eq!(std::fs::metadata(runtime).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn connect_or_spawn_starts_a_daemon_when_none_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path(), "t");
        let options = options(dir.path(), Duration::from_secs(3));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = spawned.clone();
        let stream = runtime.block_on(connect_or_spawn(&paths, move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || run_daemon(options));
            Ok(())
        }));
        assert!(stream.is_ok());
        assert!(spawned.load(std::sync::atomic::Ordering::SeqCst));
    }
}
