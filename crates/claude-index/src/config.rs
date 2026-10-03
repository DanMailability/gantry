//! Where Claude Code keeps its files.

use std::path::PathBuf;

/// The Claude config directory for an environment: `CLAUDE_CONFIG_DIR` if set and non-empty, else
/// `.claude` in the home directory (`HOME`, or `USERPROFILE` on Windows). `None` without either.
pub fn config_dir(env: &[(String, String)]) -> Option<PathBuf> {
    let get = |key: &str| env.iter().find(|(k, v)| k == key && !v.is_empty()).map(|(_, v)| PathBuf::from(v));
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    get("CLAUDE_CONFIG_DIR").or_else(|| get(home).map(|home| home.join(".claude")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[cfg(unix)]
    #[test]
    fn explicit_config_dir_wins_over_home() {
        let env = env(&[("HOME", "/home/me"), ("CLAUDE_CONFIG_DIR", "/cfg")]);
        assert_eq!(config_dir(&env), Some(PathBuf::from("/cfg")));
    }

    #[cfg(unix)]
    #[test]
    fn home_fallback_and_empty_values_count_as_unset() {
        assert_eq!(
            config_dir(&env(&[("HOME", "/home/me"), ("CLAUDE_CONFIG_DIR", "")])),
            Some(PathBuf::from("/home/me/.claude"))
        );
        assert_eq!(config_dir(&env(&[("PATH", "/bin")])), None);
    }
}
