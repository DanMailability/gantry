//! Per-profile locations of the daemon socket, lock file, and logs.

use std::io;
use std::path::{Path, PathBuf};

use interprocess::local_socket::Name;
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};

/// Profile name from `CCM_PROFILE` (if non-empty), else `dev` for debug builds and `default` otherwise.
pub fn profile_from(env_value: Option<String>, debug_build: bool) -> String {
    match env_value {
        Some(value) if !value.is_empty() => value,
        _ if debug_build => "dev".into(),
        _ => "default".into(),
    }
}

/// Profile of this process.
pub fn current_profile() -> String {
    profile_from(std::env::var("CCM_PROFILE").ok(), cfg!(debug_assertions))
}

#[derive(Clone, Debug)]
enum SocketLocation {
    #[cfg_attr(windows, allow(dead_code))]
    File(PathBuf),
    #[cfg_attr(unix, allow(dead_code))]
    Pipe(String),
}

/// Everything the daemon and GUI need to find each other for one profile.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Profile name.
    pub profile: String,
    /// Private (0700 on Unix) directory holding the socket and lock.
    pub runtime_dir: PathBuf,
    /// Lock held by the running daemon.
    pub lock_file: PathBuf,
    /// Daemon log directory.
    pub log_dir: PathBuf,
    /// Saved window layout (GUI-owned).
    pub layout_file: PathBuf,
    socket: SocketLocation,
}

impl Paths {
    /// Standard locations for `profile`.
    pub fn for_profile(profile: &str) -> Paths {
        let data = directories::ProjectDirs::from("", "", "ccm")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("ccm-data"));
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
        #[cfg(unix)]
        let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => PathBuf::from(dir).join(format!("ccm-{profile}")),
            None => std::env::temp_dir().join(format!("ccm-{user}-{profile}")),
        };
        #[cfg(windows)]
        let runtime_dir = data.join(profile).join("run");
        Paths::build(profile, runtime_dir, data.join(profile).join("logs"), data.join(profile).join("layout.json"), &user)
    }

    /// Locations entirely inside `dir` (for tests).
    pub fn in_dir(dir: &Path, profile: &str) -> Paths {
        let unique = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Paths::build(profile, dir.join("run"), dir.join("logs"), dir.join("layout.json"), &unique)
    }

    #[cfg_attr(unix, allow(unused_variables))]
    fn build(profile: &str, runtime_dir: PathBuf, log_dir: PathBuf, layout_file: PathBuf, unique: &str) -> Paths {
        #[cfg(unix)]
        let socket = SocketLocation::File(runtime_dir.join("daemon.sock"));
        #[cfg(windows)]
        let socket = SocketLocation::Pipe(format!("ccm-{profile}-{unique}"));
        Paths { profile: profile.into(), lock_file: runtime_dir.join("daemon.lock"), runtime_dir, log_dir, layout_file, socket }
    }

    /// Name to bind or connect to.
    pub fn socket_name(&self) -> io::Result<Name<'static>> {
        match &self.socket {
            #[cfg(unix)]
            SocketLocation::File(path) => path.clone().to_fs_name::<GenericFilePath>(),
            #[cfg(windows)]
            SocketLocation::Pipe(name) => name.clone().to_ns_name::<GenericNamespaced>(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("socket kind matches the platform"),
        }
    }

    /// Socket file path, on platforms where the socket is a file.
    pub fn socket_file(&self) -> Option<&Path> {
        match &self.socket {
            SocketLocation::File(path) => Some(path),
            SocketLocation::Pipe(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_selection() {
        assert_eq!(profile_from(Some("work".into()), true), "work");
        assert_eq!(profile_from(None, true), "dev");
        assert_eq!(profile_from(None, false), "default");
        assert_eq!(profile_from(Some(String::new()), false), "default");
    }

    #[test]
    fn test_paths_stay_inside_the_given_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path(), "t");
        for path in [&paths.runtime_dir, &paths.lock_file, &paths.log_dir, &paths.layout_file] {
            assert!(path.starts_with(dir.path()), "{path:?}");
        }
        #[cfg(unix)]
        assert!(paths.socket_file().unwrap().starts_with(dir.path()));
        assert!(paths.socket_name().is_ok());
    }

    #[test]
    fn profiles_do_not_share_locations() {
        let (a, b) = (Paths::for_profile("a"), Paths::for_profile("b"));
        assert_ne!(a.lock_file, b.lock_file);
        assert_ne!(a.log_dir, b.log_dir);
        assert_ne!(a.layout_file, b.layout_file);
    }
}
