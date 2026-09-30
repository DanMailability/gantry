//! Resolve the user's login-shell environment (so a GUI started from a desktop launcher still
//! gets variables set in shell profiles, e.g. `CLAUDE_CONFIG_DIR`).

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Printed right before the environment JSON so shell noise can be skipped.
pub const ENV_MARKER: &str = "__CCM_ENV__";

/// Current process environment as a JSON object (what `ccm internal-print-env` prints).
pub fn print_env_json() -> String {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    serde_json::to_string(&env).expect("string map serializes")
}

/// Extract the environment printed after [`ENV_MARKER`].
pub fn parse_env_output(stdout: &str) -> Option<Vec<(String, String)>> {
    let (_, json) = stdout.split_once(ENV_MARKER)?;
    let env: BTreeMap<String, String> = serde_json::from_str(json.trim()).ok()?;
    Some(env.into_iter().collect())
}

/// Quote `text` for a POSIX shell.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Run `shell -l -i -c "<marker>; <print_command>"` and parse its output; `None` on failure or timeout.
pub fn resolve_login_env(shell: &str, print_command: &str, timeout: Duration) -> Option<Vec<(String, String)>> {
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &format!("printf '%s' {ENV_MARKER}; {print_command}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let result = rx.recv_timeout(timeout).ok().and_then(|text| parse_env_output(&text));
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_after_marker_ignoring_shell_noise() {
        let stdout = format!("Welcome to fish!\nlast login: …\n{ENV_MARKER}{{\"A\":\"1\",\"B\":\"x y\"}}\n");
        let mut env = parse_env_output(&stdout).unwrap();
        env.sort();
        assert_eq!(env, vec![("A".into(), "1".into()), ("B".into(), "x y".into())]);
        assert_eq!(parse_env_output("no marker here"), None);
        assert_eq!(parse_env_output(&format!("{ENV_MARKER}not json")), None);
    }

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(shell_quote("/a b/it's"), r#"'/a b/it'\''s'"#);
    }

    #[test]
    fn print_env_json_round_trips() {
        let json = print_env_json();
        let env = parse_env_output(&format!("{ENV_MARKER}{json}")).unwrap();
        assert!(env.iter().any(|(k, _)| k == "PATH"));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_env_through_a_login_shell() {
        let env = resolve_login_env("/bin/sh", r#"printf '{"CCM_T":"yes"}'"#, Duration::from_secs(5)).unwrap();
        assert_eq!(env, vec![("CCM_T".into(), "yes".into())]);
    }

    #[cfg(unix)]
    #[test]
    fn hanging_shell_times_out() {
        let started = std::time::Instant::now();
        assert_eq!(resolve_login_env("/bin/sh", "sleep 10", Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
