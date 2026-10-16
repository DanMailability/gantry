//! GitHub releases: which published releases are newer than this Gantry, and the release notes as drawable blocks.

use crate::version::Version;

/// Release links Gantry may open.
pub const URL_PREFIX: &str = "https://github.com/idogrf/gantry/";

/// Most characters of one release's notes that are kept.
pub const NOTES_CAP: usize = 20_000;

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The version in its `vMAJOR.MINOR.PATCH` tag.
    pub version: Version,
    /// Publication day, `YYYY-MM-DD` (empty when GitHub sent none).
    pub date: String,
    /// The release notes, cut at [`NOTES_CAP`] characters.
    pub notes: String,
    /// The release page; empty when it is not under [`URL_PREFIX`].
    pub url: String,
}

/// A newer release than this window runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Online {
    /// The newest release.
    pub latest: Version,
    /// Every release newer than the running version, newest first.
    pub newer: Vec<Release>,
}

/// One drawable piece of release notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotesBlock {
    /// A `#`, `##` or `###` line.
    Heading(String),
    /// A `- ` or `* ` line.
    Bullet(String),
    /// Any other non-empty line.
    Line(String),
    /// A blank line.
    Gap,
}

/// What the Settings "Check now" row says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// Nothing asked yet.
    Idle,
    /// A manual check is running.
    Checking,
    /// The newest release is not newer than this window.
    UpToDate,
    /// A newer release exists.
    Available(Version),
    /// GitHub did not answer.
    Failed,
}

/// The status after a good answer: [`CheckStatus::Available`] when some release is higher than `running`.
pub fn check_status(running: Version, releases: &[Release]) -> CheckStatus {
    match releases.iter().map(|release| release.version).max() {
        Some(latest) if latest > running => CheckStatus::Available(latest),
        _ => CheckStatus::UpToDate,
    }
}

impl CheckStatus {
    /// The line in the row; `running` is this window's version.
    pub fn line(&self, running: Version) -> String {
        match self {
            CheckStatus::Idle => format!("Gantry {running}"),
            CheckStatus::Checking => "Checking…".into(),
            CheckStatus::UpToDate => format!("Gantry {running} is up to date."),
            CheckStatus::Available(latest) => format!("Gantry {latest} is available."),
            CheckStatus::Failed => "Couldn't reach GitHub.".into(),
        }
    }
}

/// The published releases in a GitHub releases list, newest first; empty when the text is not such a list.
pub fn parse_releases(json: &str) -> Vec<Release> {
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(json) else { return Vec::new() };
    let mut releases: Vec<Release> = entries.iter().filter_map(parse_release).collect();
    releases.sort_by_key(|release| std::cmp::Reverse(release.version));
    releases
}

fn parse_release(entry: &serde_json::Value) -> Option<Release> {
    let flag = |key: &str| entry.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false);
    let text = |key: &str| entry.get(key).and_then(serde_json::Value::as_str).unwrap_or_default();
    if flag("draft") || flag("prerelease") {
        return None;
    }
    let version = Version::parse(entry.get("tag_name")?.as_str()?.strip_prefix('v')?)?;
    let url = text("html_url");
    Some(Release {
        version,
        date: text("published_at").chars().take(10).collect(),
        notes: text("body").chars().take(NOTES_CAP).collect(),
        url: if url.starts_with(URL_PREFIX) { url.to_owned() } else { String::new() },
    })
}

/// The online notice: `Some` when the newest release is higher than both `running` and `installed`.
pub fn online(running: Version, installed: Option<Version>, releases: &[Release]) -> Option<Online> {
    let latest = releases.iter().map(|release| release.version).max()?;
    if latest <= installed.map_or(running, |installed| installed.max(running)) {
        return None;
    }
    let mut newer: Vec<Release> = releases.iter().filter(|release| release.version > running).cloned().collect();
    newer.sort_by_key(|release| std::cmp::Reverse(release.version));
    Some(Online { latest, newer })
}

impl Online {
    /// The title-bar pill.
    pub fn pill(&self) -> String {
        format!("↑ Update to {}", self.latest)
    }
}

/// `body` as blocks: `#` lines are headings, `- ` and `* ` lines bullets, blank lines gaps, the rest plain lines.
pub fn notes_blocks(body: &str) -> Vec<NotesBlock> {
    body.lines()
        .map(|line| {
            let line = line.trim_end();
            if line.trim().is_empty() {
                NotesBlock::Gap
            } else if line.starts_with('#') {
                NotesBlock::Heading(line.trim_start_matches('#').trim().to_owned())
            } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                NotesBlock::Bullet(item.trim().to_owned())
            } else {
                NotesBlock::Line(line.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release(version: &str) -> Release {
        Release { version: v(version), date: String::new(), notes: String::new(), url: String::new() }
    }

    fn entry(tag: &str, body: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"published_at":"2026-10-12T13:07:54Z","html_url":"{URL_PREFIX}releases/tag/{tag}","body":{body}}}"#
        )
    }

    #[test]
    fn parse_keeps_published_semver_tags_newest_first() {
        let fixture = include_str!("../tests/fixtures/releases.json");
        let versions: Vec<_> = parse_releases(fixture).iter().map(|release| release.version).collect();
        assert_eq!(versions, vec![v("0.6.2")]);
        let list = format!("[{},{},{}]", entry("v0.6.0", "null"), entry("v0.7.0", "null"), entry("v0.6.2", "null"));
        let versions: Vec<_> = parse_releases(&list).iter().map(|release| release.version).collect();
        assert_eq!(versions, vec![v("0.7.0"), v("0.6.2"), v("0.6.0")]);
    }

    #[test]
    fn parse_fills_date_and_url() {
        let list = format!("[{}]", entry("v0.7.0", r#""notes""#));
        let parsed = parse_releases(&list);
        assert_eq!(parsed[0].date, "2026-10-12");
        assert_eq!(parsed[0].url, format!("{URL_PREFIX}releases/tag/v0.7.0"));
        assert_eq!(parsed[0].notes, "notes");
    }

    #[test]
    fn parse_survives_garbage() {
        for bad in ["", "{}", r#"[{"tag_name":1}]"#, "[1,2]", "not json"] {
            assert!(parse_releases(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn null_body_is_empty_notes() {
        let list = format!("[{}]", entry("v0.7.0", "null"));
        assert_eq!(parse_releases(&list)[0].notes, "");
    }

    #[test]
    fn notes_are_capped_at_20000_chars() {
        let body = format!("\"{}\"", "é".repeat(100_000));
        let notes = parse_releases(&format!("[{}]", entry("v0.7.0", &body))).remove(0).notes;
        assert_eq!(notes.chars().count(), NOTES_CAP);
    }

    #[test]
    fn foreign_url_is_blanked() {
        let list = r#"[{"tag_name":"v0.7.0","draft":false,"prerelease":false,"html_url":"https://evil.example/x"}]"#;
        assert_eq!(parse_releases(list)[0].url, "");
    }

    #[test]
    fn draft_prerelease_and_odd_tags_are_dropped() {
        let list = r#"[
            {"tag_name":"v0.9.0","draft":true,"prerelease":false},
            {"tag_name":"v0.8.0","draft":false,"prerelease":true},
            {"tag_name":"v0.7.0-rc1","draft":false,"prerelease":false},
            {"tag_name":"nightly","draft":false,"prerelease":false},
            {"tag_name":"0.7.0","draft":false,"prerelease":false}]"#;
        assert!(parse_releases(list).is_empty());
    }

    #[test]
    fn online_none_when_running_is_newer() {
        assert_eq!(online(v("0.9.0"), None, &[release("0.8.0")]), None);
        assert_eq!(online(v("0.8.0"), None, &[release("0.8.0")]), None);
        assert_eq!(online(v("0.8.0"), None, &[]), None);
    }

    #[test]
    fn online_none_when_installed_reaches_latest() {
        assert_eq!(online(v("0.6.2"), Some(v("0.7.0")), &[release("0.7.0")]), None);
    }

    #[test]
    fn online_lists_every_newer_release() {
        let releases = [release("0.7.1"), release("0.7.0"), release("0.6.2")];
        let found = online(v("0.6.2"), None, &releases).unwrap();
        assert_eq!(found.latest, v("0.7.1"));
        assert_eq!(found.newer, vec![release("0.7.1"), release("0.7.0")]);
        assert_eq!(found.pill(), "↑ Update to 0.7.1");
    }

    #[test]
    fn check_status_compares_the_newest_release_with_the_running_version() {
        let releases = [release("0.7.0"), release("0.6.2")];
        assert_eq!(check_status(v("0.6.2"), &releases), CheckStatus::Available(v("0.7.0")));
        assert_eq!(check_status(v("0.7.0"), &releases), CheckStatus::UpToDate);
        assert_eq!(check_status(v("0.8.0"), &releases), CheckStatus::UpToDate);
        assert_eq!(check_status(v("0.6.2"), &[]), CheckStatus::UpToDate);
    }

    #[test]
    fn status_lines() {
        let running = v("0.6.2");
        assert_eq!(CheckStatus::Idle.line(running), "Gantry 0.6.2");
        assert_eq!(CheckStatus::Checking.line(running), "Checking…");
        assert_eq!(CheckStatus::UpToDate.line(running), "Gantry 0.6.2 is up to date.");
        assert_eq!(CheckStatus::Available(v("0.7.1")).line(running), "Gantry 0.7.1 is available.");
        assert_eq!(CheckStatus::Failed.line(running), "Couldn't reach GitHub.");
    }

    #[test]
    fn notes_blocks_map_lines() {
        use NotesBlock::*;
        assert_eq!(
            notes_blocks("## New\n# Big\n### Small\n- a\n* b\ntext\n\n<b>x</b>"),
            vec![
                Heading("New".into()),
                Heading("Big".into()),
                Heading("Small".into()),
                Bullet("a".into()),
                Bullet("b".into()),
                Line("text".into()),
                Gap,
                Line("<b>x</b>".into())
            ]
        );
        assert!(notes_blocks("").is_empty());
    }
}
