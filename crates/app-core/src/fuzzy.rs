//! Fuzzy matching (nucleo) shared by the sidebar filter and the launcher.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A compiled query.
pub struct Fuzzy {
    pattern: Pattern,
    matcher: Matcher,
    buf: Vec<char>,
}

impl Fuzzy {
    /// A matcher for `query`; `None` when the query is blank (everything matches).
    pub fn new(query: &str) -> Option<Fuzzy> {
        if query.trim().is_empty() {
            return None;
        }
        Some(Fuzzy {
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            buf: Vec::new(),
        })
    }

    /// Score of `text` (higher is better), or `None` if it does not match.
    pub fn score(&mut self, text: &str) -> Option<u32> {
        self.pattern.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_queries_match_everything_and_others_filter() {
        assert!(Fuzzy::new("  ").is_none());
        let mut fuzzy = Fuzzy::new("jnkns").unwrap();
        assert!(fuzzy.score("Fix Jenkins lib").is_some());
        assert!(fuzzy.score("Bump k8s chart").is_none());
        let mut exact = Fuzzy::new("rust").unwrap();
        assert!(exact.score("~/Private/Repos/rust_terminal").unwrap() > exact.score("~/r/u/s/t").unwrap_or(0));
    }

    #[test]
    fn case_is_ignored() {
        assert!(Fuzzy::new("Font").unwrap().score("font fallback").is_some());
        assert!(Fuzzy::new("font").unwrap().score("Font Fallback").is_some());
    }
}
