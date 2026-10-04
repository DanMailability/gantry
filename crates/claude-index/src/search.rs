//! The text of Claude transcripts for conversation search: what you typed and Claude's text replies, read from a
//! byte offset up to the last complete line. Tool calls, tool output, thinking and injected text are left out.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use serde_json::Value;

const REMINDER_OPEN: &str = "<system-reminder>";
const REMINDER_CLOSE: &str = "</system-reminder>";

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    /// The user.
    You,
    /// Claude.
    Claude,
}

/// One searchable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Who wrote it.
    pub speaker: Speaker,
    /// Its text blocks, trimmed, joined with blank lines.
    pub text: String,
}

/// Messages in `path` from byte `offset` up to its last complete line, and the offset just after that line. A last
/// line without `\n` is left for a later read; an `offset` past the end reads nothing.
pub fn read_messages(path: &Path, offset: u64) -> io::Result<(Vec<Message>, u64)> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let (mut messages, mut end) = (Vec::new(), offset);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        end += read as u64;
        if let Ok(record) = serde_json::from_slice::<Value>(&line) {
            messages.extend(message(&record));
        }
    }
    Ok((messages, end))
}

/// The searchable text of one record, if it is a message you typed or a reply Claude wrote.
fn message(record: &Value) -> Option<Message> {
    let speaker = match record.get("type").and_then(Value::as_str)? {
        "user" => Speaker::You,
        "assistant" => Speaker::Claude,
        _ => return None,
    };
    if ["isMeta", "isSidechain"].iter().any(|key| record.get(*key).and_then(Value::as_bool) == Some(true)) {
        return None;
    }
    let parts: Vec<&str> = match record.get("message")?.get("content")? {
        Value::String(text) => vec![text.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text")?.as_str())
            .collect(),
        _ => return None,
    };
    let text = parts
        .into_iter()
        // Command wrappers and caveats Claude Code injects as user text start with a tag.
        .filter(|part| speaker == Speaker::Claude || !part.trim_start().starts_with('<'))
        .map(without_reminders)
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(Message { speaker, text })
}

/// `text` without its `<system-reminder>…</system-reminder>` spans (an unclosed one runs to the end).
fn without_reminders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(REMINDER_OPEN) {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find(REMINDER_CLOSE) {
            Some(end) => &rest[start + end + REMINDER_CLOSE.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write(dir: &Path, lines: &[serde_json::Value]) -> PathBuf {
        let path = dir.join("s1.jsonl");
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn you(content: serde_json::Value) -> serde_json::Value {
        json!({"type": "user", "cwd": "/r", "message": {"role": "user", "content": content}})
    }

    fn claude(blocks: serde_json::Value) -> serde_json::Value {
        json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}})
    }

    fn texts(messages: &[Message]) -> Vec<(Speaker, &str)> {
        messages.iter().map(|m| (m.speaker, m.text.as_str())).collect()
    }

    #[test]
    fn keeps_your_text_and_claudes_replies() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            &[
                you(json!("fix the font fallback")),
                claude(json!([{"type": "text", "text": "Done: Noto Sans Mono."}])),
                you(json!([{"type": "text", "text": "and on Ubuntu?"}])),
            ],
        );
        let (messages, end) = read_messages(&path, 0).unwrap();
        assert_eq!(
            texts(&messages),
            vec![
                (Speaker::You, "fix the font fallback"),
                (Speaker::Claude, "Done: Noto Sans Mono."),
                (Speaker::You, "and on Ubuntu?"),
            ]
        );
        assert_eq!(end, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn drops_tool_calls_output_thinking_and_injected_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            &[
                you(json!([{"type": "tool_result", "tool_use_id": "t", "content": "secret output"}])),
                claude(json!([
                    {"type": "thinking", "thinking": "private thought"},
                    {"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}},
                    {"type": "text", "text": "visible reply"}
                ])),
                json!({"type": "user", "isMeta": true, "message": {"content": "meta caveat"}}),
                json!({"type": "user", "isSidechain": true, "message": {"content": "side chain"}}),
                you(json!("<command-name>/clear</command-name>")),
                you(json!("before <system-reminder>injected</system-reminder> after")),
                json!({"type": "summary", "summary": "not a message"}),
                json!({"type": "ai-title", "aiTitle": "title"}),
            ],
        );
        let (messages, _) = read_messages(&path, 0).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::Claude, "visible reply"), (Speaker::You, "before  after")]);
    }

    #[test]
    fn a_reply_starting_with_a_tag_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[claude(json!([{"type": "text", "text": "<div> is the wrapper"}]))]);
        let (messages, _) = read_messages(&path, 0).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::Claude, "<div> is the wrapper")]);
    }

    #[test]
    fn reads_from_an_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[you(json!("first"))]);
        let (_, end) = read_messages(&path, 0).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", you(json!("second"))).unwrap();
        let (messages, next) = read_messages(&path, end).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::You, "second")]);
        assert_eq!(next, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn a_half_written_last_line_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[you(json!("whole"))]);
        let whole = std::fs::metadata(&path).unwrap().len();
        let line = you(json!("later")).to_string();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{}", &line[..10]).unwrap();
        let (messages, end) = read_messages(&path, 0).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::You, "whole")]);
        assert_eq!(end, whole);
        writeln!(file, "{}", &line[10..]).unwrap();
        let (messages, _) = read_messages(&path, end).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::You, "later")]);
    }

    #[test]
    fn unparseable_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(&path, format!("not json\n{}\n", you(json!("kept")))).unwrap();
        let (messages, _) = read_messages(&path, 0).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::You, "kept")]);
    }

    #[test]
    fn a_huge_tool_output_line_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(4 << 20);
        let path = write(
            dir.path(),
            &[you(json!([{"type": "tool_result", "tool_use_id": "t", "content": big}])), you(json!("after it"))],
        );
        let (messages, _) = read_messages(&path, 0).unwrap();
        assert_eq!(texts(&messages), vec![(Speaker::You, "after it")]);
    }

    #[test]
    fn an_offset_past_the_end_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &[you(json!("one"))]);
        assert_eq!(read_messages(&path, 10_000).unwrap(), (Vec::new(), 10_000));
    }
}
