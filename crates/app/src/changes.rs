//! Which files each tab's agent changed. File tools are exact (the path from PostToolUse); Bash is best effort:
//! the repo's git status before the command vs. the first status that reflects everything up to its end.
//!
//! Inputs arrive off the UI thread: hooks from the agent listener, statuses from the repo watcher threads. The
//! UI is woken (bounded(1)) only when something it shows changed: a tab's summary, its marks, its set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use git::{FileChange, Status};
use hook::Kind;
use serde::{Deserialize, Serialize};

/// Commands running longer than this (a dev server, `cargo watch`) attribute nothing: their window would catch
/// everything anyone does meanwhile.
const MAX_WINDOW: Duration = Duration::from_secs(600);
/// A file-tool write this long before a command started can still be in the command's "after" status (the
/// watcher refreshes after 150 ms of quiet, at most every 1 s), so it's excluded there.
const DEBOUNCE: Duration = Duration::from_secs(1);
/// File-tool writes are remembered this long for the exclusion above.
const RECENT: Duration = Duration::from_secs(900);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touch {
    /// Attributed from a shell command's before/after git status (best effort), not from a file tool.
    #[serde(default)]
    pub via_command: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mark {
    /// Also in another tab's set.
    pub shared: bool,
    pub via_command: bool,
}

/// What the UI shows for one tab.
#[derive(Debug, Default, PartialEq)]
pub struct View {
    /// (files, +, −) of the agent's files that are changed in its repo now; None = none are.
    pub summary: Option<(usize, u64, u64)>,
    /// Those files (repo-relative, as in `Status`) with their marks.
    pub files: HashMap<String, Mark>,
    /// Every file in the set (absolute), changed now or not: the file tree's mark.
    pub touched: HashSet<PathBuf>,
}

/// The persisted set of one tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub session: Option<String>,
    pub files: Vec<(PathBuf, Touch)>,
}

struct Bash {
    id: String,
    root: PathBuf,
    before: Option<Arc<Status>>,
    /// The watcher's event counter at Pre / at Post (Post: None while running).
    received_pre: u64,
    target: Option<u64>,
    t_pre: Instant,
    t_post: Option<Instant>,
    /// No events had arrived at the last check: the next check with still none resolves it as "wrote nothing".
    quiet_once: bool,
}

#[derive(Default)]
struct Tab {
    session: Option<String>,
    files: BTreeMap<PathBuf, Touch>,
    repo: Option<PathBuf>,
    bash: Vec<Bash>,
    view: Arc<View>,
}

#[derive(Default)]
struct Repo {
    status: Option<Arc<Status>>,
    received: Option<Arc<AtomicU64>>,
    settled: u64,
}

#[derive(Default)]
struct Inner {
    tabs: HashMap<u32, Tab>,
    repos: HashMap<PathBuf, Repo>,
    /// Writes attributed to a tab (file tools and resolved commands), for excluding them from other tabs'
    /// command windows.
    recent: Vec<(PathBuf, Instant, u32)>,
    /// A set changed since the UI last saved (persist).
    sets_dirty: bool,
}

pub struct Tracker {
    inner: Mutex<Inner>,
    wake: async_channel::Sender<()>,
}

impl Tracker {
    pub fn new() -> (Arc<Self>, async_channel::Receiver<()>) {
        let (wake, rx) = async_channel::bounded(1);
        (Arc::new(Self { inner: Mutex::default(), wake }), rx)
    }

    /// A hook of `tab`'s agent (agent listener thread).
    pub fn hook(&self, tab: u32, kind: &Kind) {
        self.hook_at(tab, kind, Instant::now());
    }

    /// A status of `root` (watcher thread): `status` after the batch, `settled` = the batches it reflects.
    pub fn status(&self, root: &Path, status: Option<Arc<Status>>, settled: u64) {
        self.status_at(root, status, settled, Instant::now());
    }

    /// The watcher's event counter for `root` (when it starts).
    pub fn add_repo(&self, root: &Path, received: Arc<AtomicU64>) {
        self.inner.lock().unwrap().repos.entry(root.to_path_buf()).or_default().received = Some(received);
    }

    pub fn remove_repo(&self, root: &Path) {
        self.inner.lock().unwrap().repos.remove(root);
    }

    /// The tab's repo root (None: not in a repo).
    pub fn set_repo(&self, tab: u32, root: Option<PathBuf>) {
        let mut g = self.inner.lock().unwrap();
        let t = g.tabs.entry(tab).or_default();
        if t.repo != root {
            t.repo = root;
            self.refresh_views(&mut g);
        }
    }

    pub fn restore(&self, tab: u32, saved: Saved) {
        let mut g = self.inner.lock().unwrap();
        let t = g.tabs.entry(tab).or_default();
        t.session = saved.session;
        t.files = saved.files.into_iter().collect();
        self.refresh_views(&mut g);
    }

    pub fn saved(&self, tab: u32) -> Option<Saved> {
        let g = self.inner.lock().unwrap();
        let t = g.tabs.get(&tab)?;
        (t.session.is_some() || !t.files.is_empty()).then(|| Saved {
            session: t.session.clone(),
            files: t.files.iter().map(|(p, t)| (p.clone(), *t)).collect(),
        })
    }

    /// Forgets the tab's set (manual "Clear"); the session stays.
    pub fn clear(&self, tab: u32) {
        let mut g = self.inner.lock().unwrap();
        if let Some(t) = g.tabs.get_mut(&tab) {
            t.files.clear();
            g.sets_dirty = true;
            self.refresh_views(&mut g);
        }
    }

    pub fn remove_tab(&self, tab: u32) {
        let mut g = self.inner.lock().unwrap();
        if g.tabs.remove(&tab).is_some() {
            self.refresh_views(&mut g);
        }
    }

    pub fn view(&self, tab: u32) -> Arc<View> {
        self.inner.lock().unwrap().tabs.get(&tab).map(|t| t.view.clone()).unwrap_or_default()
    }

    /// Whether a set changed since the last call (the UI then saves the state file).
    pub fn take_sets_dirty(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().sets_dirty)
    }

    fn hook_at(&self, tab: u32, kind: &Kind, now: Instant) {
        let Kind::Hook { event, session_id, tool, tool_use_id, path, .. } = kind else { return };
        let mut g = self.inner.lock().unwrap();
        let g = &mut *g;
        let t = g.tabs.entry(tab).or_default();
        let mut changed = false;
        // A different session (new claude, /clear) starts empty; --resume keeps the id and the set.
        if let Some(sid) = session_id
            && t.session.as_ref() != Some(sid)
        {
            if t.session.is_some() && !t.files.is_empty() {
                t.files.clear();
                changed = true;
            }
            t.session = Some(sid.clone());
            g.sets_dirty = true;
        }
        if let Some(p) = path {
            let p = PathBuf::from(p);
            if t.files.insert(p.clone(), Touch { via_command: false }) != Some(Touch { via_command: false }) {
                changed = true;
            }
            g.recent.push((p, now, tab));
        }
        if tool.as_deref() == Some("Bash")
            && let Some(id) = tool_use_id
        {
            match event.as_str() {
                "PreToolUse" => {
                    if let Some(root) = t.repo.clone() {
                        let r = g.repos.entry(root.clone()).or_default();
                        let received = r.received.as_ref().map_or(0, |c| c.load(Ordering::Relaxed));
                        let before = r.status.clone();
                        let t = g.tabs.get_mut(&tab).unwrap();
                        t.bash.push(Bash {
                            id: id.clone(),
                            root,
                            before,
                            received_pre: received,
                            target: None,
                            t_pre: now,
                            t_post: None,
                            quiet_once: false,
                        });
                    }
                }
                "PostToolUse" => {
                    let received = |root: &Path| {
                        g.repos.get(root).and_then(|r| r.received.as_ref()).map_or(0, |c| c.load(Ordering::Relaxed))
                    };
                    let t = g.tabs.get(&tab).unwrap();
                    if let Some(i) = t.bash.iter().position(|b| &b.id == id) {
                        let r = received(&t.bash[i].root);
                        let b = &mut g.tabs.get_mut(&tab).unwrap().bash[i];
                        b.t_post = Some(now);
                        b.target = Some(r);
                    }
                }
                _ => {}
            }
        }
        if changed {
            g.sets_dirty = true;
        }
        // Every hook of the tab is a chance to settle its finished commands (no timers).
        changed |= resolve(g, now);
        g.recent.retain(|(_, t, _)| now.duration_since(*t) < RECENT);
        if changed {
            self.refresh_views(g);
        }
    }

    fn status_at(&self, root: &Path, status: Option<Arc<Status>>, settled: u64, now: Instant) {
        let mut g = self.inner.lock().unwrap();
        let g = &mut *g;
        let r = g.repos.entry(root.to_path_buf()).or_default();
        let new_status = status.is_some() && !opt_ptr_eq(&r.status, &status);
        if status.is_some() {
            r.status = status;
        }
        r.settled = r.settled.max(settled);
        let resolved = resolve(g, now);
        if new_status || resolved {
            self.refresh_views(g);
        }
    }

    /// Recomputes every tab's view; wakes the UI if any changed.
    fn refresh_views(&self, g: &mut Inner) {
        let owners: HashMap<&Path, usize> =
            g.tabs.values().flat_map(|t| t.files.keys()).fold(HashMap::new(), |mut m, p| {
                *m.entry(p.as_path()).or_default() += 1;
                m
            });
        let mut views = Vec::new();
        for (&id, t) in &g.tabs {
            let status = t.repo.as_ref().and_then(|r| Some((r, g.repos.get(r)?.status.as_ref()?)));
            let v = view(t, status.map(|(r, s)| (r.as_path(), s.as_ref())), &owners);
            if *t.view != v {
                views.push((id, v));
            }
        }
        if views.is_empty() {
            return;
        }
        for (id, v) in views {
            g.tabs.get_mut(&id).unwrap().view = Arc::new(v);
        }
        let _ = self.wake.try_send(());
    }
}

fn opt_ptr_eq(a: &Option<Arc<Status>>, b: &Option<Arc<Status>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn view(t: &Tab, status: Option<(&Path, &Status)>, owners: &HashMap<&Path, usize>) -> View {
    let touched: HashSet<PathBuf> = t.files.keys().cloned().collect();
    let Some((root, st)) = status else { return View { touched, ..View::default() } };
    let by_path: HashMap<&str, &FileChange> = st.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let (mut files, mut add, mut del) = (HashMap::new(), 0u64, 0u64);
    for (abs, touch) in &t.files {
        let Some(rel) = abs.strip_prefix(root).ok().and_then(Path::to_str) else { continue };
        // git's form (`/`), also when claude on Windows sent `C:\repo\src\a.rs`.
        let rel = if cfg!(windows) { rel.replace('\\', "/") } else { rel.to_string() };
        let Some(f) = by_path.get(rel.as_str()) else { continue };
        if let Some((a, d)) = f.lines {
            add += a as u64;
            del += d as u64;
        }
        let shared = owners.get(abs.as_path()).is_some_and(|&n| n > 1);
        files.insert(rel, Mark { shared, via_command: touch.via_command });
    }
    let summary = (!files.is_empty()).then_some((files.len(), add, del));
    View { summary, files, touched }
}

/// Settles finished commands whose effects are now in their repo's status. Returns whether a set changed.
fn resolve(g: &mut Inner, now: Instant) -> bool {
    let mut done: Vec<(u32, usize, Vec<PathBuf>)> = Vec::new();
    for (&tab, t) in g.tabs.iter_mut() {
        for (i, b) in t.bash.iter_mut().enumerate() {
            let Some(t_post) = b.t_post else { continue };
            let Some(r) = g.repos.get(&b.root) else {
                done.push((tab, i, Vec::new()));
                continue;
            };
            if t_post.duration_since(b.t_pre) > MAX_WINDOW {
                done.push((tab, i, Vec::new()));
                continue;
            }
            let received = r.received.as_ref().map_or(0, |c| c.load(Ordering::Relaxed));
            if received == b.received_pre {
                // No file event in the repo since Pre. Its events may still be on their way at Post, so it
                // takes a second look (the next hook or status) to call it "wrote nothing".
                if b.quiet_once {
                    done.push((tab, i, Vec::new()));
                } else {
                    b.quiet_once = true;
                }
                continue;
            }
            let target = b.target.unwrap_or(received).max(b.received_pre + 1);
            b.target = Some(target);
            if r.settled < target {
                continue; // the watcher hasn't run `git status` on those events yet
            }
            let after = r.status.as_deref();
            done.push((tab, i, newly_changed(&b.root, b.before.as_deref(), after)));
        }
    }
    if done.is_empty() {
        return false;
    }
    let mut changed = false;
    // Remove resolved calls back to front per tab (indices stay valid).
    done.sort_by_key(|d| (d.0, std::cmp::Reverse(d.1)));
    for (tab, i, paths) in done {
        let b = g.tabs.get_mut(&tab).unwrap().bash.remove(i);
        let t_post = b.t_post.unwrap_or(now);
        // Writes another tab made itself (file tools, its own commands) around this window aren't ours.
        let others: HashSet<&PathBuf> = g
            .recent
            .iter()
            .filter(|(_, t, owner)| *owner != tab && *t + DEBOUNCE >= b.t_pre && *t <= now)
            .map(|(p, _, _)| p)
            .collect();
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| !others.contains(p)).collect();
        if paths.is_empty() {
            continue;
        }
        // Another tab's command running at the same time in the same repo: can't tell whose → both.
        let mut owners = vec![tab];
        for (&o, t) in &g.tabs {
            if o != tab
                && t.bash.iter().any(|x| x.root == b.root && x.t_pre <= t_post && x.t_post.is_none_or(|p| p >= b.t_pre))
            {
                owners.push(o);
            }
        }
        for o in owners {
            let t = g.tabs.get_mut(&o).unwrap();
            for p in &paths {
                if !t.files.contains_key(p) {
                    t.files.insert(p.clone(), Touch { via_command: true });
                    changed = true;
                }
            }
            g.recent.extend(paths.iter().map(|p| (p.clone(), t_post, o)));
        }
    }
    if changed {
        g.sets_dirty = true;
    }
    changed
}

/// Files whose change (kind or +/- counts) differs between the two statuses, absolute.
fn newly_changed(root: &Path, before: Option<&Status>, after: Option<&Status>) -> Vec<PathBuf> {
    let Some(after) = after else { return Vec::new() };
    type Change = (char, Option<(u32, u32)>);
    let prev: HashMap<&str, Change> =
        before.map(|b| b.files.iter().map(|f| (f.path.as_str(), (f.kind, f.lines))).collect()).unwrap_or_default();
    after
        .files
        .iter()
        .filter(|f| prev.get(f.path.as_str()) != Some(&(f.kind, f.lines)))
        .map(|f| root.join(&f.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/r";

    fn hook(event: &str, session: &str, tool: Option<&str>, id: Option<&str>, path: Option<&str>) -> Kind {
        Kind::Hook {
            event: event.into(),
            notification_type: None,
            session_id: Some(session.into()),
            cwd: Some(ROOT.into()),
            message: None,
            tool: tool.map(Into::into),
            tool_use_id: id.map(Into::into),
            path: path.map(|p| format!("{ROOT}/{p}")),
            title: None,
        }
    }

    fn edit(p: &str) -> Kind {
        hook("PostToolUse", "s", Some("Edit"), Some("e"), Some(p))
    }

    fn bash(event: &str, id: &str) -> Kind {
        hook(event, "s", Some("Bash"), Some(id), None)
    }

    fn st(files: &[(&str, char, u32, u32)]) -> Option<Arc<Status>> {
        Some(Arc::new(Status {
            files: files
                .iter()
                .map(|&(p, kind, a, d)| FileChange {
                    path: p.into(),
                    orig: None,
                    kind,
                    staged: false,
                    unstaged: true,
                    lines: Some((a, d)),
                })
                .collect(),
            ..Status::default()
        }))
    }

    /// A tracker with tabs 1 and 2 in `/r`, a status, and the watcher's event counter.
    fn setup() -> (Arc<Tracker>, Arc<AtomicU64>, Instant) {
        let (t, _rx) = Tracker::new();
        let c = Arc::new(AtomicU64::new(0));
        t.add_repo(Path::new(ROOT), c.clone());
        t.set_repo(1, Some(ROOT.into()));
        t.set_repo(2, Some(ROOT.into()));
        (t, c, Instant::now())
    }

    fn files(t: &Tracker, tab: u32) -> Vec<(String, Mark)> {
        let mut v: Vec<_> = t.view(tab).files.iter().map(|(k, m)| (k.clone(), *m)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    const EXACT: Mark = Mark { shared: false, via_command: false };
    const CMD: Mark = Mark { shared: false, via_command: true };

    #[test]
    fn file_tools_summary_and_lifetime() {
        let (t, _, now) = setup();
        t.status_at(Path::new(ROOT), st(&[("a.rs", 'M', 3, 1), ("b.rs", 'M', 10, 0)]), 0, now);
        t.hook_at(1, &edit("a.rs"), now);
        assert_eq!(t.view(1).summary, Some((1, 3, 1)), "only the agent's file");
        assert_eq!(files(&t, 1), [("a.rs".into(), EXACT)]);
        // Reverted: out of the view, still in the set; changed again: back.
        t.status_at(Path::new(ROOT), st(&[("b.rs", 'M', 10, 0)]), 0, now);
        assert_eq!(t.view(1).summary, None);
        assert!(t.view(1).touched.contains(Path::new("/r/a.rs")));
        t.status_at(Path::new(ROOT), st(&[("a.rs", 'M', 1, 0)]), 0, now);
        assert_eq!(t.view(1).summary, Some((1, 1, 0)));
        // --resume keeps the session id and the set; a new session starts empty.
        t.hook_at(1, &hook("SessionStart", "s", None, None, None), now);
        assert_eq!(t.view(1).summary, Some((1, 1, 0)));
        t.hook_at(1, &hook("SessionStart", "other", None, None, None), now);
        assert_eq!(t.view(1).summary, None);
        assert!(t.view(1).touched.is_empty());
        // Persisted and restored.
        t.hook_at(1, &hook("PostToolUse", "other", Some("Write"), Some("w"), Some("a.rs")), now);
        let saved = t.saved(1).unwrap();
        assert_eq!(saved.session.as_deref(), Some("other"));
        let (t2, _) = Tracker::new();
        t2.set_repo(7, Some(ROOT.into()));
        t2.status_at(Path::new(ROOT), st(&[("a.rs", 'M', 1, 0)]), 0, now);
        t2.restore(7, saved);
        assert_eq!(t2.view(7).summary, Some((1, 1, 0)));
        t2.clear(7);
        assert_eq!(t2.view(7).summary, None);
    }

    #[test]
    fn bash_attribution_waits_for_the_watcher() {
        let (t, c, now) = setup();
        let s = |sec: u64| now + Duration::from_millis(sec);
        t.status_at(Path::new(ROOT), st(&[("old.rs", 'M', 1, 1)]), 0, now);
        t.hook_at(1, &bash("PreToolUse", "b1"), s(10));
        // The command writes new.rs and changes old.rs again; events arrive, the status isn't refreshed yet.
        c.fetch_add(2, Ordering::Relaxed);
        t.hook_at(1, &bash("PostToolUse", "b1"), s(20));
        assert_eq!(t.view(1).summary, None, "PostToolUse before the refresh: nothing yet");
        // A refresh that doesn't cover those events yet changes nothing.
        t.status_at(Path::new(ROOT), st(&[("old.rs", 'M', 1, 1)]), 1, s(100));
        assert_eq!(t.view(1).summary, None);
        t.status_at(Path::new(ROOT), st(&[("old.rs", 'M', 5, 1), ("new.rs", '?', 2, 0)]), 2, s(200));
        assert_eq!(files(&t, 1), [("new.rs".into(), CMD), ("old.rs".into(), CMD)]);
        assert_eq!(files(&t, 2), [], "the other tab is untouched");
    }

    #[test]
    fn bash_that_writes_nothing_resolves_empty() {
        let (t, _, now) = setup();
        t.status_at(Path::new(ROOT), st(&[("x.rs", 'M', 1, 0)]), 0, now);
        t.hook_at(1, &bash("PreToolUse", "b1"), now);
        t.hook_at(1, &bash("PostToolUse", "b1"), now);
        t.hook_at(1, &hook("Stop", "s", None, None, None), now);
        assert!(t.inner.lock().unwrap().tabs[&1].bash.is_empty(), "settled at the next hook");
        // A later change by someone else isn't pinned on it.
        t.status_at(Path::new(ROOT), st(&[("x.rs", 'M', 1, 0), ("later.rs", '?', 1, 0)]), 1, now);
        assert_eq!(files(&t, 1), []);
    }

    #[test]
    fn two_agents_in_one_repo() {
        let (t, c, now) = setup();
        let s = |ms: u64| now + Duration::from_millis(ms);
        t.status_at(Path::new(ROOT), st(&[]), 0, now);
        // Tab 2 edits b.rs (file tool) while tab 1's command runs: b.rs isn't tab 1's.
        t.hook_at(1, &bash("PreToolUse", "b1"), s(0));
        t.hook_at(2, &edit("b.rs"), s(5));
        c.fetch_add(2, Ordering::Relaxed);
        t.hook_at(1, &bash("PostToolUse", "b1"), s(10));
        t.status_at(Path::new(ROOT), st(&[("a.rs", '?', 1, 0), ("b.rs", 'M', 1, 0)]), 2, s(200));
        assert_eq!(files(&t, 1), [("a.rs".into(), CMD)]);
        assert_eq!(files(&t, 2), [("b.rs".into(), EXACT)]);
        // Overlapping commands in both tabs: the new file goes to both, shared.
        t.hook_at(1, &bash("PreToolUse", "b2"), s(300));
        t.hook_at(2, &bash("PreToolUse", "c1"), s(310));
        c.fetch_add(1, Ordering::Relaxed);
        t.hook_at(1, &bash("PostToolUse", "b2"), s(320));
        t.hook_at(2, &bash("PostToolUse", "c1"), s(330));
        t.status_at(Path::new(ROOT), st(&[("a.rs", '?', 1, 0), ("b.rs", 'M', 1, 0), ("c.rs", '?', 1, 0)]), 3, s(500));
        let shared = Mark { shared: true, via_command: true };
        assert!(files(&t, 1).contains(&("c.rs".into(), shared)), "{:?}", files(&t, 1));
        assert!(files(&t, 2).contains(&("c.rs".into(), shared)), "{:?}", files(&t, 2));
        // Both agents editing the same file with file tools: shared, exact.
        t.hook_at(1, &edit("b.rs"), s(600));
        assert!(files(&t, 2).contains(&("b.rs".into(), Mark { shared: true, via_command: false })));
    }

    #[test]
    fn a_file_tool_upgrades_a_command_attribution() {
        let (t, c, now) = setup();
        t.status_at(Path::new(ROOT), st(&[]), 0, now);
        t.hook_at(1, &bash("PreToolUse", "b"), now);
        c.fetch_add(1, Ordering::Relaxed);
        t.hook_at(1, &bash("PostToolUse", "b"), now);
        t.status_at(Path::new(ROOT), st(&[("a.rs", '?', 1, 0)]), 1, now);
        assert_eq!(files(&t, 1), [("a.rs".into(), CMD)]);
        t.hook_at(1, &edit("a.rs"), now);
        assert_eq!(files(&t, 1), [("a.rs".into(), EXACT)]);
    }

    /// The real path: a git repo, its inotify watcher feeding the tracker, a command writing a file between
    /// simulated Pre/Post hooks. Attribution lands after the watcher's debounce; a command writing nothing stays
    /// empty.
    #[test]
    fn real_watcher() {
        use std::process::Command;
        let d = std::env::temp_dir().join(format!("dirigent-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let git::Discovery::Repo(repo) = git::discover(&d) else { panic!() };
        let root = repo.root.clone();
        let (t, _rx) = Tracker::new();
        let t2 = t.clone();
        let root2 = root.clone();
        let w = git::Watcher::start(repo, move |u| t2.status(&root2, u.status, u.settled)).unwrap();
        t.add_repo(&root, w.received());
        t.set_repo(1, Some(root.clone()));
        let wait = |what: &str, ok: &dyn Fn() -> bool| {
            let start = Instant::now();
            while !ok() {
                assert!(start.elapsed() < Duration::from_secs(5), "timed out: {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait("first status", &|| t.inner.lock().unwrap().repos.get(&root).is_some_and(|r| r.status.is_some()));
        let h = |event: &str, id: &str| Kind::Hook {
            event: event.into(),
            notification_type: None,
            session_id: Some("s".into()),
            cwd: None,
            message: None,
            tool: Some("Bash".into()),
            tool_use_id: Some(id.into()),
            path: None,
            title: None,
        };
        t.hook(1, &h("PreToolUse", "b1"));
        std::fs::write(d.join("made.txt"), "x\n").unwrap();
        t.hook(1, &h("PostToolUse", "b1"));
        wait("attributed", &|| t.view(1).files.contains_key("made.txt"));
        assert_eq!(t.view(1).files["made.txt"], CMD);
        // A command that writes nothing.
        t.hook(1, &h("PreToolUse", "b2"));
        t.hook(1, &h("PostToolUse", "b2"));
        t.hook(1, &h("Stop", "-"));
        assert!(t.inner.lock().unwrap().tabs[&1].bash.is_empty());
        assert_eq!(t.view(1).files.len(), 1);
        drop(w);
        let _ = std::fs::remove_dir_all(&d);
    }
}
