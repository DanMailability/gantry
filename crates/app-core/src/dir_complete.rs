//! Path completion for the launcher on a remote host: split what was typed, filter a folder listing, drop stale
//! answers.

use protocol::message::DirEntry;

/// The parent folder typed so far (with its trailing `/`; `~/` for `~`) and the name prefix after the last `/`.
/// `("", input)` when there is no `/` and the input does not start with `~`.
pub fn split_typed(input: &str) -> (String, String) {
    match input.rfind('/') {
        Some(at) => (input[..=at].to_string(), input[at + 1..].to_string()),
        None if input == "~" => ("~/".to_string(), String::new()),
        None => (String::new(), input.to_string()),
    }
}

/// The folders whose name starts with `prefix` (case ignored), in order. Names starting with `.` appear only when
/// the prefix does.
pub fn filter(dirs: &[DirEntry], prefix: &str) -> Vec<DirEntry> {
    let prefix = prefix.to_lowercase();
    dirs.iter()
        .filter(|dir| {
            (!dir.name.starts_with('.') || prefix.starts_with('.')) && dir.name.to_lowercase().starts_with(&prefix)
        })
        .cloned()
        .collect()
}

/// Whether the typed text needs a new folder listing: it has a `/` and its parent is not the one listed.
pub fn wants_listing(typed: &str, listed_parent: &str) -> bool {
    typed.contains('/') && split_typed(typed).0 != listed_parent
}

/// Whether the 120 ms pause after the last edit has passed.
pub fn debounce_due(last_edit_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(last_edit_ms) >= 120
}

/// The newest folder listing and what asked for it; answers to older requests are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// The newest request id sent.
    pub request: u64,
    /// The parent folder as typed that `dirs` belongs to (empty before the first answer).
    pub parent: String,
    /// Its folders.
    pub dirs: Vec<DirEntry>,
    /// Why the last listing failed, if it did.
    pub error: Option<String>,
}

impl Completion {
    /// Take a listing for `parent`, unless it answers an older request than the newest.
    pub fn accept_listing(&mut self, request: u64, parent: String, dirs: Vec<DirEntry>) {
        if request < self.request {
            return;
        }
        self.parent = parent;
        self.dirs = dirs;
        self.error = None;
    }

    /// Take a failure, unless it answers an older request than the newest.
    pub fn accept_failure(&mut self, request: u64, reason: String) {
        if request < self.request {
            return;
        }
        self.dirs.clear();
        self.error = Some(reason);
    }

    /// Whether `parent` differs from the listed one.
    pub fn wants(&self, parent: &str) -> bool {
        self.parent != parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> DirEntry {
        DirEntry { name: name.into(), symlink: false }
    }

    #[test]
    fn typed_paths_split_at_the_last_slash() {
        assert_eq!(split_typed("~/pro"), ("~/".to_string(), "pro".to_string()));
        assert_eq!(split_typed("/home/me/"), ("/home/me/".to_string(), String::new()));
        assert_eq!(split_typed("~"), ("~/".to_string(), String::new()));
        assert_eq!(split_typed("pro"), (String::new(), "pro".to_string()));
    }

    #[test]
    fn filter_ignores_case_and_shows_dotfiles_only_for_a_dot() {
        let dirs = [dir("Projects"), dir("pics"), dir(".cache"), dir("src")];
        let names = |found: Vec<DirEntry>| found.into_iter().map(|d| d.name).collect::<Vec<_>>();
        assert_eq!(names(filter(&dirs, "p")), ["Projects", "pics"]);
        assert_eq!(names(filter(&dirs, "")), ["Projects", "pics", "src"]);
        assert_eq!(names(filter(&dirs, ".c")), [".cache"]);
    }

    #[test]
    fn a_listing_is_wanted_when_the_parent_changes() {
        assert!(wants_listing("~/pro", ""));
        assert!(!wants_listing("~/pro", "~/"));
        assert!(!wants_listing("pro", ""));
        assert!(!debounce_due(1000, 1119));
        assert!(debounce_due(1000, 1120));
    }

    #[test]
    fn a_stale_answer_never_replaces_a_newer_one() {
        let mut completion = Completion { request: 3, ..Completion::default() };
        completion.accept_listing(2, "~/old/".into(), vec![dir("x")]);
        assert_eq!(completion.parent, "");
        completion.accept_failure(1, "late".into());
        assert_eq!(completion.error, None);
        completion.accept_listing(3, "~/".into(), vec![dir("a")]);
        assert_eq!((completion.parent.as_str(), completion.dirs.len()), ("~/", 1));
        completion.accept_failure(3, "No such file".into());
        assert_eq!(completion.error.as_deref(), Some("No such file"));
        assert!(completion.dirs.is_empty());
        assert!(completion.wants("~/other/") && !completion.wants("~/"));
    }
}
