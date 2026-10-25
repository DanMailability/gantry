//! Parsing the visible terminal screen of a Claude Code permission prompt into a structured dialog: the question,
//! an optional detail (the command, file or action), and the numbered choices. The reading view renders this as a
//! card with tappable buttons; when parsing fails it falls back to showing the live terminal region.
//!
//! This is a best-effort reader of an undocumented, box-drawn TUI, so it is deliberately conservative: it only
//! returns a prompt when it finds at least two options numbered 1, 2, 3, … in order.

/// One choice in a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptOption {
    /// The option's number (what to send to choose it).
    pub number: u32,
    /// The visible label, e.g. "Yes, and don't ask again".
    pub label: String,
    /// Whether the TUI's selection marker (❯) is on this option.
    pub selected: bool,
}

/// A parsed permission prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// The question line, e.g. "Do you want to proceed?".
    pub question: String,
    /// The lines above the question (the command, file or action), if any, shown in monospace.
    pub detail: Option<String>,
    /// The numbered choices, in order.
    pub options: Vec<PromptOption>,
}

/// Box-drawing and frame characters stripped from the edges of a line.
const BORDER: &str = "│┃┆┇┊┋╎╏║╭╮╰╯─━┌┐└┘├┤┬┴┼▌▐";

fn is_border(c: char) -> bool {
    c.is_whitespace() || BORDER.contains(c)
}

/// A screen line with its frame and selection marker stripped: the text, and whether it was marked selected (❯/>).
fn clean_line(raw: &str) -> (String, bool) {
    let trimmed = raw.trim_matches(is_border);
    let (selected, rest) = match trimmed.strip_prefix('❯').or_else(|| trimmed.strip_prefix('>')) {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    (rest.trim().to_string(), selected)
}

/// `"2. Yes, and don't ask"` → `(2, "Yes, and don't ask")`. Requires a digit run, a dot, a space, then a label.
fn numbered(text: &str) -> Option<(u32, String)> {
    let dot = text.find('.')?;
    let number: u32 = text[..dot].parse().ok()?;
    let rest = &text[dot + 1..];
    let label = rest.strip_prefix(' ')?.trim();
    (!label.is_empty()).then(|| (number, label.to_string()))
}

/// Parse the visible screen `lines` into a prompt, or `None` when they do not look like a numbered prompt.
pub fn parse_prompt(lines: &[String]) -> Option<Prompt> {
    let cleaned: Vec<(String, bool)> = lines.iter().map(|line| clean_line(line)).collect();

    let mut options: Vec<PromptOption> = Vec::new();
    let mut first_option: Option<usize> = None;
    for (index, (text, selected)) in cleaned.iter().enumerate() {
        if let Some((number, label)) = numbered(text) {
            if first_option.is_none() {
                first_option = Some(index);
            }
            options.push(PromptOption { number, label, selected: *selected });
        }
    }
    let first_option = first_option?;
    // Conservative: at least two options, numbered 1, 2, 3, … in order.
    if options.len() < 2 || options.iter().enumerate().any(|(i, o)| o.number as usize != i + 1) {
        return None;
    }

    // The text lines above the first option, nearest first: the question is the closest one, the rest are the
    // detail (the command/file/action). Blank and border lines are skipped; a previous option or a sane line cap
    // bounds how far up it reaches, so it stays within the prompt box.
    let mut header: Vec<&str> = Vec::new();
    for (text, _) in cleaned[..first_option].iter().rev() {
        if numbered(text).is_some() {
            break;
        }
        if text.is_empty() {
            continue;
        }
        header.push(text.as_str());
        if header.len() >= 6 {
            break;
        }
    }
    header.reverse();
    let question = header.pop()?.to_string();
    let detail = (!header.is_empty()).then(|| header.join("\n"));

    Some(Prompt { question, detail, options })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_a_boxed_permission_prompt_with_a_selection() {
        let screen = lines(
            "╭──────────────────────────────────────────╮\n\
             │  Bash command                              │\n\
             │  npm install                               │\n\
             │                                            │\n\
             │  Do you want to proceed?                   │\n\
             │  ❯ 1. Yes                                  │\n\
             │    2. Yes, and don't ask again this session│\n\
             │    3. No, and tell Claude what to do       │\n\
             ╰──────────────────────────────────────────╯",
        );
        let prompt = parse_prompt(&screen).expect("a prompt");
        assert_eq!(prompt.question, "Do you want to proceed?");
        assert_eq!(prompt.detail.as_deref(), Some("Bash command\nnpm install"));
        assert_eq!(prompt.options.len(), 3);
        assert_eq!((prompt.options[0].number, prompt.options[0].selected), (1, true));
        assert_eq!(prompt.options[1].label, "Yes, and don't ask again this session");
        assert!(!prompt.options[1].selected);
        assert_eq!(prompt.options[2].number, 3);
    }

    #[test]
    fn parses_a_plain_prompt_without_a_box_or_detail() {
        let screen = lines("Do you want to make this edit?\n  1. Yes\n  2. No");
        let prompt = parse_prompt(&screen).expect("a prompt");
        assert_eq!(prompt.question, "Do you want to make this edit?");
        assert_eq!(prompt.detail, None);
        assert_eq!(prompt.options.len(), 2);
    }

    #[test]
    fn needs_at_least_two_options() {
        assert_eq!(parse_prompt(&lines("Proceed?\n  1. Yes")), None);
    }

    #[test]
    fn rejects_non_consecutive_or_unnumbered() {
        assert_eq!(parse_prompt(&lines("Pick\n  1. a\n  3. c")), None);
        assert_eq!(parse_prompt(&lines("just some text\nwith no options")), None);
        // A decimal is not an option number.
        assert_eq!(parse_prompt(&lines("version 1.5 released\nand 2.0 soon")), None);
    }

    #[test]
    fn empty_screen_is_no_prompt() {
        assert_eq!(parse_prompt(&[]), None);
        assert_eq!(parse_prompt(&lines("\n\n")), None);
    }

    #[test]
    fn the_question_is_the_line_closest_to_the_options() {
        let screen = lines("Edit file\nsrc/main.rs\nApply this change?\n  1. Yes\n  2. No");
        let prompt = parse_prompt(&screen).unwrap();
        assert_eq!(prompt.question, "Apply this change?");
        assert_eq!(prompt.detail.as_deref(), Some("Edit file\nsrc/main.rs"));
    }
}
