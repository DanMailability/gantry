//! Change notifications for a config directory's `projects/` and `sessions/`.

use std::path::Path;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Call `on_change` whenever something under `config/projects` or `config/sessions` changes. Directories
/// that do not exist yet are not watched (the caller's periodic rescan covers them). Dropping the
/// returned watcher stops the notifications.
pub fn watch(config: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            on_change();
        }
    })?;
    for dir in ["projects", "sessions"] {
        let path = config.join(dir);
        if path.is_dir() {
            watcher.watch(&path, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn writing_a_transcript_notifies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/-a")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _watcher = watch(dir.path(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        std::fs::write(dir.path().join("projects/-a/s.jsonl"), "{}\n").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("no change notification");
    }

    #[test]
    fn a_config_dir_without_subdirectories_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(watch(dir.path(), || {}).is_ok());
    }
}
