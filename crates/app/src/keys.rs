//! The global shortcuts: one table that both binds them and lists them in Settings → Keyboard.

use gpui::{App, KeyBinding};

use crate::workspace::*;

macro_rules! shortcuts {
    ($(($keys:literal, $action:expr, $label:literal)),* $(,)?) => {
        /// (gpui keystroke, label); an empty label = not listed on its own (see `EXTRA`).
        pub const TABLE: &[(&str, &str)] = &[$(($keys, $label)),*];

        pub fn bind(cx: &mut App) {
            let ctx = Some("Workspace");
            cx.bind_keys([$(KeyBinding::new($keys, $action, ctx)),*]);
        }
    };
}

shortcuts![
    ("ctrl-shift-t", NewSession, "New session (project picker)"),
    ("ctrl-shift-n", NewInProject, "New session in the current project"),
    ("ctrl-shift-w", CloseSession, "Close session"),
    ("ctrl-tab", NextSession, "Next session"),
    ("ctrl-shift-tab", PrevSession, "Previous session"),
    ("ctrl-shift-j", NextNeedsYou, "Next session that needs you"),
    ("ctrl-shift-r", ResumeAll, "Resume all saved Claude sessions"),
    ("f2", Rename, "Rename session (or double click)"),
    ("ctrl-shift-b", ToggleSidebar, "Toggle sidebar"),
    ("ctrl-shift-g", ShowChanges, "Show Changes"),
    ("ctrl-shift-e", ShowFiles, "Show Files"),
    ("ctrl-shift-d", ToggleDiffMode, "Unified / split diff"),
    ("ctrl-,", OpenSettings, "Settings"),
    // Zoom = the terminal font size setting. Bindings are matched before any view's key handler, so the terminal
    // (and claude in it) never sees these keys.
    ("ctrl-=", ZoomIn, "Zoom in (terminal font size)"),
    ("ctrl-+", ZoomIn, "Zoom in"),
    ("ctrl--", ZoomOut, "Zoom out"),
    ("ctrl-0", ZoomReset, "Reset zoom"),
    ("ctrl-1", Activate1, ""),
    ("ctrl-2", Activate2, ""),
    ("ctrl-3", Activate3, ""),
    ("ctrl-4", Activate4, ""),
    ("ctrl-5", Activate5, ""),
    ("ctrl-6", Activate6, ""),
    ("ctrl-7", Activate7, ""),
    ("ctrl-8", Activate8, ""),
    ("ctrl-9", Activate9, ""),
];

/// Keys that only work in one place (handled by that view), shown after the table.
pub const EXTRA: &[(&str, &str)] = &[
    ("Ctrl+1..9", "Jump to session"),
    ("Esc", "Close a diff, preview, Settings or dialog"),
    ("Ctrl+Enter", "Commit (in the commit box)"),
    ("Enter", "Picker: open the setup dialog · exited session: restart it"),
    ("Ctrl+Enter / Shift+Enter", "Picker: open a session at once (Shift: the other claude choice)"),
    ("Tab", "Picker: complete a path"),
];

/// `ctrl-shift-t` → `Ctrl+Shift+T`.
pub fn pretty(keys: &str) -> String {
    let mut rest = keys;
    let mut out = String::new();
    // Modifiers are prefixes; what's left is the key itself (which may be "-").
    while let Some((m, r)) = [("ctrl-", "Ctrl+"), ("shift-", "Shift+"), ("alt-", "Alt+")]
        .iter()
        .find_map(|(p, m)| rest.strip_prefix(p).map(|r| (*m, r)))
    {
        out.push_str(m);
        rest = r;
    }
    out + &if rest == "tab" { "Tab".to_string() } else { rest.to_uppercase() }
}

/// Every listed shortcut: (keys as shown, label).
pub fn listed() -> impl Iterator<Item = (String, &'static str)> {
    TABLE.iter().filter(|(_, l)| !l.is_empty()).map(|(k, l)| (pretty(k), *l)).chain(EXTRA.iter().map(|(k, l)| (k.to_string(), *l)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_shown_readably() {
        let mut keys: Vec<_> = TABLE.iter().map(|(k, _)| *k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), TABLE.len(), "a key bound twice");
        assert_eq!(pretty("ctrl-shift-t"), "Ctrl+Shift+T");
        assert_eq!(pretty("ctrl-,"), "Ctrl+,");
        assert_eq!(pretty("f2"), "F2");
        assert_eq!(pretty("ctrl--"), "Ctrl+-");
        assert_eq!(pretty("ctrl-+"), "Ctrl++");
        assert_eq!(pretty("ctrl-shift-tab"), "Ctrl+Shift+Tab");
        // gpui parses every key in the table the way it's shown.
        for (k, _) in TABLE {
            let ks = gpui::Keystroke::parse(k).unwrap_or_else(|e| panic!("{k}: {e:?}"));
            assert!(ks.modifiers.control || *k == "f2", "{k}");
        }
        assert_eq!(gpui::Keystroke::parse("ctrl--").unwrap().key, "-");
        assert!(listed().any(|(k, l)| k == "Ctrl+," && l == "Settings"));
    }
}
