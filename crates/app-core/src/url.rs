//! Find a URL at a position in a line of terminal text.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`]+"#).expect("valid URL regex"));

/// A URL found in a line of terminal text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Columns (char indices) the URL covers.
    pub cols: Range<usize>,
    /// The URL, without trailing punctuation.
    pub url: String,
}

/// Return the link covering char index `col` of `line`.
pub fn link_at(line: &str, col: usize) -> Option<Link> {
    URL.find_iter(line).find_map(|found| {
        let url = found.as_str().trim_end_matches(['.', ',', ')', ']', ';', ':', '!', '?']);
        let start = line[..found.start()].chars().count();
        let cols = start..start + url.chars().count();
        cols.contains(&col).then(|| Link { cols, url: url.to_string() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_at(line: &str, col: usize) -> Option<String> {
        link_at(line, col).map(|link| link.url)
    }

    #[test]
    fn finds_url_under_column() {
        let line = "see https://example.com/a?b=1 now";
        assert_eq!(url_at(line, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 28).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 2), None);
        assert_eq!(url_at(line, 30), None);
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_url() {
        assert_eq!(url_at("(https://x.io/p).", 5).as_deref(), Some("https://x.io/p"));
    }

    #[test]
    fn link_reports_its_column_range() {
        let link = link_at("see https://x.io/p now", 6).unwrap();
        assert_eq!((link.cols, link.url.as_str()), (4..18, "https://x.io/p"));
    }

    #[test]
    fn columns_count_chars_not_bytes() {
        let line = "שלום http://a.b";
        assert_eq!(url_at(line, 5).as_deref(), Some("http://a.b"));
    }
}
