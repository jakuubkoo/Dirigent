//! File tree model (no UI): per-tab expanded/selected state, the flattened visible rows built from cached directory
//! listings, keyboard navigation, and git status marks. Directories are listed only when expanded.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use git::{Entry, Listing, Status};

/// Listings by absolute directory path, shared by all tabs.
pub type Cache = HashMap<PathBuf, Arc<Listing>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TreeState {
    /// Absolute paths of expanded directories (persisted; entries outside the current root are kept but unused).
    pub expanded: BTreeSet<PathBuf>,
    pub selected: Option<PathBuf>,
}

/// One visible row: an entry of a cached listing (no per-row allocation).
#[derive(Clone)]
pub struct Row {
    listing: Arc<Listing>,
    ix: usize,
    pub depth: usize,
    pub expanded: bool,
    /// Ignored itself, or inside an ignored directory (`list_dir` of `target/` alone can't tell).
    pub ignored: bool,
}

impl Row {
    pub fn entry(&self) -> &Entry {
        &self.listing.entries[self.ix]
    }
}

/// Directories whose listing the tree needs: the root and every expanded directory whose ancestors (up to the root)
/// are all expanded too.
pub fn wanted(root: &Path, state: &TreeState) -> Vec<PathBuf> {
    let mut v = vec![root.to_path_buf()];
    for dir in state.expanded.range(root.to_path_buf()..) {
        if !dir.starts_with(root) {
            break; // BTreeSet order: everything under `root` follows it contiguously
        }
        if dir != root && dir.ancestors().skip(1).take_while(|a| *a != root).all(|a| state.expanded.contains(a)) {
            v.push(dir.clone());
        }
    }
    v
}

/// The visible rows, depth-first. An expanded directory without a listing yet shows no children (loading).
pub fn flatten(root: &Path, state: &TreeState, cache: &Cache, show_ignored: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    struct Ctx<'a> {
        state: &'a TreeState,
        cache: &'a Cache,
        show_ignored: bool,
    }
    fn walk(dir: &Path, depth: usize, in_ignored: bool, c: &Ctx, rows: &mut Vec<Row>) {
        let Some(listing) = c.cache.get(dir) else { return };
        for (ix, e) in listing.entries.iter().enumerate() {
            let ignored = in_ignored || e.ignored;
            if ignored && !c.show_ignored {
                continue;
            }
            let expanded = e.is_dir && c.state.expanded.contains(&e.path);
            rows.push(Row { listing: listing.clone(), ix, depth, expanded, ignored });
            if expanded {
                walk(&e.path, depth + 1, ignored, c, rows);
            }
        }
    }
    walk(root, 0, false, &Ctx { state, cache, show_ignored }, &mut rows);
    rows
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp(usize),
    PageDown(usize),
    Enter,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Select(PathBuf),
    Expand(PathBuf),
    Collapse(PathBuf),
    Open(PathBuf),
}

/// What a key does with the current rows and selection.
pub fn key(rows: &[Row], selected: Option<&Path>, k: Key) -> Action {
    if rows.is_empty() {
        return Action::None;
    }
    let cur = selected.and_then(|s| rows.iter().position(|r| r.entry().path == s));
    let select = |i: usize| Action::Select(rows[i.min(rows.len() - 1)].entry().path.clone());
    let Some(i) = cur else {
        // Nothing selected (or it scrolled out of the tree): any key starts at the top.
        return select(0);
    };
    let r = &rows[i];
    let e = r.entry();
    match k {
        Key::Up => select(i.saturating_sub(1)),
        Key::Down => select(i + 1),
        Key::Home => select(0),
        Key::End => select(rows.len() - 1),
        Key::PageUp(n) => select(i.saturating_sub(n)),
        Key::PageDown(n) => select(i + n),
        Key::Right if e.is_dir && !r.expanded => Action::Expand(e.path.clone()),
        Key::Right if r.expanded && rows.get(i + 1).is_some_and(|c| c.depth > r.depth) => select(i + 1),
        Key::Right => Action::None,
        Key::Left if r.expanded => Action::Collapse(e.path.clone()),
        Key::Left => match rows[..i].iter().rposition(|p| p.depth + 1 == r.depth) {
            Some(p) => select(p),
            None => Action::None,
        },
        Key::Enter if e.is_dir && r.expanded => Action::Collapse(e.path.clone()),
        Key::Enter if e.is_dir => Action::Expand(e.path.clone()),
        Key::Enter => Action::Open(e.path.clone()),
    }
}

/// Git status as the tree shows it: a kind letter per changed file, and every directory containing a change.
#[derive(Debug, Default, PartialEq)]
pub struct Marks {
    pub files: HashMap<PathBuf, char>,
    pub dirs: HashSet<PathBuf>,
}

pub fn marks(root: &Path, st: &Status) -> Marks {
    let mut m = Marks::default();
    for f in &st.files {
        let path = root.join(&f.path);
        for a in path.ancestors().skip(1) {
            if a == root || !m.dirs.insert(a.to_path_buf()) {
                break; // reached the root, or the rest is already in
            }
        }
        m.files.insert(path, f.kind);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dir: &str, name: &str, is_dir: bool, ignored: bool) -> Entry {
        Entry { name: name.into(), path: Path::new(dir).join(name), is_dir, ignored }
    }

    /// /r: src/ (main.rs, util/ (x.rs)), target/ (ignored), Cargo.toml
    fn cache() -> Cache {
        let mut c = Cache::new();
        let l = |entries| Arc::new(Listing { entries });
        c.insert(
            "/r".into(),
            l(vec![
                entry("/r", "src", true, false),
                entry("/r", "target", true, true),
                entry("/r", "Cargo.toml", false, false),
            ]),
        );
        c.insert(
            "/r/src".into(),
            l(vec![entry("/r/src", "util", true, false), entry("/r/src", "main.rs", false, false)]),
        );
        c.insert("/r/src/util".into(), l(vec![entry("/r/src/util", "x.rs", false, false)]));
        c.insert("/r/target".into(), l(vec![entry("/r/target", "debug", true, false)]));
        c
    }

    fn state(expanded: &[&str], selected: Option<&str>) -> TreeState {
        TreeState { expanded: expanded.iter().map(PathBuf::from).collect(), selected: selected.map(PathBuf::from) }
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.entry().name)).collect()
    }

    #[test]
    fn flatten_expanded_only() {
        let root = Path::new("/r");
        let rows = flatten(root, &state(&[], None), &cache(), true);
        assert_eq!(names(&rows), vec!["src", "target", "Cargo.toml"]);
        let s = state(&["/r/src", "/r/src/util"], None);
        assert_eq!(
            names(&flatten(root, &s, &cache(), true)),
            vec!["src", "  util", "    x.rs", "  main.rs", "target", "Cargo.toml"]
        );
        assert_eq!(
            names(&flatten(root, &s, &cache(), false)),
            vec!["src", "  util", "    x.rs", "  main.rs", "Cargo.toml"],
            "ignored hidden"
        );
        // util stays expanded in the state while src is collapsed: not shown, not wanted.
        let s = state(&["/r/src/util", "/other"], None);
        assert_eq!(names(&flatten(root, &s, &cache(), true)), vec!["src", "target", "Cargo.toml"]);
        assert_eq!(wanted(root, &s), vec![PathBuf::from("/r")]);
        let s = state(&["/r/src", "/r/src/util", "/r/target"], None);
        let rows = flatten(root, &s, &cache(), true);
        let ignored: Vec<_> = rows.iter().filter(|r| r.ignored).map(|r| r.entry().name.as_str()).collect();
        assert_eq!(ignored, vec!["target", "debug"], "inside an ignored dir counts as ignored");
        assert_eq!(
            wanted(root, &s),
            vec![PathBuf::from("/r"), "/r/src".into(), "/r/src/util".into(), "/r/target".into()]
        );
    }

    #[test]
    fn keyboard() {
        let root = Path::new("/r");
        let s = state(&["/r/src"], Some("/r/src/main.rs"));
        let rows = flatten(root, &s, &cache(), true); // src, util, main.rs, target, Cargo.toml
        let k = |sel: Option<&str>, key_: Key| key(&rows, sel.map(Path::new), key_);
        let sel = |p: &str| Action::Select(p.into());
        assert_eq!(k(None, Key::Down), sel("/r/src"), "no selection starts at the top");
        assert_eq!(k(Some("/r/src/main.rs"), Key::Up), sel("/r/src/util"));
        assert_eq!(k(Some("/r/src/main.rs"), Key::Down), sel("/r/target"));
        assert_eq!(k(Some("/r/Cargo.toml"), Key::Down), sel("/r/Cargo.toml"), "stays at the end");
        assert_eq!(k(Some("/r/src"), Key::Up), sel("/r/src"));
        assert_eq!(k(Some("/r/src/main.rs"), Key::Left), sel("/r/src"), "left on a file: parent");
        assert_eq!(k(Some("/r/src"), Key::Left), Action::Collapse("/r/src".into()));
        assert_eq!(k(Some("/r/src"), Key::Right), sel("/r/src/util"), "right on an expanded dir: first child");
        assert_eq!(k(Some("/r/src/util"), Key::Right), Action::Expand("/r/src/util".into()));
        assert_eq!(k(Some("/r/target"), Key::Left), Action::None, "top level has no parent row");
        assert_eq!(k(Some("/r/src/main.rs"), Key::Enter), Action::Open("/r/src/main.rs".into()));
        assert_eq!(k(Some("/r/src/util"), Key::Enter), Action::Expand("/r/src/util".into()));
        assert_eq!(k(Some("/r/src"), Key::Enter), Action::Collapse("/r/src".into()));
        assert_eq!(k(Some("/r/src"), Key::End), sel("/r/Cargo.toml"));
        assert_eq!(k(Some("/r/Cargo.toml"), Key::PageUp(3)), sel("/r/src/util"));
        assert_eq!(k(Some("/r/src"), Key::PageDown(99)), sel("/r/Cargo.toml"));
    }

    #[test]
    fn status_marks() {
        let st = Status {
            files: ["src/a/b.rs", "src/c.rs", "new/deep/file.txt", "top.md"]
                .iter()
                .zip(['M', 'A', '?', 'D'])
                .map(|(p, k)| git::FileChange {
                    path: p.to_string(),
                    orig: None,
                    kind: k,
                    staged: false,
                    unstaged: true,
                    lines: None,
                })
                .collect(),
            ..Status::default()
        };
        let m = marks(Path::new("/r"), &st);
        assert_eq!(m.files[Path::new("/r/src/a/b.rs")], 'M');
        assert_eq!(m.files[Path::new("/r/new/deep/file.txt")], '?');
        let mut dirs: Vec<_> = m.dirs.iter().map(|d| d.to_str().unwrap()).collect();
        dirs.sort();
        assert_eq!(dirs, vec!["/r/new", "/r/new/deep", "/r/src", "/r/src/a"]);
    }
}
