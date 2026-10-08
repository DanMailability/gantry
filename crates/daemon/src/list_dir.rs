//! Folder listing for the launcher's path completion (`ListDir`).

use std::path::{Path, PathBuf};

use protocol::message::DirEntry;

/// Most folders one answer holds.
const MAX_ENTRIES: usize = 500;

/// The error as text without its OS code ("Permission denied", not "Permission denied (os error 13)").
fn reason(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_string()
}

/// List the folders in `path` (absolute, or starting with `~` which `home` replaces): sorted case-insensitively with
/// names starting with `.` last, cut at 500. Returns the expanded path too, or why the listing failed.
pub fn list_dir(path: &str, home: Option<&Path>) -> Result<(PathBuf, Vec<DirEntry>), String> {
    const REFUSED: &str = "give an absolute path or start with ~";
    if path.is_empty() {
        return Err(REFUSED.into());
    }
    let expanded = app_core::settings::expand_home(Path::new(path), home);
    if !expanded.is_absolute() {
        return Err(REFUSED.into());
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&expanded).map_err(|error| reason(&error))?.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let symlink = kind.is_symlink();
        let is_dir =
            if symlink { std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) } else { kind.is_dir() };
        if is_dir {
            dirs.push(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), symlink });
        }
    }
    dirs.sort_by_key(|dir| (dir.name.starts_with('.'), dir.name.to_lowercase()));
    dirs.truncate(MAX_ENTRIES);
    Ok((expanded, dirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dirs: &[DirEntry]) -> Vec<&str> {
        dirs.iter().map(|dir| dir.name.as_str()).collect()
    }

    #[test]
    fn tilde_is_expanded() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("x")).unwrap();
        let (path, dirs) = list_dir("~", Some(tmp.path())).unwrap();
        assert_eq!(path, tmp.path());
        assert_eq!(names(&dirs), ["x"]);
    }

    #[test]
    fn only_folders_sorted_dotfiles_last() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["b", "A", ".hidden"] {
            std::fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        std::fs::write(tmp.path().join("c.txt"), "").unwrap();
        let (_, dirs) = list_dir(tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(names(&dirs), ["A", "b", ".hidden"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_folder_counts() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("b")).unwrap();
        std::fs::write(tmp.path().join("f"), "").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("b"), tmp.path().join("link")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("f"), tmp.path().join("filelink")).unwrap();
        let (_, dirs) = list_dir(tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(
            dirs,
            [DirEntry { name: "b".into(), symlink: false }, DirEntry { name: "link".into(), symlink: true }]
        );
    }

    // The error texts asserted here are Unix's.
    #[cfg(unix)]
    #[test]
    fn a_file_missing_or_relative_path_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, "").unwrap();
        let reason = list_dir(file.to_str().unwrap(), None).unwrap_err();
        assert!(reason.to_lowercase().contains("not a directory") && !reason.contains("os error"), "{reason}");
        let reason = list_dir(tmp.path().join("missing").to_str().unwrap(), None).unwrap_err();
        assert!(reason.to_lowercase().contains("no such file") && !reason.contains("os error"), "{reason}");
        let refused = Err("give an absolute path or start with ~".to_string());
        assert_eq!(list_dir("relative/dir", None), refused);
        assert_eq!(list_dir("", None), refused);
    }

    // Unix permissions; a root user can read the folder anyway, so the test then has nothing to check.
    #[cfg(unix)]
    #[test]
    fn a_folder_without_permission_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = list_dir(locked.to_str().unwrap(), None);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Err(reason) = result else { return };
        assert!(reason.to_lowercase().contains("permission denied") && !reason.contains("os error"), "{reason}");
    }

    #[test]
    fn the_answer_is_cut_at_500() {
        let tmp = tempfile::tempdir().unwrap();
        for index in 0..600 {
            std::fs::create_dir(tmp.path().join(format!("d{index:03}"))).unwrap();
        }
        let (_, dirs) = list_dir(tmp.path().to_str().unwrap(), None).unwrap();
        assert_eq!((dirs.len(), dirs[0].name.as_str(), dirs[499].name.as_str()), (500, "d000", "d499"));
    }
}
