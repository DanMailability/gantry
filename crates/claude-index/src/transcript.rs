//! Summaries of Claude transcripts (`projects/<folder>/<session id>.jsonl`), read from the file's head
//! (directory, first prompt) and tail (latest titles) only — transcripts can be hundreds of megabytes.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Bytes read from the start of a transcript looking for its directory and first prompt.
const HEAD_LIMIT: u64 = 1 << 20;
/// Bytes read from the end looking for titles; Claude re-appends its title records as it writes.
const TAIL_LIMIT: u64 = 256 << 10;
/// Longest title kept, in characters.
const MAX_TITLE_CHARS: usize = 120;

/// What ccm shows for one transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptSummary {
    /// Session id (the file name without `.jsonl`).
    pub session_id: String,
    /// Directory the session ran in, read from inside the transcript.
    pub cwd: PathBuf,
    /// Name the user gave the session (newest `custom-title` / `agent-name`).
    pub name: Option<String>,
    /// Title Claude generated (newest `ai-title`).
    pub ai_title: Option<String>,
    /// First line of the first prompt the user typed.
    pub first_prompt: Option<String>,
}

impl TranscriptSummary {
    /// Name, else Claude's title, else the first prompt.
    pub fn title(&self) -> String {
        self.name.clone().or_else(|| self.ai_title.clone()).or_else(|| self.first_prompt.clone()).unwrap_or_default()
    }
}

/// Summarize the transcript at `path`. `Ok(None)` for transcripts without a directory or with nothing to
/// show (no prompt and no title) — sessions that never started.
pub fn summarize(path: &Path) -> io::Result<Option<TranscriptSummary>> {
    let Some(session_id) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { return Ok(None) };
    let mut file = File::open(path)?;
    let (cwd, first_prompt) = read_head(&mut file)?;
    let (name, ai_title) = read_tail(&mut file)?;
    let Some(cwd) = cwd else { return Ok(None) };
    if first_prompt.is_none() && name.is_none() && ai_title.is_none() {
        return Ok(None);
    }
    Ok(Some(TranscriptSummary { session_id, cwd, name, ai_title, first_prompt }))
}

/// The first `cwd` and the first user prompt within `HEAD_LIMIT` bytes. Unparseable lines are skipped.
fn read_head(file: &mut File) -> io::Result<(Option<PathBuf>, Option<String>)> {
    let mut reader = BufReader::new(file.by_ref().take(HEAD_LIMIT));
    let (mut cwd, mut prompt) = (None, None);
    let mut line = Vec::new();
    while cwd.is_none() || prompt.is_none() {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line) else { continue };
        if cwd.is_none() {
            cwd = record.get("cwd").and_then(Value::as_str).filter(|cwd| !cwd.is_empty()).map(PathBuf::from);
        }
        if prompt.is_none() {
            prompt = user_prompt(&record);
        }
    }
    Ok((cwd, prompt))
}

/// The newest name and AI title in the last `TAIL_LIMIT` bytes. Only lines mentioning a title record
/// are parsed; a partial first line (the window starts mid-line) and a half-written last line fail to
/// parse and are skipped.
fn read_tail(file: &mut File) -> io::Result<(Option<String>, Option<String>)> {
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_LIMIT)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let (mut name, mut ai_title) = (None, None);
    for line in tail.split(|&byte| byte == b'\n') {
        if !mentions(line, b"-title\"") && !mentions(line, b"\"agent-name\"") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else { continue };
        let text = |key: &str| record.get(key).and_then(Value::as_str).map(clean).filter(|text| !text.is_empty());
        match record.get("type").and_then(Value::as_str) {
            Some("custom-title") => name = text("customTitle").or(name),
            Some("agent-name") => name = text("agentName").or(name),
            Some("ai-title") => ai_title = text("aiTitle").or(ai_title),
            _ => {}
        }
    }
    Ok((name, ai_title))
}

fn mentions(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len()).any(|window| window == needle)
}

/// First line of a prompt the user typed. Skips meta records, sidechains, and wrapped command output
/// (text starting with `<`).
fn user_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if ["isMeta", "isSidechain"].iter().any(|key| record.get(*key).and_then(Value::as_bool) == Some(true)) {
        return None;
    }
    let text = match record.get("message")?.get("content")? {
        Value::String(text) => text.as_str(),
        Value::Array(blocks) => {
            blocks.iter().find(|block| block.get("type").and_then(Value::as_str) == Some("text"))?.get("text")?.as_str()?
        }
        _ => return None,
    };
    let text = text.trim_start();
    if text.starts_with('<') {
        return None;
    }
    Some(clean(text.lines().next()?)).filter(|line| !line.is_empty())
}

/// Trimmed, without control characters, at most `MAX_TITLE_CHARS` characters.
fn clean(text: &str) -> String {
    text.trim().chars().filter(|c| !c.is_control()).take(MAX_TITLE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, lines: &[String]) -> PathBuf {
        let path = dir.join("0f3c-session.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn user(text: &str) -> String {
        serde_json::json!({"type": "user", "cwd": "/repo", "message": {"role": "user", "content": text}}).to_string()
    }

    fn record(kind: &str, key: &str, value: &str) -> String {
        serde_json::json!({"type": kind, key: value, "sessionId": "0f3c-session"}).to_string()
    }

    #[test]
    fn summary_reads_directory_prompt_and_titles() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[
            r#"{"type":"mode","mode":"normal"}"#.into(),
            serde_json::json!({"type": "attachment", "cwd": "/home/me/repo"}).to_string(),
            user("Fix the Jenkins lib\nsecond line"),
            record("ai-title", "aiTitle", "Jenkins library fix"),
        ]);
        let summary = summarize(&path).unwrap().unwrap();
        assert_eq!(summary.session_id, "0f3c-session");
        assert_eq!(summary.cwd, PathBuf::from("/home/me/repo"));
        assert_eq!(summary.first_prompt.as_deref(), Some("Fix the Jenkins lib"));
        assert_eq!(summary.ai_title.as_deref(), Some("Jenkins library fix"));
        assert_eq!(summary.name, None);
    }

    #[test]
    fn title_prefers_name_then_ai_title_then_prompt() {
        let mut summary = TranscriptSummary {
            session_id: "s".into(),
            cwd: "/".into(),
            name: Some("mine".into()),
            ai_title: Some("ai".into()),
            first_prompt: Some("prompt".into()),
        };
        assert_eq!(summary.title(), "mine");
        summary.name = None;
        assert_eq!(summary.title(), "ai");
        summary.ai_title = None;
        assert_eq!(summary.title(), "prompt");
    }

    #[test]
    fn newest_title_records_win() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[
            user("hello"),
            record("ai-title", "aiTitle", "old title"),
            record("custom-title", "customTitle", "First name"),
            record("ai-title", "aiTitle", "new title"),
            record("agent-name", "agentName", "Second name"),
        ]);
        let summary = summarize(&path).unwrap().unwrap();
        assert_eq!((summary.name.as_deref(), summary.ai_title.as_deref()), (Some("Second name"), Some("new title")));
    }

    #[test]
    fn meta_records_and_command_wrappers_are_not_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[
            serde_json::json!({"type": "user", "isMeta": true, "cwd": "/r", "message": {"content": "Caveat: meta"}}).to_string(),
            user("<command-name>/clear</command-name>"),
            serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "x"}]}}).to_string(),
            serde_json::json!({"type": "user", "message": {"content": [{"type": "text", "text": "  Real prompt  "}]}}).to_string(),
        ]);
        assert_eq!(summarize(&path).unwrap().unwrap().first_prompt.as_deref(), Some("Real prompt"));
    }

    #[test]
    fn transcripts_without_a_directory_or_content_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let no_cwd = write(dir.path(), &[serde_json::json!({"type": "user", "message": {"content": "hi"}}).to_string()]);
        assert_eq!(summarize(&no_cwd).unwrap(), None);
        let empty = write(dir.path(), &[serde_json::json!({"type": "attachment", "cwd": "/r"}).to_string()]);
        assert_eq!(summarize(&empty).unwrap(), None);
    }

    #[test]
    fn garbage_and_a_half_written_last_line_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let huge = format!(r#"{{"type":"assistant","text":"{}"}}"#, "x".repeat(3 << 20));
        let content = [
            "not json at all".to_string(),
            "\u{0}\u{1}binary".to_string(),
            user("the prompt"),
            huge,
            record("ai-title", "aiTitle", "kept"),
            r#"{"type":"ai-title","aiTitle":"half wri"#.to_string(),
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();
        let summary = summarize(&path).unwrap().unwrap();
        assert_eq!(summary.first_prompt.as_deref(), Some("the prompt"));
        assert_eq!(summary.ai_title.as_deref(), Some("kept"));
    }

    #[test]
    fn titles_are_found_at_the_end_of_large_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let filler: Vec<String> = (0..2000).map(|i| format!(r#"{{"type":"assistant","n":{i},"pad":"{}"}}"#, "y".repeat(200))).collect();
        let mut lines = vec![user("start"), record("ai-title", "aiTitle", "early only")];
        lines.extend(filler);
        lines.push(record("custom-title", "customTitle", "Late name"));
        let summary = summarize(&write(dir.path(), &lines)).unwrap().unwrap();
        assert_eq!(summary.name.as_deref(), Some("Late name"));
        // A title record only in the head of a large file is outside the tail window.
        assert_eq!(summary.ai_title, None);
        assert_eq!(summary.title(), "Late name");
    }

    #[test]
    fn titles_drop_control_characters_and_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let long = "é".repeat(500);
        let path = write(dir.path(), &[user(&format!("a\u{7}b{long}")), record("ai-title", "aiTitle", "x\u{1b}[31my")]);
        let summary = summarize(&path).unwrap().unwrap();
        assert_eq!(summary.ai_title.as_deref(), Some("x[31my"));
        assert_eq!(summary.first_prompt.unwrap().chars().count(), MAX_TITLE_CHARS);
    }
}
