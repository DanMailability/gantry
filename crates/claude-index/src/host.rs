//! Process and filesystem queries about the machine sessions run on (phase 2 adds remote hosts).

use std::path::Path;

/// What the index needs to know about a host.
pub trait Host {
    /// Whether process `pid` runs and — when `start` is given — started at `start` (guards against pid
    /// reuse). `None` when this host cannot tell.
    fn is_running(&self, pid: u32, start: Option<&str>) -> Option<bool>;
    /// This host's own pid namespace in Claude's `pidDomain` format; `None` when unknown.
    fn pid_domain(&self) -> Option<String>;
    /// Whether `path` is an existing directory.
    fn dir_exists(&self, path: &Path) -> bool;
}

/// The machine ccm runs on.
pub struct LocalHost {
    domain: Option<String>,
}

impl LocalHost {
    /// Query the local machine (reads its pid namespace once).
    pub fn new() -> LocalHost {
        LocalHost { domain: local_pid_domain() }
    }
}

impl Default for LocalHost {
    fn default() -> Self {
        LocalHost::new()
    }
}

impl Host for LocalHost {
    fn is_running(&self, pid: u32, start: Option<&str>) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return Some(false) };
            Some(start.is_none_or(|start| proc_start_time(&stat).as_deref() == Some(start)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (pid, start);
            None
        }
    }

    fn pid_domain(&self) -> Option<String> {
        self.domain.clone()
    }

    fn dir_exists(&self, path: &Path) -> bool {
        path.is_dir()
    }
}

#[cfg(target_os = "linux")]
fn local_pid_domain() -> Option<String> {
    let machine = std::fs::read_to_string("/etc/machine-id").ok()?;
    let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
    Some(format!("linux:{}:{}", machine.trim(), namespace.display()))
}

#[cfg(not(target_os = "linux"))]
fn local_pid_domain() -> Option<String> {
    None
}

/// Field 22 (start time) of a `/proc/<pid>/stat` line. The command name (field 2) may contain spaces and
/// parentheses, so fields are counted after its last `)`.
pub fn proc_start_time(stat: &str) -> Option<String> {
    // After the command name come fields 3, 4, …; field 22 is the 20th of them.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_field_22_even_with_odd_command_names() {
        let fields: Vec<String> = (3..=25).map(|n| n.to_string()).collect();
        let stat = format!("1234 (we ird) (name) {}", fields.join(" "));
        assert_eq!(proc_start_time(&stat).as_deref(), Some("22"));
        assert_eq!(proc_start_time("1234 (short) S 1"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_host_checks_pid_and_start_time() {
        let host = LocalHost::new();
        let me = std::process::id();
        let start = proc_start_time(&std::fs::read_to_string("/proc/self/stat").unwrap()).unwrap();
        assert_eq!(host.is_running(me, Some(&start)), Some(true));
        assert_eq!(host.is_running(me, None), Some(true));
        assert_eq!(host.is_running(me, Some("1")), Some(false));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        assert_eq!(host.is_running(gone, None), Some(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_pid_domain_has_claudes_format() {
        let domain = LocalHost::new().pid_domain().unwrap();
        assert!(domain.starts_with("linux:") && domain.contains(":pid:["), "{domain}");
    }
}
