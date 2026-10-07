//! Reading `~/.ssh/config` host aliases, and checks for the SSH host wizard's fields.

/// The host aliases of an ssh config: the `Host` patterns that are plain names (no `*`, `?` or `!`), in first-seen
/// order. An `Include` is read once through `read_include` (its own `Include`s are not followed).
pub fn aliases(text: &str, read_include: &mut dyn FnMut(&str) -> Option<String>) -> Vec<String> {
    let mut found = Vec::new();
    collect(text, read_include, true, &mut found);
    found
}

fn collect(text: &str, read_include: &mut dyn FnMut(&str) -> Option<String>, follow: bool, found: &mut Vec<String>) {
    let mut included: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else { continue };
        if keyword.eq_ignore_ascii_case("host") {
            for pattern in words {
                let plain = !pattern.contains(['*', '?']) && !pattern.starts_with('!');
                if plain && !found.iter().any(|known| known == pattern) {
                    found.push(pattern.to_string());
                }
            }
        } else if follow && keyword.eq_ignore_ascii_case("include") {
            for name in words {
                if included.iter().any(|done| done == name) {
                    continue;
                }
                included.push(name.to_string());
                if let Some(text) = read_include(name) {
                    collect(&text, read_include, false, found);
                }
            }
        }
    }
}

/// Whether `text` matches `pattern`, where `*` stands for any run of characters and `?` for exactly one.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    fn go(pattern: &[char], text: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some(('*', rest)) => (0..=text.len()).any(|skip| go(rest, &text[skip..])),
            Some(('?', rest)) => text.split_first().is_some_and(|(_, tail)| go(rest, tail)),
            Some((c, rest)) => text.split_first().is_some_and(|(t, tail)| t == c && go(rest, tail)),
        }
    }
    go(&pattern.chars().collect::<Vec<_>>(), &text.chars().collect::<Vec<_>>())
}

/// Whether `target` can follow `ssh --`: an alias or `user@host`, no whitespace or shell syntax, not an option.
pub fn validate_target(target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("Enter a host or user@host".into());
    }
    if target.starts_with('-') {
        return Err("A host cannot start with a dash".into());
    }
    if target.chars().any(|c| c.is_whitespace() || ";&|$`'\"<>()".contains(c)) {
        return Err("A host has no spaces or shell characters".into());
    }
    Ok(())
}

/// Whether `name` can be the name of a new host among the `existing` names.
pub fn validate_name(name: &str, existing: &[&str]) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name".into());
    }
    if name.eq_ignore_ascii_case("local") {
        return Err("\"local\" is this machine".into());
    }
    if name.contains(['/', ':']) {
        return Err("A name cannot contain / or :".into());
    }
    if existing.contains(&name) {
        return Err("A host with this name exists".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_skip_wildcards_and_negations_and_follow_one_include() {
        let text = "Host *\n  User x\nHost mainpc build-box\n  HostName a\nHost !bad ok? gpu1\nInclude extra\n";
        let mut read = |name: &str| (name == "extra").then(|| "Host from-include\nInclude deeper\n".to_string());
        assert_eq!(aliases(text, &mut read), ["mainpc", "build-box", "gpu1", "from-include"]);
    }

    #[test]
    fn wildcards_match_whole_names() {
        assert!(wildcard_match("*.conf", "a.conf"));
        assert!(wildcard_match("host-?", "host-1"));
        assert!(wildcard_match("*", "anything"));
        assert!(!wildcard_match("*.conf", "a.conf.bak"));
        assert!(!wildcard_match("host-?", "host-12"));
        assert!(wildcard_match("exact", "exact"));
    }

    #[test]
    fn names_must_be_unique_plain_and_not_local() {
        assert!(validate_name("MainPC", &["lab"]).is_ok());
        for bad in ["", " ", "local", "a/b", "a:b", "lab"] {
            assert!(validate_name(bad, &["lab"]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn targets_are_alias_or_user_at_host_without_spaces_or_leading_dash() {
        assert!(validate_target("ido@10.0.0.12").is_ok());
        assert!(validate_target("mainpc").is_ok());
        for bad in ["", "-oProxyCommand=x", "a b", "a;b"] {
            assert!(validate_target(bad).is_err(), "{bad:?}");
        }
    }
}
