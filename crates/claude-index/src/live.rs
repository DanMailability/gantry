//! Live Claude instances: `sessions/<pid>.json`, written by each running Claude and removed when it exits.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// What a live Claude reports doing (`status` in its pid file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Working on a turn, or idle with a background shell running (`status` `shell`).
    Busy,
    /// Waiting for the next prompt.
    Idle,
    /// Blocked on the user: a permission prompt, a dialog, a question.
    Waiting {
        /// Claude's `waitingFor`, e.g. `input needed`, `dialog open`.
        reason: Option<String>,
    },
}

/// One running Claude, as its pid file describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRecord {
    /// Process id (in the namespace named by `pid_domain`).
    pub pid: u32,
    /// Session it is running.
    pub session_id: String,
    /// Directory it runs in.
    pub cwd: PathBuf,
    /// Process start time as Claude recorded it (Linux: `/proc/<pid>/stat` field 22).
    pub proc_start: Option<String>,
    /// Pid namespace of `pid`, e.g. `linux:<machine id>:pid:[4026531836]`.
    pub pid_domain: Option<String>,
    /// Last update, milliseconds since the Unix epoch.
    pub updated_at_ms: Option<u64>,
    /// What it reports doing; `None` when the file has no (known) status.
    pub activity: Option<Activity>,
    /// When the status was last written (`statusUpdatedAt`, else `updatedAt`), ms since the Unix epoch.
    pub activity_since_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PidFile {
    pid: Option<u32>,
    session_id: Option<String>,
    cwd: Option<String>,
    proc_start: Option<Value>,
    pid_domain: Option<String>,
    updated_at: Option<Value>,
    status: Option<Value>,
    waiting_for: Option<Value>,
    status_updated_at: Option<Value>,
}

/// Parse one pid file; `None` unless it names a pid, a session and a directory.
pub fn parse(text: &str) -> Option<LiveRecord> {
    let file: PidFile = serde_json::from_str(text).ok()?;
    let proc_start = file.proc_start.and_then(|value| match value {
        Value::String(text) => Some(text),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    });
    let activity = match file.status.as_ref().and_then(Value::as_str) {
        Some("busy" | "shell") => Some(Activity::Busy),
        Some("idle") => Some(Activity::Idle),
        Some("waiting") => {
            Some(Activity::Waiting { reason: file.waiting_for.as_ref().and_then(Value::as_str).map(String::from) })
        }
        _ => None,
    };
    // Timestamps of another type mean unknown; they never drop the file.
    let updated_at = file.updated_at.as_ref().and_then(Value::as_u64);
    Some(LiveRecord {
        pid: file.pid?,
        session_id: file.session_id.filter(|id| !id.is_empty())?,
        cwd: PathBuf::from(file.cwd?),
        proc_start,
        pid_domain: file.pid_domain,
        updated_at_ms: updated_at,
        activity,
        activity_since_ms: file.status_updated_at.as_ref().and_then(Value::as_u64).or(updated_at),
    })
}

/// Every parseable `*.json` pid file in `dir` (missing directory = none). Others are logged and skipped.
pub fn read_all(dir: &Path) -> Vec<LiveRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut records = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        match std::fs::read_to_string(&path).ok().as_deref().and_then(parse) {
            Some(record) => records.push(record),
            None => tracing::debug!("skipping unreadable pid file {}", path.display()),
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_SHAPE: &str = r#"{"pid":3394468,"sessionId":"9835b878-ce17","cwd":"/home/me/repo","startedAt":1790771770444,
        "procStart":"64344299","version":"2.1.285","kind":"interactive","pidDomain":"linux:9dec:pid:[4026531836]",
        "name":"repo-ff","nameSource":"derived","status":"busy","updatedAt":1790840985757,"unknownField":[1,2]}"#;

    #[test]
    fn the_observed_pid_file_shape_parses() {
        let record = parse(REAL_SHAPE).unwrap();
        assert_eq!(
            record,
            LiveRecord {
                pid: 3394468,
                session_id: "9835b878-ce17".into(),
                cwd: "/home/me/repo".into(),
                proc_start: Some("64344299".into()),
                pid_domain: Some("linux:9dec:pid:[4026531836]".into()),
                updated_at_ms: Some(1790840985757),
                activity: Some(Activity::Busy),
                // No `statusUpdatedAt` in this shape: `updatedAt` stands in.
                activity_since_ms: Some(1790840985757),
            }
        );
    }

    #[test]
    fn status_values_map_to_activities() {
        let with = |extra: &str| parse(&format!(r#"{{"pid":1,"sessionId":"s","cwd":"/",{extra}}}"#)).unwrap();
        assert_eq!(with(r#""status":"shell""#).activity, Some(Activity::Busy));
        let idle = with(r#""status":"idle","statusUpdatedAt":5,"updatedAt":9"#);
        assert_eq!((idle.activity, idle.activity_since_ms), (Some(Activity::Idle), Some(5)));
        let waiting = with(r#""status":"waiting","waitingFor":"input needed""#);
        assert_eq!(waiting.activity, Some(Activity::Waiting { reason: Some("input needed".into()) }));
        assert_eq!(with(r#""status":"waiting""#).activity, Some(Activity::Waiting { reason: None }));
    }

    #[test]
    fn odd_status_values_mean_unknown() {
        let with = |extra: &str| parse(&format!(r#"{{"pid":1,"sessionId":"s","cwd":"/"{extra}}}"#));
        for extra in [
            "",
            r#","status":"paused""#,
            r#","status":7"#,
            r#","status":"waiting","waitingFor":3"#,
            r#","statusUpdatedAt":"2026-10-01T10:00:00Z""#,
            r#","updatedAt":1.5,"statusUpdatedAt":-3"#,
        ] {
            let record = with(extra).unwrap_or_else(|| panic!("dropped a pid file with {extra:?}"));
            let expected = if extra.contains("waiting") { Some(Activity::Waiting { reason: None }) } else { None };
            assert_eq!(record.activity, expected, "{extra}");
        }
    }

    #[test]
    fn numeric_start_times_are_accepted_and_required_fields_enforced() {
        let record = parse(r#"{"pid":5,"sessionId":"s","cwd":"/","procStart":123}"#).unwrap();
        assert_eq!(record.proc_start.as_deref(), Some("123"));
        assert_eq!(parse(r#"{"pid":5,"cwd":"/"}"#), None);
        assert_eq!(parse(r#"{"sessionId":"s","cwd":"/"}"#), None);
        assert_eq!(parse("not json"), None);
    }

    #[test]
    fn read_all_skips_other_files_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.json"), r#"{"pid":1,"sessionId":"a","cwd":"/"}"#).unwrap();
        std::fs::write(dir.path().join("2.json"), "{garbage").unwrap();
        std::fs::write(dir.path().join("1.abc.key"), "secret").unwrap();
        let records = read_all(dir.path());
        assert_eq!(records.iter().map(|r| r.session_id.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert!(read_all(&dir.path().join("missing")).is_empty());
    }
}
