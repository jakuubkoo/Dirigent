//! App state across restarts: `$XDG_STATE_HOME/agent-ide/state.json` (versioned, atomic writes).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct State {
    pub version: u32,
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub sidebar: SidebarState,
    pub window: Option<WindowState>,
    pub last_dir: Option<PathBuf>,
    /// Collapsed sidebar groups: repo roots, `""` for "Other".
    #[serde(default)]
    pub collapsed_groups: Vec<PathBuf>,
    /// Right-side git panel (added after version 1 shipped: missing in older files).
    #[serde(default)]
    pub git_panel: PanelState,
    #[serde(default)]
    pub diff_mode: DiffMode,
    #[serde(default)]
    pub split_style: SplitStyle,
    #[serde(default)]
    pub diff_engine: DiffEngine,
    #[serde(default)]
    pub notifications: Notifications,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: VERSION,
            tabs: Vec::new(),
            active: 0,
            sidebar: SidebarState { width: 220., open: true },
            window: None,
            last_dir: None,
            git_panel: PanelState::default(),
            collapsed_groups: Vec::new(),
            diff_mode: DiffMode::Unified,
            split_style: SplitStyle::Clean,
            diff_engine: DiffEngine::Auto,
            notifications: Notifications::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TabState {
    pub name: String,
    pub cwd: PathBuf,
    /// Claude Code session id of the last agent that ran in this tab.
    #[serde(default)]
    pub claude_session: Option<String>,
    /// Arguments claude was started with there (filtered again when resuming).
    #[serde(default)]
    pub claude_args: Vec<String>,
    /// Expanded directories of the tab's file tree (absolute paths).
    #[serde(default)]
    pub expanded: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct SidebarState {
    pub width: f32,
    pub open: bool,
}

/// The right panel. Fields added later default individually, so older files keep their width/open.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct PanelState {
    pub width: f32,
    pub open: bool,
    pub tab: PanelTab,
    /// File tree: show gitignored entries (dimmed) or hide them.
    pub show_ignored: bool,
}

impl Default for PanelState {
    fn default() -> Self {
        Self { width: 340., open: true, tab: PanelTab::Changes, show_ignored: true }
    }
}

/// How the diff view lays out changes.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    #[default]
    Unified,
    Split,
}

/// How the split diff draws changes: Classic = line backgrounds and +/- markers; Clean = plain code with only the
/// changed tokens (or whole added/removed lines) colored.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SplitStyle {
    Classic,
    #[default]
    Clean,
}

/// Desktop notifications (no settings UI yet: edit the state file). Fields default individually.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct Notifications {
    /// A session needs you (permission prompt, question).
    pub needs_you: bool,
    /// A session finished its turn.
    pub done: bool,
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { needs_you: true, done: true, sound: false }
    }
}

/// What finds the changes in a diff: Auto = the structural (syntax tree) diff when the file allows it, else the
/// word diff; Word = always the word diff.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiffEngine {
    #[default]
    Auto,
    Word,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanelTab {
    #[default]
    Changes,
    Files,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub mode: WindowMode,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// `AGENT_IDE_STATE` (tests/perf runs), else `$XDG_STATE_HOME/agent-ide/state.json`, else `~/.local/state/…`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENT_IDE_STATE") {
        return p.into();
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("agent-ide/state.json")
}

/// Missing file → defaults. Unreadable, corrupted or unknown-version file → moved aside to `<file>.bad-<unix time>`
/// and defaults; never panics.
pub fn load(path: &Path) -> State {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return State::default(),
        Err(e) => return backup(path, &e.to_string()),
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string()).and_then(|v| {
        match v["version"].as_u64() {
            Some(n) if n == VERSION as u64 => serde_json::from_value::<State>(v).map_err(|e| e.to_string()),
            other => Err(format!("unsupported version {other:?}")),
        }
    });
    parsed.unwrap_or_else(|e| backup(path, &e))
}

fn backup(path: &Path, why: &str) -> State {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut bad = path.as_os_str().to_owned();
    bad.push(format!(".bad-{secs}"));
    let moved = std::fs::rename(path, &bad).is_ok();
    eprintln!(
        "agent-ide: ignoring state file {} ({why}){}; starting fresh",
        path.display(),
        if moved { format!(", kept as {}", PathBuf::from(&bad).display()) } else { String::new() }
    );
    State::default()
}

/// Atomic: write `<file>.tmp`, fsync, rename over the old file. A crash leaves either the old or the new state.
pub fn save(path: &Path, state: &State) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Whether Claude Code still has a transcript for this session (`~/.claude/projects/*/<id>.jsonl`, the same
/// cross-project lookup `claude --resume <id>` does). Blocking IO: call off the UI thread.
pub fn transcript_exists(session_id: &str) -> bool {
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return false;
    }
    let projects = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude/projects");
    let Ok(dirs) = std::fs::read_dir(projects) else { return false };
    dirs.flatten().any(|d| d.path().join(format!("{session_id}.jsonl")).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-ide-persist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("state.json")
    }

    fn sample() -> State {
        State {
            tabs: vec![TabState {
                name: "api".into(),
                cwd: "/home/x/api".into(),
                claude_session: Some("00000000-0000-4000-8000-000000000005".into()),
                claude_args: vec!["--model".into(), "haiku".into()],
                expanded: vec!["/home/x/api/src".into()],
            }],
            active: 0,
            window: Some(WindowState { mode: WindowMode::Maximized, x: 10., y: 20., w: 1200., h: 750. }),
            last_dir: Some("/home/x".into()),
            git_panel: PanelState { width: 400., open: false, tab: PanelTab::Files, show_ignored: false },
            diff_mode: DiffMode::Split,
            split_style: SplitStyle::Classic,
            diff_engine: DiffEngine::Word,
            notifications: Notifications { needs_you: true, done: false, sound: true },
            ..State::default()
        }
    }

    #[test]
    fn roundtrip_and_atomic_save() {
        let p = tmp("roundtrip");
        assert_eq!(load(&p), State::default(), "missing file = defaults");
        save(&p, &sample()).unwrap();
        assert!(!p.with_extension("json.tmp").exists(), "temp file renamed away");
        assert_eq!(load(&p), sample());
        // Old files without the optional tab fields still load.
        std::fs::write(&p, r#"{"version":1,"tabs":[{"name":"a","cwd":"/"}],"active":0,
            "sidebar":{"width":200,"open":false},"window":null,"last_dir":null}"#).unwrap();
        let old = load(&p);
        assert_eq!(old.tabs[0].claude_session, None);
        assert_eq!(old.git_panel, PanelState::default(), "files from before the git panel load with its defaults");
        assert!(old.tabs[0].expanded.is_empty());
        assert_eq!(old.diff_mode, DiffMode::Unified);
        assert_eq!(old.split_style, SplitStyle::Clean, "Clean is the default split style");
        assert_eq!(old.diff_engine, DiffEngine::Auto, "structural when possible by default");
        assert_eq!(old.notifications, Notifications { needs_you: true, done: true, sound: false });
        std::fs::write(&p, r#"{"version":1,"tabs":[],"active":0,"sidebar":{"width":200,"open":false},"window":null,
            "last_dir":null,"notifications":{"done":false}}"#).unwrap();
        assert_eq!(load(&p).notifications, Notifications { done: false, ..Notifications::default() }, "per field");
        // A git panel saved before the file tree existed keeps its width/open.
        std::fs::write(&p, r#"{"version":1,"tabs":[],"active":0,"sidebar":{"width":200,"open":false},"window":null,
            "last_dir":null,"git_panel":{"width":280,"open":false}}"#).unwrap();
        assert_eq!(load(&p).git_panel, PanelState { width: 280., open: false, ..PanelState::default() });
    }

    #[test]
    fn corrupted_or_unknown_version_is_backed_up() {
        for (i, bad) in ["not json", r#"{"version":1,"tabs":"#, r#"{"version":99,"tabs":[]}"#, r#"{"tabs":[]}"#]
            .iter()
            .enumerate()
        {
            let p = tmp(&format!("bad{i}"));
            std::fs::write(&p, bad).unwrap();
            assert_eq!(load(&p), State::default(), "{bad}");
            assert!(!p.exists(), "bad file moved aside");
            let backups: Vec<_> = std::fs::read_dir(p.parent().unwrap())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("state.json.bad-"))
                .collect();
            assert_eq!(backups.len(), 1, "{bad}");
            assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), *bad);
        }
    }

    #[test]
    fn transcript_lookup_rejects_odd_ids() {
        assert!(!transcript_exists("../../etc/passwd"));
        assert!(!transcript_exists(""));
        assert!(!transcript_exists("00000000-0000-4000-8000-000000000000"));
    }
}
