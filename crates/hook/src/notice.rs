//! The short text a desktop notification shows for a hook. Commands are never shown (they can carry secrets);
//! file tools show the path.

use serde_json::Value;

/// Longest path shown, in characters.
const MAX_DETAIL: usize = 80;

/// `Wants to run a command`, `Edit: src/main.rs`, or `Wants to use <tool>`.
pub fn permission_text(tool: &str, input: &Value) -> String {
    if tool == "Bash" {
        return "Wants to run a command".into();
    }
    let path = ["file_path", "notebook_path", "path"].iter().find_map(|k| input[*k].as_str());
    match path.map(|p| cut(p, MAX_DETAIL)).filter(|p| !p.is_empty()) {
        Some(p) => format!("{tool}: {p}"),
        None => format!("Wants to use {tool}"),
    }
}

/// A Notification hook's message, cut to one short line.
pub fn message_text(message: &str) -> String {
    cut(message, MAX_DETAIL + 40)
}

/// Longest session title made from a prompt, in characters.
pub const MAX_TITLE: usize = 40;

/// A session title from the first prompt: its first non-empty line, whitespace collapsed, at most `MAX_TITLE`
/// characters cut on a word boundary (`…` marks the cut). Slash commands and empty prompts give none.
pub fn prompt_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    if line.starts_with('/') {
        return None;
    }
    let one = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= MAX_TITLE {
        return Some(one);
    }
    let mut out = String::new();
    for word in one.split(' ') {
        let len = out.chars().count() + usize::from(!out.is_empty()) + word.chars().count();
        if len > MAX_TITLE - 1 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = one.chars().take(MAX_TITLE - 1).collect(); // one long word: hard cut
    }
    out.push('…');
    Some(out)
}

/// One line, at most `max` characters (an ellipsis marks the cut).
fn cut(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let mut out: String = one.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_text_shows_paths_never_commands() {
        assert_eq!(permission_text("Bash", &json!({"command": "TOKEN=x deploy"})), "Wants to run a command");
        assert_eq!(
            permission_text("Edit", &json!({"file_path": "/home/x/src/main.rs", "old_string": "a"})),
            "Edit: /home/x/src/main.rs"
        );
        assert_eq!(permission_text("WebFetch", &json!({"url": "https://example.com"})), "Wants to use WebFetch");
        let long = format!("/home/x/{}", "dir/".repeat(40));
        let t = permission_text("Write", &json!({ "file_path": long }));
        assert_eq!(t.chars().count(), "Write: ".len() + MAX_DETAIL);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn prompt_titles() {
        assert_eq!(prompt_title("Reply with just the word ok").as_deref(), Some("Reply with just the word ok"));
        assert_eq!(prompt_title("\n  \n  fix   the\tbug \nsecond line").as_deref(), Some("fix the bug"));
        assert_eq!(
            prompt_title("Switch to plan mode first. Do not write any code until I approve.").as_deref(),
            Some("Switch to plan mode first. Do not write…")
        );
        let t = prompt_title(&"ž".repeat(100)).unwrap();
        assert_eq!(t.chars().count(), MAX_TITLE);
        assert!(t.ends_with('…'));
        for p in ["/clear", "  /rename x", "", " \n\t "] {
            assert_eq!(prompt_title(p), None, "{p:?}");
        }
        for p in ["Switch to plan mode first. Do not write any code until I approve.", &"word ".repeat(30)] {
            assert!(prompt_title(p).unwrap().chars().count() <= MAX_TITLE);
        }
    }
}
