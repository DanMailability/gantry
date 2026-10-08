//! The SSH install wizard's pure side: the remote probe (command and parsing), the check against this Gantry, the
//! upload path rules and the step order.

use crate::version::{Version, parse_version_output};

const MARKER: &str = "@@gantry-probe";

/// Whether the remote user's lingering is on (their systemd services outlive their last login).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linger {
    /// `Linger=yes`.
    On,
    /// `Linger=no`.
    Off,
    /// No `loginctl`, or an answer that is not understood.
    Unknown,
}

/// What one probe command found on the remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// `uname -sm`, e.g. `Linux x86_64`.
    pub os_arch: String,
    /// The remote `$HOME`.
    pub home: String,
    /// The remote Gantry's version, `None` when it is not there.
    pub version: Option<String>,
    /// The lingering state.
    pub linger: Linger,
}

/// The remote shell text of the probe; `command` is the remote Gantry, inserted raw for the remote shell to expand.
pub fn probe_command(command: &str) -> String {
    format!(
        "printf '{MARKER}\\n'; uname -sm; printf '%s\\n' \"$HOME\"; \
         v=$({command} --version 2>/dev/null | head -n 1); printf '%s\\n' \"${{v:-MISSING}}\"; \
         loginctl show-user \"$(id -un)\" -p Linger 2>/dev/null || echo LINGER_UNKNOWN"
    )
}

/// Read the probe's stdout: the four lines after the marker line (login banners before it are ignored).
pub fn parse_probe(stdout: &str) -> Result<Probe, String> {
    let mut lines = stdout.lines();
    if !lines.any(|line| line.trim_end() == MARKER) {
        return Err("could not read the remote's answer (does its shell print text at login?)".into());
    }
    let (Some(os_arch), Some(home), Some(version), Some(linger)) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err("the remote's answer was cut short".into());
    };
    let version = match version.trim() {
        "MISSING" => None,
        line => match parse_version_output(line) {
            Some(version) => Some(version.to_string()),
            None => return Err(format!("the remote Gantry's version could not be read: {line}")),
        },
    };
    let linger = match linger.trim() {
        "Linger=yes" => Linger::On,
        "Linger=no" => Linger::Off,
        _ => Linger::Unknown,
    };
    Ok(Probe { os_arch: os_arch.trim().to_string(), home: home.trim().to_string(), version, linger })
}

/// Where the remote Gantry stands against this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GantryOnRemote {
    /// Not installed at the command's path.
    Missing,
    /// An older version.
    Older(String),
    /// The same version.
    Same(String),
    /// A newer version.
    Newer(String),
}

/// What the wizard shows after the probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    /// The remote is Linux x86_64, the only target.
    pub supported: bool,
    /// The remote Gantry against this one.
    pub gantry: GantryOnRemote,
    /// The lingering state.
    pub linger: Linger,
}

/// Judge `probe` against `local_version`; an unreadable local version counts the remote as older.
pub fn check(probe: &Probe, local_version: &str) -> CheckResult {
    let gantry = match &probe.version {
        None => GantryOnRemote::Missing,
        Some(remote) => match (Version::parse(remote), Version::parse(local_version)) {
            (Some(remote_v), Some(local_v)) if remote_v == local_v => GantryOnRemote::Same(remote.clone()),
            (Some(remote_v), Some(local_v)) if remote_v > local_v => GantryOnRemote::Newer(remote.clone()),
            _ => GantryOnRemote::Older(remote.clone()),
        },
    };
    CheckResult { supported: probe.os_arch == "Linux x86_64", gantry, linger: probe.linger }
}

/// The remote directory and file of an upload, as shell text (shell-safe characters only, so unquoted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadPath {
    /// The directory to create.
    pub dir: String,
    /// The binary's path.
    pub file: String,
}

/// The upload target for a host `command`: only a plain path (`~/…` or `/…`) qualifies.
pub fn upload_path(command: &str) -> Result<UploadPath, String> {
    let plain = (command.starts_with("~/") || command.starts_with('/'))
        && !command.ends_with('/')
        && command.chars().all(|c| c.is_ascii_alphanumeric() || "._/+~@%-".contains(c));
    if !plain {
        return Err("the command is not a plain path; install Gantry on that host by hand".into());
    }
    let cut = command.rfind('/').unwrap_or(0);
    let dir = if cut == 0 { "/" } else { &command[..cut] };
    Ok(UploadPath { dir: dir.to_string(), file: command.to_string() })
}

/// A wizard step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Pick or type the host.
    Host,
    /// Probe the remote.
    Check,
    /// Upload and verify.
    Install,
    /// Lingering, save, connect.
    Finish,
}

/// The step after `step` once its work succeeded: the install is skipped when the versions match.
pub fn next_step(step: Step, check: &CheckResult) -> Step {
    match step {
        Step::Host => Step::Check,
        Step::Check if matches!(check.gantry, GantryOnRemote::Same(_)) => Step::Finish,
        Step::Check => Step::Install,
        Step::Install | Step::Finish => Step::Finish,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_is_read_from_its_marked_lines_even_after_banner_lines() {
        let out = "Welcome!\n@@gantry-probe\nLinux x86_64\n/home/ido\ngantry 0.16.0 (abc1234)\nLinger=no\n";
        let p = parse_probe(out).unwrap();
        assert_eq!(
            (p.os_arch.as_str(), p.home.as_str(), p.version.as_deref(), p.linger),
            ("Linux x86_64", "/home/ido", Some("0.16.0"), Linger::Off)
        );
    }

    #[test]
    fn a_missing_gantry_and_no_loginctl_are_read() {
        let p = parse_probe("@@gantry-probe\nLinux x86_64\n/home/ido\nMISSING\nLINGER_UNKNOWN\n").unwrap();
        assert_eq!((p.version, p.linger), (None, Linger::Unknown));
    }

    #[test]
    fn garbled_probes_are_errors() {
        for bad in ["", "no marker\n", "@@gantry-probe\nLinux x86_64\n"] {
            assert!(parse_probe(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn check_compares_versions_and_supports_only_linux_x86_64() {
        let p = |os: &str, v: Option<&str>| Probe {
            os_arch: os.into(),
            home: "/h".into(),
            version: v.map(Into::into),
            linger: Linger::On,
        };
        assert_eq!(check(&p("Linux x86_64", None), "0.17.0").gantry, GantryOnRemote::Missing);
        assert_eq!(check(&p("Linux x86_64", Some("0.16.0")), "0.17.0").gantry, GantryOnRemote::Older("0.16.0".into()));
        assert_eq!(check(&p("Linux x86_64", Some("0.17.0")), "0.17.0").gantry, GantryOnRemote::Same("0.17.0".into()));
        assert_eq!(check(&p("Linux x86_64", Some("0.18.0")), "0.17.0").gantry, GantryOnRemote::Newer("0.18.0".into()));
        assert!(!check(&p("Linux aarch64", None), "0.17.0").supported);
        assert!(!check(&p("Darwin arm64", None), "0.17.0").supported);
    }

    #[test]
    fn only_plain_paths_can_be_uploaded_to() {
        assert_eq!(
            upload_path("~/.local/bin/gantry").unwrap(),
            UploadPath { dir: "~/.local/bin".into(), file: "~/.local/bin/gantry".into() }
        );
        assert!(upload_path("/opt/gantry/bin/gantry").is_ok());
        for bad in ["gantry", "env X=1 gantry", "/opt/my gantry/g", "~/a;b", "$HOME/g", "~/a'b"] {
            assert!(upload_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn steps_skip_the_install_when_the_versions_match() {
        let c = |g| CheckResult { supported: true, gantry: g, linger: Linger::On };
        assert_eq!(next_step(Step::Check, &c(GantryOnRemote::Same("0.17.0".into()))), Step::Finish);
        assert_eq!(next_step(Step::Check, &c(GantryOnRemote::Missing)), Step::Install);
        assert_eq!(next_step(Step::Install, &c(GantryOnRemote::Missing)), Step::Finish);
    }
}
