//! `gantry --version` prints the version line and exits.

#[test]
fn version_flag_prints_version_and_commit() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gantry")).arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text, format!("gantry {} ({})\n", env!("CARGO_PKG_VERSION"), protocol::build::COMMIT));
    assert!(!protocol::build::COMMIT.is_empty());
}
