//! Directory listings for the file tree, and a watcher for expanded directories outside any repo.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Instant;

use notify::{Config, Event, EventKindMask, RecursiveMode, Watcher as _};

use crate::watch::{MAX_WAIT, QUIET, changes_listing};

/// The gitignore rules used everywhere (watching and listing): nested .gitignore files, `.git/info/exclude` and
/// the global excludes file; hidden files are not special; `.git` itself is skipped.
pub(crate) fn walker(dir: &Path) -> ignore::WalkBuilder {
    let mut b = ignore::WalkBuilder::new(dir);
    b.hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(|e| e.file_name() != ".git");
    b
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    /// Directories and symlinks to directories.
    pub is_dir: bool,
    /// Matched by a gitignore rule (only inside a git repo).
    pub ignored: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    /// Directories first, then files; case-insensitive by name. No `.git`.
    pub entries: Vec<Entry>,
}

/// One directory's entries. Blocking (readdir + gitignore matching, no `git` calls): run off the UI thread.
pub fn list_dir(dir: &Path) -> std::io::Result<Listing> {
    let kept: HashSet<PathBuf> =
        walker(dir).max_depth(Some(1)).build().flatten().filter(|e| e.depth() == 1).map(|e| e.into_path()).collect();
    let mut entries = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = e.path();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()));
        let ignored = !kept.contains(&path);
        entries.push(Entry { name, path, is_dir, ignored });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
    Ok(Listing { entries })
}

/// Case-insensitive with numbers compared by value: `file_2` < `file_10`. Ties (only case or leading zeros differ)
/// fall back to plain byte order so the result is stable.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        n.push(c);
                    }
                    n
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let ord = m.len().cmp(&n.len()).then_with(|| m.cmp(n)); // by value, any length
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

enum Msg {
    Watch(PathBuf),
    Unwatch(PathBuf),
    Changed(Vec<PathBuf>),
    Stop,
}

/// Watches individual directories (NonRecursive, refcounted) for entries appearing, disappearing or being renamed:
/// the expanded directories of file trees that aren't in a repo (a repo's `Watcher` covers its own). One notify
/// instance and thread for the whole app; watch/unwatch never block the caller.
pub struct DirWatcher {
    tx: Sender<Msg>,
    watched: Arc<AtomicUsize>,
}

impl DirWatcher {
    /// `on_change(dirs)`: directories whose listing changed, debounced like the repo watcher.
    pub fn start(on_change: impl Fn(Vec<PathBuf>) + Send + 'static) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let watched = Arc::new(AtomicUsize::new(0));
        let (tx2, watched2) = (tx.clone(), watched.clone());
        std::thread::Builder::new().name("dir-watch".into()).spawn(move || {
            let handler = move |res: notify::Result<Event>| {
                let Ok(ev) = res else { return };
                if changes_listing(&ev.kind) {
                    let dirs = ev.paths.iter().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
                    let _ = tx2.send(Msg::Changed(dirs));
                }
            };
            let config = Config::default().with_event_kinds(EventKindMask::CORE);
            let Ok(mut w) = notify::RecommendedWatcher::new(handler, config) else { return };
            let mut counts: HashMap<PathBuf, usize> = HashMap::new();
            let mut dirty: HashSet<PathBuf> = HashSet::new();
            let mut burst: Option<Instant> = None;
            loop {
                let msg = match burst {
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    Some(_) => rx.recv_timeout(QUIET),
                };
                let timed_out = matches!(msg, Err(RecvTimeoutError::Timeout));
                match msg {
                    Ok(Msg::Watch(d)) => {
                        let n = counts.entry(d.clone()).or_default();
                        *n += 1;
                        if *n == 1 {
                            let _ = w.watch(&d, RecursiveMode::NonRecursive);
                        }
                    }
                    Ok(Msg::Unwatch(d)) => {
                        if let Some(n) = counts.get_mut(&d) {
                            *n -= 1;
                            if *n == 0 {
                                counts.remove(&d);
                                let _ = w.unwatch(&d);
                            }
                        }
                    }
                    Ok(Msg::Changed(dirs)) => {
                        dirty.extend(dirs.into_iter().filter(|d| counts.contains_key(d)));
                        burst.get_or_insert_with(Instant::now);
                    }
                    Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                watched2.store(counts.len(), Ordering::Relaxed);
                let due = burst.is_some_and(|t| t.elapsed() >= MAX_WAIT) || timed_out;
                if due {
                    burst = None;
                    if !dirty.is_empty() {
                        on_change(dirty.drain().collect());
                    }
                }
            }
        })?;
        Ok(Self { tx, watched })
    }

    pub fn watch(&self, dir: &Path) {
        let _ = self.tx.send(Msg::Watch(dir.to_path_buf()));
    }

    pub fn unwatch(&self, dir: &Path) {
        let _ = self.tx.send(Msg::Unwatch(dir.to_path_buf()));
    }

    /// Directories currently watched (tests/perf; updated by the watcher thread).
    pub fn watched(&self) -> usize {
        self.watched.load(Ordering::Relaxed)
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;

    #[test]
    fn natural_order() {
        let mut v = vec![
            "file_10.txt",
            "File_2.txt",
            "file_1.txt",
            "file_100.txt",
            "a",
            "B",
            "v1.10",
            "v1.9",
            "x007",
            "x7",
            "99999999999999999999999",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            vec![
                "99999999999999999999999",
                "a",
                "B",
                "file_1.txt",
                "File_2.txt",
                "file_10.txt",
                "file_100.txt",
                "v1.9",
                "v1.10",
                "x007",
                "x7"
            ]
        );
    }
}
