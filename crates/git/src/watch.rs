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
}

enum Ev {
    Paths(Vec<(PathBuf, Change)>),
    Refresh,
    Stop,
}

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
}

pub struct Watcher {
    tx: Sender<Ev>,
    latest: Arc<Mutex<Option<Arc<Status>>>>,
    refreshes: Arc<AtomicU64>,
}

impl Watcher {
    /// Starts watching. `on_update` runs on the watcher thread after every `git status` (the first one right away)
    /// and for batches that only changed directory listings. An unchanged status can still mean a file's content
    /// changed (same +/- counts), so an open diff should be re-read on every refresh.
    pub fn start(repo: Repo, on_update: impl Fn(Update) + Send + 'static) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let refreshes = Arc::new(AtomicU64::new(0));
        let (latest2, refreshes2, tx2) = (latest.clone(), refreshes.clone(), tx.clone());
        std::thread::Builder::new().name("git-watch".into()).spawn(move || {
            let git_dir = repo.git_dir.clone();
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
                };
                let paths: Vec<_> = ev.paths.into_iter().filter(|p| counts(p, &git_dir)).map(|p| (p, change)).collect();
                if !paths.is_empty() {
                    let _ = tx2.send(Ev::Paths(paths));
                }
            };
            // CORE = no access events: our own `git status` reading files must not look like a change.
            let config = Config::default().with_event_kinds(EventKindMask::CORE);
            let Ok(mut w) = notify::RecommendedWatcher::new(handler, config) else { return };
            let mut watch_error = None;
            let _ = w.watch(&repo.git_dir, RecursiveMode::NonRecursive);
            let refs = repo.git_dir.join("refs/heads");
            if refs.is_dir() {
                let _ = w.watch(&refs, RecursiveMode::Recursive);
            }
            let started = Instant::now();
            let n = watch_tree(&mut w, &repo.root, &mut watch_error);
            crate::perf(|| format!("git watch {}: {n} dirs in {:.1} ms", repo.root.display(), ms(started)));

            let mut last: Option<Status> = None;
            let (mut pending, mut force, mut first) = (Vec::new(), true, true);
            loop {
                if !force {
                    match rx.recv() {
                        Ok(Ev::Paths(p)) => pending.extend(p),
                        Ok(Ev::Refresh) => force = true,
                        Ok(Ev::Stop) | Err(_) => return,
                    }
                }
                if !first {
                    let burst = Instant::now();
                    while burst.elapsed() < MAX_WAIT {
                        match rx.recv_timeout(QUIET) {
                            Ok(Ev::Paths(p)) => pending.extend(p),
                            Ok(Ev::Refresh) => force = true,
                            Ok(Ev::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                            Err(RecvTimeoutError::Timeout) => break,
                        }
                    }
                }
                first = false;
                let paths: HashSet<(PathBuf, Change)> = std::mem::take(&mut pending).into_iter().collect();
                let git_hit = paths.iter().any(|(p, _)| p.starts_with(&repo.git_dir));
                let work: Vec<&(PathBuf, Change)> = paths.iter().filter(|(p, _)| !p.starts_with(&repo.git_dir)).collect();
                let dirs: Vec<PathBuf> = work
                    .iter()
                    .filter(|(_, c)| c.listing)
                    .filter_map(|(p, _)| p.parent().map(Path::to_path_buf))
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
                        watch_tree(&mut w, p, &mut watch_error);
                    }
                }
                if !force && !git_hit && relevant.is_empty() {
                    crate::perf(|| format!("git events ignored: {} paths", paths.len()));
                    if !dirs.is_empty() {
                        on_update(Update { dirs, ..Update::default() });
                    }
                    continue;
                }
                force = false;
                refreshes2.fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                let Ok(mut st) = status(&repo) else {
                    on_update(Update { dirs, ..Update::default() });
                    continue;
                };
                crate::perf(|| format!("git status {}: {:.1} ms", repo.root.display(), ms(started)));
                st.watch_error = watch_error.clone();
                let changed = last.as_ref() != Some(&st);
                if changed {
                    *latest2.lock().unwrap() = Some(Arc::new(st.clone()));
                    last = Some(st);
                }
                on_update(Update { refreshed: true, status_changed: changed, dirs });
            }
        })?;
        Ok(Self { tx, latest, refreshes })
    }

    /// The last status (None until the first one is ready).
    pub fn latest(&self) -> Option<Arc<Status>> {
        self.latest.lock().unwrap().clone()
    }

    /// Re-runs `git status` now (manual refresh when the repo can't be fully watched).
    pub fn refresh(&self) {
        let _ = self.tx.send(Ev::Refresh);
    }

    /// How many times `git status` ran (perf/tests).
    pub fn refreshes(&self) -> u64 {
        self.refreshes.load(Ordering::Relaxed)
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

/// Inside the git dir only these change what `git status` shows (objects/, logs/, *.lock don't).
fn counts(path: &Path, git_dir: &Path) -> bool {
    match path.strip_prefix(git_dir) {
        Ok(rel) => rel == Path::new("index") || rel == Path::new("HEAD") || rel == Path::new("packed-refs") || rel.starts_with("refs/heads"),
        Err(_) => true,
    }
}

/// Watches `dir` and every directory below it that git doesn't ignore (nested .gitignore files, info/exclude and
/// the global excludes file all apply). Returns how many directories got a watch.
fn watch_tree(w: &mut notify::RecommendedWatcher, dir: &Path, error: &mut Option<String>) -> usize {
    let walk = crate::fs::walker(dir).build();
    let mut n = 0;
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_dir()) {
            continue;
        }
        match w.watch(entry.path(), RecursiveMode::NonRecursive) {
            Ok(()) => n += 1,
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
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p.as_os_str().as_bytes());
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
    out.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(std::ffi::OsString::from_vec(p.to_vec())))
        .collect()
}
