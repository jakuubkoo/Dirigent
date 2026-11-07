//! One watcher thread per repo: inotify watches on every non-ignored directory (never on `target/`,
//! `node_modules/`, …) plus the few `.git` entries that change the status, a debounce, and a `git status` only
//! when a relevant path changed. Idle = the thread and notify's thread are blocked: no CPU, no timers.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, EventKindMask, RecursiveMode, Watcher as _};

use crate::{Repo, Status, git, status};

/// Quiet time after the last event before refreshing.
pub(crate) const QUIET: Duration = Duration::from_millis(150);
/// A steady stream of events still refreshes this often (a throttle: it can't postpone the refresh forever).
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(1);

/// Entries appeared, disappeared or were renamed (a directory listing changed); content writes don't count.
pub(crate) fn changes_listing(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Change {
    /// Created or moved in (a new directory must get its own watch).
    created: bool,
    /// The parent directory's listing changed.
    listing: bool,
    /// Windows: a content/metadata change (not a listing change). For a directory that is only its last-write
    /// time moving because something inside changed, which that directory's own watch already reported.
    #[cfg(windows)]
    content: bool,
    /// Windows: removed or renamed away (a watched directory leaves `Watched`).
    #[cfg(windows)]
    gone: bool,
}

enum Ev {
    Paths(Vec<(PathBuf, Change)>),
    Refresh,
    /// Something changed in these directories (reported from outside: a repo inside WSL has no watches).
    Touched(Vec<PathBuf>),
    Stop,
}

/// A repo inside a WSL distro gets no watches: Windows change notifications over WSL's 9P share miss what Linux
/// processes write. It refreshes on `refresh`/`touched` (agent steps, shell prompts, the user's click).
const UNWATCHED: &str = "Inside WSL: refreshes after agent steps and prompts. Click to refresh.";

/// What one debounced batch of events produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update {
    /// `git status` ran.
    pub refreshed: bool,
    /// ...and its result differs from the previous one.
    pub status_changed: bool,
    /// Watched directories whose entries changed (created, removed, renamed; ignored entries included), for file
    /// trees. Ignored directories are never watched, so nothing below them shows up here.
    pub dirs: Vec<PathBuf>,
    /// The status after this batch (also when unchanged); None when `git status` didn't run.
    pub status: Option<Arc<Status>>,
    /// How many event batches (see `Watcher::received`) this update accounts for: once an update's `settled`
    /// reaches a `received` value read earlier, everything that happened before that read is reflected.
    pub settled: u64,
}

pub struct Watcher {
    tx: Sender<Ev>,
    latest: Arc<Mutex<Option<Arc<Status>>>>,
    refreshes: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
    /// A repo inside WSL keeps its distro's git server up (`wsl::Hold`).
    _server: Option<crate::wsl::Hold>,
}

impl Watcher {
    /// Starts watching. `on_update` runs on the watcher thread after every `git status` (the first one right away)
    /// and for batches that only changed directory listings. An unchanged status can still mean a file's content
    /// changed (same +/- counts), so an open diff should be re-read on every refresh.
    pub fn start(repo: Repo, on_update: impl Fn(Update) + Send + 'static) -> std::io::Result<Self> {
        let root_for_hold = repo.root.clone();
        let (tx, rx) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let refreshes = Arc::new(AtomicU64::new(0));
        let received = Arc::new(AtomicU64::new(0));
        let (latest2, refreshes2, tx2, received2) = (latest.clone(), refreshes.clone(), tx.clone(), received.clone());
        std::thread::Builder::new().name("git-watch".into()).spawn(move || {
            let (git_dir, common) = (repo.git_dir.clone(), shared(&repo));
            let handler = move |res: notify::Result<Event>| {
                let ev = match res {
                    Ok(ev) if !ev.need_rescan() => ev,
                    _ => {
                        let _ = tx2.send(Ev::Refresh); // queue overflow etc.: we may have missed something
                        return;
                    }
                };
                let change = Change {
                    created: matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))),
                    listing: changes_listing(&ev.kind),
                    #[cfg(windows)]
                    content: matches!(
                        ev.kind,
                        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)
                    ),
                    #[cfg(windows)]
                    gone: matches!(
                        ev.kind,
                        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::From))
                    ),
                };
                let paths: Vec<_> = ev
                    .paths
                    .into_iter()
                    .filter(|p| counts(p, &git_dir, common.as_deref()))
                    .map(|p| (p, change))
                    .collect();
                if !paths.is_empty() {
                    received2.fetch_add(1, Ordering::Relaxed);
                    let _ = tx2.send(Ev::Paths(paths));
                }
            };
            // CORE = no access events: our own `git status` reading files must not look like a change.
            let config = Config::default().with_event_kinds(EventKindMask::CORE);
            let Ok(mut w) = notify::RecommendedWatcher::new(handler, config) else { return };
            let in_git = |p: &Path| p.starts_with(&repo.git_dir) || shared(&repo).is_some_and(|c| p.starts_with(c));
            let unwatched = crate::wsl::of(&repo.root).is_some();
            let mut watch_error = unwatched.then(|| UNWATCHED.to_string());
            let mut watched = Watched::default();
            if !unwatched {
                let _ = w.watch(&repo.git_dir, RecursiveMode::NonRecursive);
                // All of refs/ (heads, and remotes for ahead/behind after fetch/push; remotes/ may not exist yet).
                let refs = repo.git_dir.join("refs");
                if refs.is_dir() {
                    let _ = w.watch(&refs, RecursiveMode::Recursive);
                }
                // A linked worktree: branches live in the shared `.git` (a commit made in another checkout moves them).
                if let Some(common) = shared(&repo) {
                    let _ = w.watch(&common, RecursiveMode::NonRecursive);
                    let refs = common.join("refs");
                    if refs.is_dir() {
                        let _ = w.watch(&refs, RecursiveMode::Recursive);
                    }
                }
                // Every directory we watch (Windows only; see `Change::content`). Paths in events are built from the
                // watched paths, so an exact lookup matches: no syscall per event.
                #[cfg(windows)]
                for g in [Some(repo.git_dir.clone()), shared(&repo)].into_iter().flatten() {
                    for d in ["", "refs", "refs/heads", "refs/remotes"] {
                        watched.0.insert(g.join(d).components().collect());
                    }
                }
                let started = Instant::now();
                let n = watch_tree(&mut w, &repo.root, &mut watch_error, &mut watched);
                crate::perf(|| format!("git watch {}: {n} dirs in {:.1} ms", repo.root.display(), ms(started)));
            }

            let mut last: Option<Arc<Status>> = None;
            let (mut pending, mut force, mut first) = (Vec::new(), true, true);
            let mut touched: Vec<PathBuf> = Vec::new();
            // Event batches taken off the channel (the channel is FIFO, so this trails `received` exactly).
            let mut consumed = 0u64;
            loop {
                if !force {
                    match rx.recv() {
                        Ok(Ev::Paths(p)) => {
                            consumed += 1;
                            pending.extend(p)
                        }
                        Ok(Ev::Refresh) => force = true,
                        Ok(Ev::Touched(d)) => {
                            consumed += 1;
                            force = true;
                            touched.extend(d)
                        }
                        Ok(Ev::Stop) | Err(_) => return,
                    }
                }
                if !first {
                    let burst = Instant::now();
                    while burst.elapsed() < MAX_WAIT {
                        match rx.recv_timeout(QUIET) {
                            Ok(Ev::Paths(p)) => {
                                consumed += 1;
                                pending.extend(p)
                            }
                            Ok(Ev::Refresh) => force = true,
                            Ok(Ev::Touched(d)) => {
                                consumed += 1;
                                force = true;
                                touched.extend(d)
                            }
                            Ok(Ev::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                            Err(RecvTimeoutError::Timeout) => break,
                        }
                    }
                }
                first = false;
                let paths: HashSet<(PathBuf, Change)> = std::mem::take(&mut pending).into_iter().collect();
                #[cfg(windows)]
                for (p, c) in &paths {
                    if c.gone {
                        watched.0.remove(p);
                    }
                }
                #[cfg(windows)]
                let paths: HashSet<(PathBuf, Change)> =
                    paths.into_iter().filter(|(p, c)| !(c.content && watched.0.contains(p))).collect();
                let git_hit = paths.iter().any(|(p, _)| in_git(p));
                crate::perf(|| {
                    let mut v: Vec<_> = paths.iter().map(|(p, _)| p.display().to_string()).collect();
                    v.sort();
                    format!("git event batch: {}", v.join(", "))
                });
                let work: Vec<&(PathBuf, Change)> = paths.iter().filter(|(p, _)| !in_git(p)).collect();
                let dirs: Vec<PathBuf> = work
                    .iter()
                    .filter(|(_, c)| c.listing)
                    .filter_map(|(p, _)| p.parent().map(Path::to_path_buf))
                    .chain(std::mem::take(&mut touched))
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let ignored = if work.is_empty() {
                    HashSet::new()
                } else {
                    check_ignore(&repo.root, work.iter().map(|(p, _)| p.as_path()))
                };
                let relevant: Vec<_> = work.into_iter().filter(|(p, _)| !ignored.contains(p)).collect();
                for (p, c) in &relevant {
                    if c.created && p.is_dir() {
                        watch_tree(&mut w, p, &mut watch_error, &mut watched);
                    }
                }
                if !force && !git_hit && relevant.is_empty() {
                    crate::perf(|| format!("git events ignored: {} paths", paths.len()));
                    // Still reported (with the unchanged status): a pending agent command waits for `settled`.
                    on_update(Update { dirs, status: last.clone(), settled: consumed, ..Update::default() });
                    continue;
                }
                force = false;
                refreshes2.fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                let Ok(mut st) = status(&repo) else {
                    on_update(Update { dirs, settled: consumed, ..Update::default() });
                    continue;
                };
                crate::perf(|| format!("git status {}: {:.1} ms", repo.root.display(), ms(started)));
                st.watch_error = watch_error.clone();
                let changed = last.as_deref() != Some(&st);
                if changed {
                    let st = Arc::new(st);
                    *latest2.lock().unwrap() = Some(st.clone());
                    last = Some(st);
                }
                on_update(Update {
                    refreshed: true,
                    status_changed: changed,
                    dirs,
                    status: last.clone(),
                    settled: consumed,
                });
            }
        })?;
        let _server = crate::wsl::Hold::of(&root_for_hold);
        Ok(Self { tx, latest, refreshes, received, _server })
    }

    /// The last status (None until the first one is ready).
    pub fn latest(&self) -> Option<Arc<Status>> {
        self.latest.lock().unwrap().clone()
    }

    /// Re-runs `git status` now (manual refresh when the repo can't be fully watched).
    pub fn refresh(&self) {
        let _ = self.tx.send(Ev::Refresh);
    }

    /// Something changed in `dirs` (empty: somewhere): re-runs `git status` and reports the dirs to file trees.
    /// For repos without watches (inside WSL); counts as an event batch for `received`/`settled`.
    pub fn touched(&self, dirs: Vec<PathBuf>) {
        self.toucher().touch(dirs);
    }

    /// A handle for `touched` from other threads (the agent listener, a session's prompt reports).
    pub fn toucher(&self) -> Toucher {
        Toucher { tx: self.tx.clone(), received: self.received.clone() }
    }

    /// Event batches seen so far (a counter shared with the event handler). Read it, then wait for an update
    /// whose `settled` is at least that value to be sure every change before the read is in the status.
    pub fn received(&self) -> Arc<AtomicU64> {
        self.received.clone()
    }

    /// How many times `git status` ran (perf/tests).
    pub fn refreshes(&self) -> u64 {
        self.refreshes.load(Ordering::Relaxed)
    }
}

/// `Watcher::touched` from anywhere; does nothing once the watcher is gone.
#[derive(Clone)]
pub struct Toucher {
    tx: Sender<Ev>,
    received: Arc<AtomicU64>,
}

impl Toucher {
    pub fn touch(&self, dirs: Vec<PathBuf>) {
        self.received.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Ev::Touched(dirs));
    }
}

impl std::fmt::Debug for Toucher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Toucher")
    }
}

impl Drop for Watcher {
    /// Ends the thread (after a running `git` call, if any), which drops the inotify watches.
    fn drop(&mut self) {
        let _ = self.tx.send(Ev::Stop);
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// A linked worktree's shared git dir (None for a main checkout: its git dir is already watched).
fn shared(repo: &crate::Repo) -> Option<PathBuf> {
    (repo.common_dir != repo.git_dir).then(|| repo.common_dir.clone())
}

/// Inside the git dir only these change what `git status` shows (objects/, logs/, *.lock don't); in a linked
/// worktree's shared dir only the branches (its own index/HEAD are in `git_dir`, which is inside the shared one).
fn counts(path: &Path, git_dir: &Path, common: Option<&Path>) -> bool {
    if let Ok(rel) = path.strip_prefix(git_dir) {
        return rel == Path::new("index")
            || rel == Path::new("HEAD")
            || rel == Path::new("packed-refs")
            || rel.starts_with("refs/heads")
            || rel.starts_with("refs/remotes");
    }
    match common.map(|c| path.strip_prefix(c)) {
        Some(Ok(rel)) => {
            rel == Path::new("packed-refs") || rel.starts_with("refs/heads") || rel.starts_with("refs/remotes")
        }
        _ => true,
    }
}

/// The directories with a watch of their own (filled on Windows only).
#[derive(Default)]
struct Watched(#[cfg(windows)] HashSet<PathBuf>);

/// Watches `dir` and every directory below it that git doesn't ignore (nested .gitignore files, info/exclude and
/// the global excludes file all apply). Returns how many directories got a watch.
fn watch_tree(
    w: &mut notify::RecommendedWatcher,
    dir: &Path,
    error: &mut Option<String>,
    #[cfg_attr(unix, allow(unused_variables))] watched: &mut Watched,
) -> usize {
    let walk = crate::fs::walker(dir).build();
    let mut n = 0;
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_dir()) {
            continue;
        }
        match w.watch(entry.path(), RecursiveMode::NonRecursive) {
            Ok(()) => {
                n += 1;
                #[cfg(windows)]
                watched.0.insert(entry.path().to_path_buf());
            }
            Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => {
                *error = Some("Too many directories to watch (OS inotify limit): changes may be missed".into());
                break;
            }
            Err(_) => {} // vanished or unreadable meanwhile
        }
    }
    n
}

/// Which of `paths` git ignores (one `git check-ignore --stdin` for the whole batch; tracked files never count as
/// ignored). On any error: none.
fn check_ignore<'a>(root: &Path, paths: impl Iterator<Item = &'a Path>) -> HashSet<PathBuf> {
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p.as_os_str().as_encoded_bytes());
        input.push(0);
    }
    let mut c = git(root);
    c.args(["check-ignore", "-z", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let Ok(mut child) = c.spawn() else { return HashSet::new() };
    // Written from another thread: git answers while reading, so a big batch would fill both pipes.
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(&input);
        }
    });
    let mut out = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_end(&mut out);
    }
    let _ = writer.join();
    let _ = child.wait();
    out.split(|&b| b == 0).filter(|p| !p.is_empty()).map(path_from_bytes).collect()
}

#[cfg(unix)]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(b.to_vec()))
}

/// git for Windows speaks UTF-8.
#[cfg(windows)]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}
