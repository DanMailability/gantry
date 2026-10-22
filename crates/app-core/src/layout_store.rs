//! The workspace layout file: versioned JSON, validated on load, replaced atomically on save.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct SavedLayout {
    version: u32,
    workspace: Workspace,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Move a version 1 layout to version 2: the one daemon instance becomes the local host's entry.
fn migrate_v1(value: &mut serde_json::Value) {
    if let Some(workspace) = value.get_mut("workspace").and_then(|workspace| workspace.as_object_mut())
        && let Some(instance) = workspace.remove("instance")
    {
        workspace.insert("instances".into(), serde_json::json!({ "local": instance }));
    }
    value["version"] = VERSION.into();
}

/// Read the saved workspace. `Ok(None)` if there is none; an error if it is unreadable, from
/// another format version, or internally inconsistent. Version 1 layouts are migrated.
pub fn load(path: &Path) -> io::Result<Option<Workspace>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut value: serde_json::Value = serde_json::from_str(&text).map_err(|err| invalid(err.to_string()))?;
    match value.get("version").and_then(|version| version.as_u64()) {
        Some(1) => migrate_v1(&mut value),
        Some(version) if version == u64::from(VERSION) => {}
        other => return Err(invalid(format!("layout version {other:?} (expected {VERSION})"))),
    }
    let saved: SavedLayout = serde_json::from_value(value).map_err(|err| invalid(err.to_string()))?;
    if !saved.workspace.is_consistent() {
        return Err(invalid("layout references missing or duplicate panes"));
    }
    Ok(Some(saved.workspace))
}

/// Write the workspace, replacing the file atomically (write a temp file, then rename).
pub fn save(path: &Path, workspace: &Workspace) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(&SavedLayout { version: VERSION, workspace: workspace.saveable() })
        .map_err(|err| invalid(err.to_string()))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::PaneLaunch;
    use protocol::host::HostId;

    fn sample() -> Workspace {
        let mut ws = Workspace::default();
        ws.set_instance(&protocol::host::HostId::local(), "i");
        let a = ws.new_tab(PaneLaunch::claude("/repo".into()));
        ws.pane_started(a, 3, "/repo".into());
        ws.split(crate::split_tree::Axis::Vertical, PaneLaunch::claude("/repo".into()));
        ws
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("layout.json");
        let workspace = sample();
        save(&path, &workspace).unwrap();
        assert_eq!(load(&path).unwrap(), Some(workspace));
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp file left behind");
    }

    #[test]
    fn names_round_trip_and_old_layouts_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let mut workspace = sample();
        workspace.rename_tab(0, "Test cleanup");
        let shell = workspace.new_tab(PaneLaunch::shell("/repo".into(), &Default::default()));
        workspace.rename_leaf(shell, "build box");
        save(&path, &workspace).unwrap();
        assert_eq!(load(&path).unwrap(), Some(workspace));
        assert_eq!(load(&path).unwrap().unwrap().leaf_name(shell), Some("build box"));
        // A layout written before names existed has no name keys at all.
        let mut value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let tab = &mut value["workspace"]["tabs"][0];
        assert_eq!(tab["name"], "Test cleanup");
        tab.as_object_mut().unwrap().remove("name");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(load(&path).unwrap().is_some());
    }

    #[test]
    fn a_missing_file_is_no_layout() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("layout.json")).unwrap(), None);
    }

    #[test]
    fn garbage_foreign_versions_and_inconsistent_layouts_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_err());
        let mut value = serde_json::to_value(SavedLayout { version: VERSION, workspace: sample() }).unwrap();
        value["version"] = 99.into();
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(load(&path).is_err());
        let mut broken = sample();
        broken.leaves.clear();
        std::fs::write(&path, serde_json::to_string(&SavedLayout { version: VERSION, workspace: broken }).unwrap())
            .unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn the_settings_tab_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let mut workspace = sample();
        workspace.open_settings();
        save(&path, &workspace).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("settings"));
        assert_eq!(load(&path).unwrap().unwrap().tabs, sample().tabs);
    }

    #[test]
    fn layouts_saved_before_m4_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{"version":1,"workspace":{"instance":"i","tabs":[{"root":{"Leaf":1},"focused":1}],"active":0,
            "leaves":{"1":{"pane":null,"launch":{"program":"claude","args":[],"cwd":"/r"}}},"next_id":1}}"#,
        )
        .unwrap();
        let workspace = load(&path).unwrap().unwrap();
        assert_eq!(workspace.leaves[&1].session, None);
        assert_eq!(workspace.sidebar, crate::workspace::SidebarState::default());
    }

    #[test]
    fn a_version_1_layout_loads_as_local_and_is_saved_as_version_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{"version":1,"workspace":{"instance":"old","tabs":[{"root":{"Leaf":1},"focused":1}],"active":0,
            "leaves":{"1":{"pane":4,"launch":{"program":"claude","args":[],"cwd":"/r"}}},"next_id":1}}"#,
        )
        .unwrap();
        let workspace = load(&path).unwrap().unwrap();
        assert_eq!(workspace.instances[&HostId::local()], "old");
        assert!(workspace.leaves[&1].host.is_local());
        assert_eq!(workspace.leaves[&1].pane, Some(4));
        save(&path, &workspace).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 2") && text.contains("\"instances\""), "{text}");
        assert!(!text.contains("\"instance\""), "{text}");
        assert_eq!(load(&path).unwrap(), Some(workspace));
    }

    #[test]
    fn remote_leaves_round_trip_and_local_ones_carry_no_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let mut workspace = sample();
        let remote = workspace.new_tab(PaneLaunch::shell("/srv".into(), &Default::default()));
        workspace.leaves.get_mut(&remote).unwrap().host = HostId::named("devbox");
        workspace.pane_started(remote, 9, "/srv".into());
        workspace.set_instance(&HostId::named("devbox"), "r1");
        save(&path, &workspace).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("\"host\"").count(), 1, "only the remote leaf names its host: {text}");
        assert_eq!(load(&path).unwrap(), Some(workspace));
    }

    #[test]
    fn a_layout_without_host_folders_loads_and_an_empty_map_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let mut workspace = sample();
        save(&path, &workspace).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("host_folders"), "{text}");
        assert_eq!(load(&path).unwrap().unwrap().host_folders, Default::default());
        let devbox = HostId::named("devbox");
        workspace
            .host_folders
            .insert(devbox.clone(), vec![crate::launcher::HostFolder { path: "/srv".into(), last_active_ms: 4 }]);
        save(&path, &workspace).unwrap();
        assert_eq!(load(&path).unwrap(), Some(workspace));
    }
}
