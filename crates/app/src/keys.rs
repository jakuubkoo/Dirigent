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

        /// The Command Center's `>` list: every labelled entry once (the first keys of a label), with its action.
        pub fn commands() -> Vec<(&'static str, &'static str, Box<dyn gpui::Action>)> {
            let mut all: Vec<(&'static str, &'static str, Box<dyn gpui::Action>)> = vec![$(($keys, $label, Box::new($action))),*];
            all.extend(command_only());
            let mut seen = std::collections::HashSet::new();
            all.into_iter().filter(|(_, l, _)| !l.is_empty() && seen.insert(*l)).collect()
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
    ("ctrl-shift-b", ToggleSidebar, "Show / hide Sessions"),
    ("ctrl-shift-g", ShowChanges, "Show Changes"),
    ("ctrl-shift-e", ShowFiles, "Show Files"),
    ("ctrl-shift-d", ToggleDiffMode, "Unified / split diff"),
    ("ctrl-shift-o", SwitchBranch, "Switch or create a branch"),
    ("ctrl-,", OpenSettings, "Settings"),
    ("ctrl-p", QuickOpen, "Go to file (Command Center)"),
    ("ctrl-shift-p", CommandPalette, "Run a command (Command Center, >)"),
    ("ctrl-shift-f", FindInFiles, "Search in files (Command Center, #; results in the Search panel)"),
    // Not Ctrl+F: ^F is the shell's forward-char (zsh-autosuggestions accepts with it), Claude Code's cursor-right
    // and less/vim's page down.
    ("ctrl-alt-f", FindInTerminal, "Find in terminal"),
    ("shift-f10", SessionMenu, "Session menu (also the Menu key; or right click a session)"),
    ("menu", SessionMenu, ""),
    // UI zoom (the whole window, terminal font included). Bindings are matched before any view's key handler, so
    // the terminal (and claude in it) never sees these keys.
    ("ctrl-=", ZoomIn, "Zoom in (whole UI)"),
    ("ctrl-+", ZoomIn, "Zoom in (whole UI)"),
    ("ctrl--", ZoomOut, "Zoom out (whole UI)"),
    ("ctrl-0", ZoomReset, "Reset zoom to 100 %"),
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

/// Commands without a shortcut (Command Center `>` only).
fn command_only() -> Vec<(&'static str, &'static str, Box<dyn gpui::Action>)> {
    vec![
        ("", "Git: Sync (fetch, then fast-forward, push or publish)", Box::new(GitSync)),
        ("", "Git: Fetch", Box::new(GitFetch)),
        ("", "Git: Pull (fast-forward only)", Box::new(GitPull)),
        ("", "Git: Push (publish a new branch)", Box::new(GitPush)),
    ]
}

/// Keys that only work in one place (handled by that view), shown after the table.
pub const EXTRA: &[(&str, &str)] = &[
    ("Ctrl+1..9", "Jump to session"),
    ("Esc", "Close a diff, preview, Settings, menu, dialog or the terminal's find bar"),
    ("Enter / Shift+Enter", "Terminal find bar: next (newer) / previous (older) match"),
    ("Alt+C / Alt+R", "Terminal find bar: match case / regular expression"),
    ("Up / Down, Enter", "Menus: choose an item, run it"),
    (
        "Ctrl+Shift+C / Ctrl+Shift+V",
        "Terminal: copy the selection / paste text (Ctrl+V stays the app's: images in Claude Code)",
    ),
    (
        "Drag, double / triple click",
        "Terminal: select text, a word, a line (Shift+click extends; Shift also while an app takes the mouse)",
    ),
    ("Middle click", "Terminal: paste the selection (Linux primary selection)"),
    ("Ctrl+click", "Terminal: open a link in the browser, or a file path (src/main.rs:42) in the preview"),
    ("Middle click on a session", "Close it (or its x on hover)"),
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
    TABLE
        .iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(k, l)| (pretty(k), *l))
        .chain(EXTRA.iter().map(|(k, l)| (k.to_string(), *l)))
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
            // Without Ctrl only keys no terminal app relies on.
            assert!(ks.modifiers.control || ["f2", "shift-f10", "menu"].contains(k), "{k}");
        }
        assert_eq!(gpui::Keystroke::parse("ctrl--").unwrap().key, "-");
        assert!(listed().any(|(k, l)| k == "Ctrl+," && l == "Settings"));
        assert_eq!(pretty("shift-f10"), "Shift+F10");
        // The command list: every labelled entry once, with its action.
        let cmds = commands();
        let labels: std::collections::HashSet<_> =
            TABLE.iter().filter(|(_, l)| !l.is_empty()).map(|(_, l)| *l).collect();
        assert_eq!(cmds.len(), labels.len() + command_only().len());
        assert!(
            cmds.iter().any(|(k, l, a)| *k == "ctrl-=" && l.starts_with("Zoom in") && a.name().ends_with("ZoomIn"))
        );
    }
}
