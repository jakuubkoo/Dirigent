//! A project's file list for quick open and content search: built once off the UI thread with the file tree's
//! ignore rules, then patched from the repo watcher's changed directories (no watcher of its own).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::fs::walker;

/// One file, relative to the index root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated relative path.
    pub rel: String,
    /// Byte offset of the file name in `rel`.
    pub name: usize,
    /// `rel` lowercased (ASCII), for matching without allocating.
    pub lower: Box<[u8]>,
}

impl Entry {
    pub fn new(rel: String) -> Self {
        let name = rel.rfind('/').map_or(0, |i| i + 1);
        let lower = rel.as_bytes().to_ascii_lowercase().into_boxed_slice();
        Self { rel, name, lower }
    }
}

/// More files than this are not indexed (a note says so).
pub const CAP: usize = 300_000;

pub struct Index {
    pub root: PathBuf,
    /// Includes gitignored files (not kept live: ignored directories aren't watched).
    pub ignored: bool,
    /// Sorted by `rel`. Replaced, never mutated in place: searches keep their snapshot.
    pub files: Arc<Vec<Entry>>,
    /// The cap was hit.
    pub capped: bool,
}

/// The walker for an index: the tree's rules, or no ignore rules at all (`.git` is always skipped).
fn rules(dir: &Path, ignored: bool) -> ignore::WalkBuilder {
    let mut b = walker(dir);
    if ignored {
        b.git_ignore(false).git_exclude(false).git_global(false);
    }
    b
}

fn is_file(e: &ignore::DirEntry) -> bool {
    e.file_type().is_some_and(|t| t.is_file() || (t.is_symlink() && e.path().is_file()))
}

/// A relative path with `/` (git's form, what the index holds) also on Windows.
fn slashed(rel: &Path) -> String {
    let s = rel.to_string_lossy();
    if cfg!(windows) { s.replace('\\', "/") } else { s.into_owned() }
}

/// Every file under `dir` (relative to `root`), walked in parallel, at most `cap`.
fn walk(root: &Path, dir: &Path, ignored: bool, cap: usize) -> (Vec<String>, bool) {
    let found = Mutex::new(Vec::new());
    let count = AtomicUsize::new(0);
    rules(dir, ignored).build_parallel().run(|| {
        let (found, count) = (&found, &count);
        Box::new(move |e| {
            let Ok(e) = e else { return ignore::WalkState::Continue };
            if is_file(&e)
                && let Ok(rel) = e.path().strip_prefix(root)
            {
                if count.fetch_add(1, Ordering::Relaxed) >= cap {
                    return ignore::WalkState::Quit;
                }
                found.lock().unwrap().push(slashed(rel));
            }
            ignore::WalkState::Continue
        })
    });
    let capped = count.load(Ordering::Relaxed) > cap;
    (found.into_inner().unwrap(), capped)
}

impl Index {
    /// Blocking: walks the whole tree. Run off the UI thread.
    pub fn build(root: &Path, ignored: bool, cap: usize) -> Self {
        let (mut rels, capped) = walk(root, root, ignored, cap);
        rels.sort_unstable();
        Self { root: root.to_path_buf(), ignored, files: Arc::new(rels.into_iter().map(Entry::new).collect()), capped }
    }

    /// Applies the watcher's changed directories (parents of created / removed / renamed entries): each is
    /// re-listed; a directory that appeared is walked, one that vanished drops everything under it. Blocking (IO):
    /// call from the watcher thread, never the UI. Returns whether the list changed.
    pub fn refresh(&mut self, dirs: &[PathBuf]) -> bool {
        let mut drop_prefix: Vec<String> = Vec::new(); // "a/b/" (or "" for the root)
        let mut add: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for dir in dirs {
            let Ok(rel_dir) = dir.strip_prefix(&self.root) else { continue };
            if !seen.insert(rel_dir.to_path_buf()) {
                continue;
            }
            let prefix = if rel_dir.as_os_str().is_empty() { String::new() } else { format!("{}/", slashed(rel_dir)) };
            if !dir.is_dir() {
                drop_prefix.push(prefix);
                continue;
            }
            // The directory's direct files now, and its subdirectories.
            let (mut files, mut subdirs) = (Vec::new(), Vec::new());
            for e in rules(dir, self.ignored).max_depth(Some(1)).build().flatten().filter(|e| e.depth() == 1) {
                let name = e.file_name().to_string_lossy().into_owned();
                if is_file(&e) {
                    files.push(format!("{prefix}{name}"));
                } else if e.file_type().is_some_and(|t| t.is_dir()) {
                    subdirs.push(format!("{prefix}{name}/"));
                }
            }
            // Direct files are replaced; subdirectories that are gone are dropped; new ones are walked.
            drop_prefix.push(format!("{prefix}\0direct"));
            add.extend(files);
            for sub in subdirs {
                let known = self.files.iter().any(|f| f.rel.starts_with(&sub));
                if !known {
                    let (found, _) = walk(&self.root, &self.root.join(&sub), self.ignored, CAP);
                    add.extend(found);
                }
            }
            // Known subdirectories that are gone (removed or renamed away).
            let gone: HashSet<String> = self
                .files
                .iter()
                .filter_map(|f| f.rel.strip_prefix(&prefix)?.split_once('/').map(|(d, _)| d.to_string()))
                .filter(|d| !dir.join(d).is_dir())
                .collect();
            drop_prefix.extend(gone.into_iter().map(|d| format!("{prefix}{d}/")));
        }
        if drop_prefix.is_empty() && add.is_empty() {
            return false;
        }
        let dropped = |rel: &str| {
            drop_prefix.iter().any(|p| match p.strip_suffix("\0direct") {
                // Direct children of that directory only.
                Some(dir) => rel.strip_prefix(dir).is_some_and(|r| !r.contains('/')),
                None => rel.starts_with(p.as_str()),
            })
        };
        let mut rels: Vec<String> =
            self.files.iter().filter(|f| !dropped(&f.rel)).map(|f| f.rel.clone()).chain(add).collect();
        rels.sort_unstable();
        rels.dedup();
        let changed = rels.len() != self.files.len() || rels.iter().zip(self.files.iter()).any(|(a, b)| *a != b.rel);
        if changed {
            self.files = Arc::new(rels.into_iter().map(Entry::new).collect());
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dirigent-index-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // A repo, so .gitignore applies.
        std::fs::create_dir_all(d.join(".git")).unwrap();
        d
    }

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    fn rels(ix: &Index) -> Vec<&str> {
        ix.files.iter().map(|f| f.rel.as_str()).collect()
    }

    #[test]
    fn build_respects_gitignore_unless_asked() {
        let d = tmp("build");
        std::fs::write(d.join(".gitignore"), "target/\n*.log\n").unwrap();
        for f in ["src/main.rs", "src/lib/mod.rs", "README.md", "target/debug/x", "a.log", ".env"] {
            write(&d, f);
        }
        let ix = Index::build(&d, false, CAP);
        assert_eq!(rels(&ix), [".env", ".gitignore", "README.md", "src/lib/mod.rs", "src/main.rs"]);
        assert_eq!(ix.files[3].name, "src/lib/".len());
        assert_eq!(&*ix.files[2].lower, b"readme.md");
        let all = Index::build(&d, true, CAP);
        assert!(rels(&all).contains(&"target/debug/x") && rels(&all).contains(&"a.log"));
        assert!(!rels(&all).iter().any(|r| r.starts_with(".git/")), ".git is never listed");
        // The cap.
        let capped = Index::build(&d, false, 2);
        assert!(capped.capped && capped.files.len() == 2);
    }

    #[test]
    fn refresh_follows_creates_renames_and_deletes() {
        let d = tmp("refresh");
        std::fs::write(d.join(".gitignore"), "*.log\n").unwrap();
        for f in ["a.rs", "src/b.rs", "src/deep/c.rs", "old/x.rs"] {
            write(&d, f);
        }
        let mut ix = Index::build(&d, false, CAP);
        // New file, new directory with files, ignored file, deleted file, renamed directory.
        write(&d, "src/new.rs");
        write(&d, "fresh/sub/y.rs");
        write(&d, "src/z.log");
        std::fs::remove_file(d.join("a.rs")).unwrap();
        std::fs::rename(d.join("old"), d.join("renamed")).unwrap();
        // What the watcher reports: the parents of the changed entries.
        assert!(ix.refresh(&[d.join("src"), d.clone()]));
        assert_eq!(
            rels(&ix),
            [".gitignore", "fresh/sub/y.rs", "renamed/x.rs", "src/b.rs", "src/deep/c.rs", "src/new.rs"]
        );
        // A removed directory reported itself (its parent's listing was not re-read).
        std::fs::remove_dir_all(d.join("src/deep")).unwrap();
        assert!(ix.refresh(&[d.join("src/deep")]));
        assert!(!rels(&ix).contains(&"src/deep/c.rs"));
        // Nothing changed: no new list.
        let before = ix.files.clone();
        assert!(!ix.refresh(&[d.join("src")]));
        assert!(Arc::ptr_eq(&before, &ix.files));
    }
}
