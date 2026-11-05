//! App state across restarts: `$XDG_STATE_HOME/dirigent/state.json` (versioned, atomic writes).

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
    /// Remembered projects (added later: missing in older files → seeded from the tabs).
    #[serde(default)]
    pub projects: Option<Vec<crate::projects::Project>>,
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
            projects: None,
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
    /// Files this tab's agent changed (its Claude session's set).
    #[serde(default)]
    pub changes: Option<crate::changes::Saved>,
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

/// `DIRIGENT_STATE` (tests/perf runs), else `$XDG_STATE_HOME/dirigent/state.json`, else `~/.local/state/…`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("DIRIGENT_STATE") {
        return p.into();
    }
    state_base().join("dirigent/state.json")
}

fn state_base() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
}

/// The app was called agent-ide: its state dir moves to the new name on the first start (not for test runs with
/// `DIRIGENT_STATE`). Says what it did on stderr.
pub fn migrate_old_state() {
    if std::env::var_os("DIRIGENT_STATE").is_some() {
        return;
    }
    let base = state_base();
    match migrate(&base.join("agent-ide"), &base.join("dirigent")) {
        Some(Ok(how)) => eprintln!("dirigent: {how} the state from {}", base.join("agent-ide").display()),
        Some(Err(e)) => eprintln!("dirigent: couldn't take over the old agent-ide state: {e}"),
        None => {}
    }
}

/// Moves `old` to `new` when only `old` exists (None: nothing to do). Across filesystems, where a rename can't work,
/// only `state.json` is copied and `old` stays.
pub fn migrate(old: &Path, new: &Path) -> Option<std::io::Result<&'static str>> {
    if !old.is_dir() || new.exists() {
        return None;
    }
    Some(std::fs::rename(old, new).map(|_| "moved").or_else(|_| {
        std::fs::create_dir_all(new)?;
        std::fs::copy(old.join("state.json"), new.join("state.json")).map(|_| "copied")
    }))
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
        "dirigent: ignoring state file {} ({why}){}; starting fresh",
        path.display(),
        if moved { format!(", kept as {}", PathBuf::from(&bad).display()) } else { String::new() }
    );
    State::default()
}

/// Atomic: write `<file>.tmp`, fsync, rename over the old file. A crash leaves either the old or the new state.
pub fn save(path: &Path, state: &State) -> std::io::Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?)
}

/// `<file>.tmp`, fsync, rename over `path` (creating its directory).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
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

    #[test]
    fn old_state_dir_moves_once() {
        let d = std::env::temp_dir().join(format!("dirigent-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (old, new) = (d.join("agent-ide"), d.join("dirigent"));
        assert!(migrate(&old, &new).is_none(), "nothing to move");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("state.json"), "{}").unwrap();
        assert_eq!(migrate(&old, &new).unwrap().unwrap(), "moved");
        assert!(!old.exists() && new.join("state.json").exists());
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("state.json"), "old").unwrap();
        assert!(migrate(&old, &new).is_none(), "the new one exists: untouched");
        assert_eq!(std::fs::read_to_string(new.join("state.json")).unwrap(), "{}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn projects_missing_in_old_files_and_round_trip() {
        let old = r#"{"version":1,"tabs":[],"active":0,"sidebar":{"width":220.0,"open":true},"window":null,"last_dir":null}"#;
        let st: State = serde_json::from_str(old).unwrap();
        assert_eq!(st.projects, None, "None = seed from the tabs on first load");
        let mut st = st;
        st.projects = Some(vec![crate::projects::Project { root: "/p".into(), pinned: true, auto_claude: true, claude_args: vec!["--model".into(), "opus".into()], ..Default::default() }]);
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back, st);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dirigent-persist-{}-{name}", std::process::id()));
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
                changes: Some(crate::changes::Saved {
                    session: Some("00000000-0000-4000-8000-000000000005".into()),
                    files: vec![("/home/x/api/src/a.rs".into(), crate::changes::Touch { via_command: true })],
                }),
            }],
            active: 0,
            window: Some(WindowState { mode: WindowMode::Maximized, x: 10., y: 20., w: 1200., h: 750. }),
            last_dir: Some("/home/x".into()),
            git_panel: PanelState { width: 400., open: false, tab: PanelTab::Files, show_ignored: false },
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
        // Preferences an older file still holds (now in settings.json) are ignored, and the next save drops them.
        std::fs::write(&p, r#"{"version":1,"tabs":[],"active":0,"sidebar":{"width":200,"open":false},"window":null,
            "last_dir":null,"diff_mode":"split","notifications":{"done":false}}"#).unwrap();
        let st = load(&p);
        save(&p, &st).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("diff_mode") && !text.contains("notifications"), "{text}");
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
