//! Preferences: `$XDG_CONFIG_HOME/dirigent/settings.json`, separate from the state file (tabs, window, projects).
//! Loaded leniently (a bad value falls back to its default with a note, never a crash), written atomically, watched
//! for hand edits (`watch`: inotify on the directory, no polling).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::layout::{self, Dock, Layout};

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

/// What finds the changes in a diff: Auto = the structural (syntax tree) diff when the file allows it, else the
/// word diff; Word = always the word diff. Old state files said "auto".
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiffEngine {
    #[default]
    #[serde(rename = "structural", alias = "auto")]
    Auto,
    Word,
}

/// What a plain click on a sidebar group's `+` opens (Shift: the other claude choice).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlusOpens {
    /// The project's "start claude" default.
    #[default]
    Project,
    Shell,
    Claude,
}

/// Desktop notifications.
#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Answer claude's "trust this folder?" dialog with Yes in sessions we start claude in.
    pub trust_auto_accept: bool,
    pub plus_opens: PlusOpens,
    /// The whole UI in percent (Ctrl+= / Ctrl+- / Ctrl+0); multiplies every size, the terminal font included.
    pub ui_zoom: u32,
    pub font_size: f32,
    pub scrollback: usize,
    pub notifications: Notifications,
    pub diff_mode: DiffMode,
    pub split_style: SplitStyle,
    pub diff_engine: DiffEngine,
    pub layout: Layout,
    /// Fetch every watched repo in the background every N minutes; 0 = never (the default: no timers unless asked).
    pub fetch_minutes: u32,
}

pub const FONT_SIZE: (f32, f32) = (8., 32.);
pub const SCROLLBACK: (usize, usize) = (0, 100_000);
pub const UI_ZOOM: (u32, u32) = (50, 200);
pub const FETCH_MINUTES: (u32, u32) = (0, 120);
/// Ctrl+= / Ctrl+- step, percent.
pub const ZOOM_STEP: u32 = 10;

impl Settings {
    /// Ctrl+= (Some(1)), Ctrl+- (Some(-1)), Ctrl+0 (None): the next UI zoom.
    pub fn zoom_step(self, dir: Option<i32>) -> u32 {
        match dir {
            None => Settings::default().ui_zoom,
            Some(d) => {
                // Snap to the step grid first (a hand-typed 115 goes to 120 / 110).
                let cur = self.ui_zoom as i32;
                let st = ZOOM_STEP as i32;
                let snapped = if d > 0 { (cur / st + 1) * st } else { (cur + st - 1) / st * st - st };
                (snapped.max(0) as u32).clamp(UI_ZOOM.0, UI_ZOOM.1)
            }
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trust_auto_accept: true,
            plus_opens: PlusOpens::Project,
            ui_zoom: 100,
            font_size: crate::theme::TERMINAL_FONT_SIZE,
            scrollback: 10_000,
            notifications: Notifications::default(),
            diff_mode: DiffMode::Unified,
            split_style: SplitStyle::Clean,
            diff_engine: DiffEngine::Auto,
            layout: Layout::default(),
            fetch_minutes: 0,
        }
    }
}

/// Every key: (section, key). Anything else in the file gets an "unknown" note.
const KEYS: &[(&str, &[&str])] = &[
    ("general", &["trust_auto_accept", "plus_opens", "ui_zoom"]),
    ("terminal", &["font_size", "scrollback"]),
    ("notifications", &["needs_you", "done", "sound"]),
    ("diff", &["mode", "split_style", "engine"]),
    ("layout", &["sessions", "files", "changes", "search", "left_width", "right_width"]),
    ("git", &["fetch_minutes"]),
];

impl Settings {
    /// Only the values that differ from the defaults (a later default change reaches everyone who never set it;
    /// a reset removes the key). Nothing set: `{}`.
    pub fn to_json(self) -> String {
        let all = |s: Settings| {
            json!({
                "general": { "trust_auto_accept": s.trust_auto_accept, "plus_opens": s.plus_opens, "ui_zoom": s.ui_zoom },
                "terminal": { "font_size": s.font_size, "scrollback": s.scrollback },
                "notifications": {
                    "needs_you": s.notifications.needs_you,
                    "done": s.notifications.done,
                    "sound": s.notifications.sound,
                },
                "diff": { "mode": s.diff_mode, "split_style": s.split_style, "engine": s.diff_engine },
                "layout": {
                    "sessions": s.layout.sessions,
                    "files": s.layout.files,
                    "changes": s.layout.changes,
                    "search": s.layout.search,
                    "left_width": s.layout.left_width,
                    "right_width": s.layout.right_width,
                },
                "git": { "fetch_minutes": s.fetch_minutes },
            })
        };
        let (mine, defaults) = (all(self), all(Settings::default()));
        let mut out = serde_json::Map::new();
        for (sec, keys) in mine.as_object().into_iter().flatten() {
            let changed: serde_json::Map<_, _> = keys
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, v)| defaults[sec][k.as_str()] != **v)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !changed.is_empty() {
                out.insert(sec.clone(), changed.into());
            }
        }
        let mut s = serde_json::to_string_pretty(&Value::Object(out)).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// Result of reading the file: the values to use and what was wrong with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub settings: Settings,
    pub notes: Vec<String>,
    /// Not valid JSON: never written over without a backup.
    pub broken: bool,
}

/// Lenient: each key on its own; a missing key is its default, a bad one its default plus a note. Text that isn't
/// a JSON object keeps `fallback` (the last good values) with a note.
pub fn parse(text: &str, fallback: &Settings) -> Parsed {
    let v = match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => return broken(fallback, "settings.json: not a JSON object".into()),
        Err(e) => return broken(fallback, format!("settings.json: {e}")),
    };
    let mut notes = Vec::new();
    let d = Settings::default();
    let mut g = Get { v: &v, notes: &mut notes };
    let s = Settings {
        trust_auto_accept: g.get("general", "trust_auto_accept", d.trust_auto_accept, "true or false", |_| true),
        plus_opens: g.get("general", "plus_opens", d.plus_opens, "\"project\", \"shell\" or \"claude\"", |_| true),
        ui_zoom: g.get("general", "ui_zoom", d.ui_zoom, "a whole number 50–200 (percent)", |n: &u32| {
            (UI_ZOOM.0..=UI_ZOOM.1).contains(n)
        }),
        font_size: g.get("terminal", "font_size", d.font_size, "a number 8–32", |n: &f32| {
            (FONT_SIZE.0..=FONT_SIZE.1).contains(n)
        }),
        scrollback: g.get("terminal", "scrollback", d.scrollback, "a whole number 0–100000", |n: &usize| {
            (SCROLLBACK.0..=SCROLLBACK.1).contains(n)
        }),
        notifications: Notifications {
            needs_you: g.get("notifications", "needs_you", d.notifications.needs_you, "true or false", |_| true),
            done: g.get("notifications", "done", d.notifications.done, "true or false", |_| true),
            sound: g.get("notifications", "sound", d.notifications.sound, "true or false", |_| true),
        },
        diff_mode: g.get("diff", "mode", d.diff_mode, "\"unified\" or \"split\"", |_| true),
        split_style: g.get("diff", "split_style", d.split_style, "\"classic\" or \"clean\"", |_| true),
        diff_engine: g.get("diff", "engine", d.diff_engine, "\"structural\" or \"word\"", |_| true),
        layout: {
            let dl = d.layout;
            let mut dock = |key, def: Dock| g.get("layout", key, def, "\"left\" or \"right\"", |_| true);
            let (sessions, files, changes, search) = (
                dock("sessions", dl.sessions),
                dock("files", dl.files),
                dock("changes", dl.changes),
                dock("search", dl.search),
            );
            let mut width = |key, def| {
                g.get("layout", key, def, "a whole number 100–2000 (pixels)", |n: &u32| {
                    (layout::WIDTH.0..=layout::WIDTH.1).contains(n)
                })
            };
            let (left_width, right_width) = (width("left_width", dl.left_width), width("right_width", dl.right_width));
            Layout { sessions, files, changes, search, left_width, right_width }
        },
        fetch_minutes: g.get(
            "git",
            "fetch_minutes",
            d.fetch_minutes,
            "a whole number 0–120 (minutes; 0 = off)",
            |n: &u32| (FETCH_MINUTES.0..=FETCH_MINUTES.1).contains(n),
        ),
    };
    for (sec, val) in v.as_object().into_iter().flatten() {
        match KEYS.iter().find(|(s, _)| s == sec) {
            None => notes.push(format!("{sec}: unknown section, ignored")),
            Some((_, keys)) => match val.as_object() {
                None => notes.push(format!("{sec}: not an object, using the defaults")),
                Some(o) => notes.extend(
                    o.keys().filter(|k| !keys.contains(&k.as_str())).map(|k| format!("{sec}.{k}: unknown, ignored")),
                ),
            },
        }
    }
    Parsed { settings: s, notes, broken: false }
}

fn broken(fallback: &Settings, note: String) -> Parsed {
    Parsed { settings: *fallback, notes: vec![note], broken: true }
}

struct Get<'a> {
    v: &'a Value,
    notes: &'a mut Vec<String>,
}

impl Get<'_> {
    fn get<T: DeserializeOwned + Serialize>(
        &mut self,
        sec: &str,
        key: &str,
        default: T,
        expect: &str,
        ok: impl Fn(&T) -> bool,
    ) -> T {
        let Some(raw) = self.v.get(sec).and_then(|s| s.get(key)) else { return default };
        match serde_json::from_value::<T>(raw.clone()) {
            Ok(t) if ok(&t) => t,
            _ => {
                let def = serde_json::to_string(&default).unwrap_or_default();
                self.notes.push(format!("{sec}.{key}: {raw} is not {expect}, using {def}"));
                default
            }
        }
    }
}

/// `DIRIGENT_SETTINGS`; with `DIRIGENT_STATE` (test runs) `settings.json` next to that state file, so a test never
/// touches the real config; else `$XDG_CONFIG_HOME/dirigent/settings.json`, else `~/.config/…`.
pub fn path() -> PathBuf {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    path_from(var("DIRIGENT_SETTINGS"), var("DIRIGENT_STATE"), var("XDG_CONFIG_HOME"), var("HOME"))
}

fn path_from(
    settings: Option<PathBuf>,
    state: Option<PathBuf>,
    config: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(p) = settings {
        return p;
    }
    if let Some(s) = state {
        return s.with_file_name("settings.json");
    }
    config.unwrap_or_else(|| home.unwrap_or_default().join(".config")).join("dirigent/settings.json")
}

/// What `load` found: the values, notes, and the text as read (to tell our own writes from hand edits).
pub struct Loaded {
    pub parsed: Parsed,
    pub text: Option<String>,
}

/// Startup. No settings file yet → the preferences an older state file still holds (diff mode/style/engine,
/// notifications) are taken over and the file is written. Blocking IO: before the window opens, like the state.
pub fn load(path: &Path, state_path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => Loaded { parsed: parse(&text, &Settings::default()), text: Some(text) },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (parsed, migrated) = from_state(state_path);
            let text = parsed.settings.to_json();
            match write(path, &text) {
                Ok(()) if migrated => {
                    eprintln!("dirigent: moved the preferences from the state file to {}", path.display())
                }
                Ok(()) => {}
                Err(e) => eprintln!("dirigent: couldn't write {}: {e}", path.display()),
            }
            Loaded { parsed, text: Some(text) }
        }
        Err(e) => Loaded { parsed: broken(&Settings::default(), format!("settings.json: {e}")), text: None },
    }
}

/// Dock widths used to live in the state file (`sidebar.width`, `git_panel.width`): a settings file without a
/// `layout` section takes them over once (true: something changed, write the file). Broken text: nothing.
pub fn adopt_widths(s: &mut Settings, text: Option<&str>, left: f32, right: f32) -> bool {
    let Some(v) = text.and_then(|t| serde_json::from_str::<Value>(t).ok()) else { return false };
    if !v.is_object() || v.get("layout").is_some() {
        return false;
    }
    let before = s.layout;
    let fit = |w: f32| (w.round() as u32).clamp(layout::WIDTH.0, layout::WIDTH.1);
    s.layout.left_width = fit(left);
    s.layout.right_width = fit(right);
    s.layout != before
}

/// The preference fields of a state file from before settings.json (true: it had at least one).
fn from_state(state_path: &Path) -> (Parsed, bool) {
    let old = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null);
    let mut diff = serde_json::Map::new();
    for (from, to) in [("diff_mode", "mode"), ("split_style", "split_style"), ("diff_engine", "engine")] {
        if let Some(v) = old.get(from) {
            diff.insert(to.into(), v.clone());
        }
    }
    let notif = old.get("notifications").cloned();
    let migrated = !diff.is_empty() || notif.is_some();
    let mut v = json!({ "diff": diff });
    if let Some(n) = notif {
        v["notifications"] = n;
    }
    (parse(&v.to_string(), &Settings::default()), migrated)
}

/// A UI change. `broken`: the file isn't valid JSON (a hand edit in progress?) → kept as `<file>.bad-<unix time>`
/// first. Atomic (tmp + fsync + rename).
pub fn save(path: &Path, text: &str, broken: bool) -> std::io::Result<()> {
    if broken && path.exists() {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut bad = path.as_os_str().to_owned();
        bad.push(format!(".bad-{secs}"));
        std::fs::rename(path, &bad)?;
    }
    write(path, text)
}

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    crate::persist::write_atomic(path, text.as_bytes())
}

/// Watches the file's directory (an editor may replace the file by rename or write it in place) and sends one
/// ping per burst of changes to `settings.json` (100 ms quiet). The thread sleeps in `recv` while nothing happens.
/// Keep the returned watcher alive.
pub fn watch(path: &Path) -> notify::Result<(notify::RecommendedWatcher, async_channel::Receiver<()>)> {
    use notify::{Config, Event, EventKindMask, RecursiveMode, Watcher as _};
    let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<()>();
    let handler = move |res: notify::Result<Event>| {
        if let Ok(ev) = res
            && ev.paths.iter().any(|p| p.file_name() == Some(&name))
        {
            let _ = raw_tx.send(());
        }
    };
    let mut w = notify::RecommendedWatcher::new(handler, Config::default().with_event_kinds(EventKindMask::CORE))?;
    w.watch(&dir, RecursiveMode::NonRecursive)?;
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new().name("settings-watch".into()).spawn(move || {
        while raw_rx.recv().is_ok() {
            while raw_rx.recv_timeout(Duration::from_millis(100)).is_ok() {}
            if tx.is_closed() {
                return;
            }
            let _ = tx.try_send(());
        }
    })?;
    Ok((w, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dirigent-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip_and_defaults() {
        let s = Settings {
            trust_auto_accept: false,
            plus_opens: PlusOpens::Claude,
            ui_zoom: 130,
            font_size: 15.,
            scrollback: 500,
            notifications: Notifications { needs_you: false, done: true, sound: true },
            diff_mode: DiffMode::Split,
            split_style: SplitStyle::Classic,
            diff_engine: DiffEngine::Word,
            layout: Layout {
                sessions: Dock::Right,
                files: Dock::Left,
                changes: Dock::Right,
                search: Dock::Left,
                left_width: 300,
                right_width: 400,
            },
            fetch_minutes: 15,
        };
        let p = parse(&s.to_json(), &Settings::default());
        assert!(s.to_json().contains("\"fetch_minutes\": 15"));
        let bad = parse(r#"{"git": {"fetch_minutes": 500}}"#, &Settings::default());
        assert_eq!((bad.settings.fetch_minutes, bad.notes.len()), (0, 1), "out of range: off, with a note");
        assert_eq!(p, Parsed { settings: s, notes: vec![], broken: false });
        assert_eq!(parse("{}", &s).settings, Settings::default(), "missing keys are defaults, not the fallback");
        assert_eq!(Settings::default().to_json(), "{}\n", "nothing set: an empty object");
        let one = Settings { scrollback: 500, ..Settings::default() }.to_json();
        assert_eq!(
            serde_json::from_str::<Value>(&one).unwrap(),
            json!({ "terminal": { "scrollback": 500 } }),
            "only what differs"
        );
        // A reset (back to the default) drops the key, so a later default change applies again.
        let back = Settings { scrollback: d_scroll(), ..Settings::default() }.to_json();
        assert_eq!(back, "{}\n");
    }

    #[test]
    fn layout_section_only_non_defaults_bad_values_and_width_migration() {
        let mut s = Settings::default();
        s.layout.set_dock(layout::Panel::Files, Dock::Left);
        s.layout.left_width = 260;
        assert_eq!(
            serde_json::from_str::<Value>(&s.to_json()).unwrap(),
            json!({ "layout": { "files": "left", "left_width": 260 } })
        );
        let p = parse(r#"{"layout":{"sessions":"top","left_width":5,"right_width":500}}"#, &Settings::default());
        assert_eq!(p.settings.layout, Layout { right_width: 500, ..Layout::default() });
        assert_eq!(p.notes.len(), 2, "{:?}", p.notes);
        // The state file's widths come over once, only into a file without a layout section.
        let mut s = Settings::default();
        assert!(adopt_widths(&mut s, Some("{}\n"), 250.4, 420.));
        assert_eq!((s.layout.left_width, s.layout.right_width), (250, 420));
        let mut s = Settings::default();
        assert!(!adopt_widths(&mut s, Some("{}"), 220., 340.), "defaults: nothing to write");
        assert!(!adopt_widths(&mut s, Some(r#"{"layout":{}}"#), 250., 420.), "already has a layout section");
        assert!(!adopt_widths(&mut s, Some("{ half"), 250., 420.), "broken file: untouched");
        assert!(!adopt_widths(&mut s, None, 250., 420.));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn zoom_steps_snap_and_clamp() {
        let at = |z| Settings { ui_zoom: z, ..Settings::default() };
        assert_eq!(at(100).zoom_step(Some(1)), 110);
        assert_eq!(at(100).zoom_step(Some(-1)), 90);
        assert_eq!(at(115).zoom_step(Some(1)), 120);
        assert_eq!(at(115).zoom_step(Some(-1)), 110);
        assert_eq!(at(200).zoom_step(Some(1)), 200);
        assert_eq!(at(50).zoom_step(Some(-1)), 50);
        assert_eq!(at(170).zoom_step(None), 100);
        assert_eq!(parse(r#"{"general":{"ui_zoom":300}}"#, &at(100)).settings.ui_zoom, 100, "out of range: default");
        assert_eq!(parse(r#"{"general":{"ui_zoom":150}}"#, &at(100)).settings.ui_zoom, 150);
    }

    fn d_scroll() -> usize {
        Settings::default().scrollback
    }

    #[test]
    fn bad_values_fall_back_per_key_with_a_note() {
        let p = parse(
            r#"{"terminal":{"font_size":"big","scrollback":-1},"diff":{"mode":"split","engine":"fancy","extra":1},
                "notifications":{"sound":true},"themes":{}}"#,
            &Settings::default(),
        );
        let d = Settings::default();
        assert_eq!(p.settings.font_size, d.font_size);
        assert_eq!(p.settings.scrollback, d.scrollback);
        assert_eq!(p.settings.diff_mode, DiffMode::Split, "good keys next to bad ones still apply");
        assert_eq!(p.settings.diff_engine, DiffEngine::Auto);
        assert!(p.settings.notifications.sound && p.settings.notifications.done);
        assert!(!p.broken);
        let joined = p.notes.join("\n");
        for want in [
            "terminal.font_size: \"big\" is not a number 8–32, using 13.5",
            "terminal.scrollback",
            "diff.engine",
            "diff.extra: unknown",
            "themes: unknown section",
        ] {
            assert!(joined.contains(want), "{want} in {joined}");
        }
        assert_eq!(p.notes.len(), 5);
        let out = parse(r#"{"terminal":{"font_size":40}}"#, &d);
        assert_eq!(out.settings.font_size, d.font_size, "out of range");
        assert_eq!(parse(r#"{"general":5}"#, &d).notes, vec!["general: not an object, using the defaults"]);
    }

    #[test]
    fn broken_json_keeps_the_last_good_values() {
        let last = Settings { font_size: 18., ..Settings::default() };
        for text in ["{\"terminal\": {", "[1]", ""] {
            let p = parse(text, &last);
            assert!(p.broken, "{text}");
            assert_eq!(p.settings, last);
            assert_eq!(p.notes.len(), 1);
        }
    }

    #[test]
    fn path_never_leaves_a_test_run_and_follows_xdg() {
        let p = |s: Option<&str>, st: Option<&str>, c: Option<&str>| {
            path_from(s.map(Into::into), st.map(Into::into), c.map(Into::into), Some("/home/u".into()))
        };
        assert_eq!(p(Some("/x/s.json"), Some("/t/state.json"), None), PathBuf::from("/x/s.json"));
        assert_eq!(p(None, Some("/t/state.json"), Some("/c")), PathBuf::from("/t/settings.json"));
        assert_eq!(p(None, None, Some("/c")), PathBuf::from("/c/dirigent/settings.json"));
        assert_eq!(p(None, None, None), PathBuf::from("/home/u/.config/dirigent/settings.json"));
    }

    #[test]
    fn first_start_takes_the_preferences_over_from_an_old_state_file() {
        let d = tmp("migrate");
        let (state, file) = (d.join("state.json"), d.join("settings.json"));
        std::fs::write(
            &state,
            r#"{"version":1,"tabs":[],"diff_mode":"split","split_style":"classic",
            "diff_engine":"auto","notifications":{"done":false,"sound":true}}"#,
        )
        .unwrap();
        let l = load(&file, &state);
        let s = l.parsed.settings;
        assert_eq!(
            (s.diff_mode, s.split_style, s.diff_engine),
            (DiffMode::Split, SplitStyle::Classic, DiffEngine::Auto)
        );
        assert_eq!(s.notifications, Notifications { needs_you: true, done: false, sound: true });
        assert!(l.parsed.notes.is_empty());
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(Some(&written), l.text.as_ref());
        assert_eq!(
            serde_json::from_str::<Value>(&written).unwrap(),
            json!({ "notifications": { "done": false, "sound": true }, "diff": { "mode": "split", "split_style": "classic" } }),
            "only the non-default values (auto = the default structural, needs_you = the default true)"
        );
        assert_eq!(parse(&written, &Settings::default()).settings, s);

        // An existing settings file wins: the state file's leftovers are never applied again.
        std::fs::write(&file, r#"{"diff":{"mode":"unified"}}"#).unwrap();
        assert_eq!(load(&file, &state).parsed.settings.diff_mode, DiffMode::Unified);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"diff":{"mode":"unified"}}"#, "untouched");

        // No state file (or one without preferences): defaults, file written.
        let (state2, file2) = (d.join("none.json"), d.join("s2.json"));
        assert_eq!(load(&file2, &state2).parsed.settings, Settings::default());
        assert_eq!(std::fs::read_to_string(&file2).unwrap(), "{}\n", "kept as an empty object, not deleted");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saving_over_a_broken_file_keeps_a_backup() {
        let d = tmp("backup");
        let file = d.join("settings.json");
        std::fs::write(&file, "{ half").unwrap();
        save(&file, "{}\n", true).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}\n");
        let bad: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bad-"))
            .collect();
        assert_eq!(bad.len(), 1);
        assert_eq!(std::fs::read_to_string(bad[0].path()).unwrap(), "{ half");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn watch_sees_in_place_writes_and_renames() {
        let d = tmp("watch");
        let file = d.join("settings.json");
        std::fs::write(&file, "{}").unwrap();
        let (_w, rx) = watch(&file).unwrap();
        let wait = |what: &str| {
            let start = std::time::Instant::now();
            while rx.try_recv().is_err() {
                assert!(start.elapsed() < Duration::from_secs(3), "no ping for {what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(&file).unwrap();
            f.write_all(b"{\"a\":1}").unwrap();
        }
        wait("in-place write");
        std::fs::write(d.join("other.json"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "other files in the directory don't count");
        std::fs::write(d.join("new.tmp"), "{}").unwrap();
        std::fs::rename(d.join("new.tmp"), &file).unwrap();
        wait("rename over it");
        let _ = std::fs::remove_dir_all(&d);
    }
}
