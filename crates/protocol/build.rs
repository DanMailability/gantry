//! Sets `GANTRY_COMMIT`: the short git commit of the build, from the environment (release builds in a container)
//! or from git, else `unknown`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GANTRY_COMMIT");
    let commit = std::env::var("GANTRY_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GANTRY_COMMIT={commit}");
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

/// The short commit, rebuilding whenever HEAD moves (HEAD itself, the branch it points to, packed refs).
fn git_commit() -> Option<String> {
    let mut watched = vec!["HEAD".to_string(), "packed-refs".to_string()];
    watched.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for path in watched {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    git(&["rev-parse", "--short=7", "HEAD"])
}
