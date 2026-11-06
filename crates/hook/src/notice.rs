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
}
