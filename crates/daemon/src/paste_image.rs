//! Saving a pasted image where the pane's Claude can read it (`SavePasteImage`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use protocol::message::ImageFormat;

/// Saved images older than this are deleted when the next one is saved.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A fresh file name for an image: `<uuid>.<extension>`.
pub fn file_name(format: ImageFormat) -> String {
    format!("{}.{}", uuid::Uuid::new_v4(), format.extension())
}

/// Save `data` in `dir` under a fresh name and return its absolute path. The folder is created private (0700) and
/// files older than `MAX_AGE` before `now` are deleted first. The bytes go to `<name>.tmp` (0600) and are renamed, so
/// no reader sees a half file. Errors read "<what>: <io error>".
pub fn save_local(dir: &Path, format: ImageFormat, data: &[u8], now: SystemTime) -> Result<PathBuf, String> {
    let describe = |what: &str, path: &Path, error: std::io::Error| format!("{what} {}: {error}", path.display());
    std::fs::create_dir_all(dir).map_err(|e| describe("cannot create", dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| describe("cannot protect", dir, e))?;
    }
    sweep(dir, now);
    let name = file_name(format);
    let path = dir.join(&name);
    let tmp = dir.join(format!("{name}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written =
        options.open(&tmp).and_then(|mut file| file.write_all(data)).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(describe("cannot write", &path, error));
    }
    Ok(path)
}

/// Delete the regular files in `dir` last modified more than `MAX_AGE` before `now`.
fn sweep(dir: &Path, now: SystemTime) {
    let Some(limit) = now.checked_sub(MAX_AGE) else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() && meta.modified().is_ok_and(|modified| modified < limit) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(n: u64) -> Duration {
        Duration::from_secs(n * 3600)
    }

    #[test]
    fn saves_the_bytes_under_a_fresh_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("paste");
        let a = save_local(&dir, ImageFormat::Png, b"one", SystemTime::now()).unwrap();
        let b = save_local(&dir, ImageFormat::Png, b"two", SystemTime::now()).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.extension().unwrap(), "png");
        assert!(a.starts_with(&dir) && a.is_absolute());
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        let leftovers = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().extension().unwrap() == "tmp");
        assert_eq!(leftovers.count(), 0);
    }

    // Unix permission bits; Windows has no equivalent here.
    #[cfg(unix)]
    #[test]
    fn folder_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("paste");
        let path = save_local(&dir, ImageFormat::Jpeg, b"x", SystemTime::now()).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn sweeps_files_older_than_a_day() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let now = SystemTime::now();
        let old = dir.join("old.png");
        let recent = dir.join("recent.png");
        for (path, age) in [(&old, hours(25)), (&recent, hours(1))] {
            let file = std::fs::File::create(path).unwrap();
            file.set_modified(now - age).unwrap();
        }
        save_local(&dir, ImageFormat::Png, b"x", now).unwrap();
        assert!(!old.exists());
        assert!(recent.exists());
    }

    #[test]
    fn fails_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, b"").unwrap();
        let dir = file.join("paste");
        let error = save_local(&dir, ImageFormat::Gif, b"x", SystemTime::now()).unwrap_err();
        assert!(error.contains(&dir.display().to_string()), "{error}");
    }
}
