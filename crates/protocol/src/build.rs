//! What this binary was built from.

/// The app version: `[workspace.package] version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit of the build, or `unknown`.
pub const COMMIT: &str = env!("GANTRY_COMMIT");

/// What `gantry --version` prints: `gantry 0.3.0 (a1b2c3d)`.
pub fn version_line() -> String {
    format!("gantry {VERSION} ({COMMIT})")
}
