//! `gantry --version` prints the version line and exits.

#[test]
fn version_flag_prints_version_and_commit() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gantry")).arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text, format!("gantry {} ({})\n", env!("CARGO_PKG_VERSION"), protocol::build::COMMIT));
    assert!(!protocol::build::COMMIT.is_empty());
}

#[test]
fn gantryd_prints_the_same_version_line_and_refuses_what_it_does_not_know() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gantryd")).arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text, format!("gantry {} ({})\n", env!("CARGO_PKG_VERSION"), protocol::build::COMMIT));
    let unknown = std::process::Command::new(env!("CARGO_BIN_EXE_gantryd")).arg("--launch").output().unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

/// The point of `gantryd`: a host needs none of the window, sound or keyboard libraries to run it.
#[cfg(target_os = "linux")]
#[test]
fn gantryd_links_no_window_sound_or_keyboard_library() {
    let Ok(output) = std::process::Command::new("ldd").arg(env!("CARGO_BIN_EXE_gantryd")).output() else {
        return; // no ldd here
    };
    let libraries = String::from_utf8_lossy(&output.stdout);
    for gui in ["libasound", "libxkbcommon", "libxcb", "libwayland", "libX11", "libGL", "libvulkan"] {
        assert!(!libraries.contains(gui), "gantryd links {gui}:\n{libraries}");
    }
}
