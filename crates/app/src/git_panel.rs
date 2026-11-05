//! Read-only git UI: the repo store (one watcher per repo, shared by its tabs), the right-side changes panel and
//! the diff view shown over the terminal. Both views are separate entities rendered with `.cached(..)` and only
//! virtualized lists (`uniform_list`): a 10k-line diff shapes only the rows on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use git::{Diff, FileChange, LineKind, Status};
use gpui::{
    AnyView, App, ClickEvent, Context, FocusHandle, Focusable, KeyDownEvent, SharedString, UniformListScrollHandle,
    WeakEntity, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::scrollbar::{self, scrollbar};

use crate::terminal_element::{FONT_FAMILY, FONT_SIZE, palette};
use crate::workspace::{Workspace, colors};

/// One watcher per repo root, refcounted by the tabs whose cwd is in it. The last tab leaving drops it (and its
/// inotify watches).
pub struct Repos {
    map: HashMap<PathBuf, Entry>,
    /// Pinged (bounded(1), coalescing) after any watcher's refresh.
    ping: Sender<()>,
    /// Directories whose listing changed (from all repo watchers and the app's `DirWatcher`), for file trees.
    dirty: Arc<Mutex<HashSet<PathBuf>>>,
}

struct Entry {
    repo: git::Repo,
    watcher: git::Watcher,
    tabs: usize,
}

impl Repos {
    pub fn new() -> (Self, Receiver<()>) {
        let (ping, rx) = async_channel::bounded(1);
        (Self { map: HashMap::new(), ping, dirty: Default::default() }, rx)
    }

    pub fn acquire(&mut self, repo: git::Repo) {
        if let Some(e) = self.map.get_mut(&repo.root) {
            e.tabs += 1;
            return;
        }
        let report = self.reporter();
        match git::Watcher::start(repo.clone(), move |u| report(u.dirs)) {
            Ok(watcher) => {
                self.map.insert(repo.root.clone(), Entry { repo, watcher, tabs: 1 });
            }
            Err(e) => eprintln!("agent-ide: can't watch {}: {e}", repo.root.display()),
        }
    }

    pub fn release(&mut self, root: &Path) {
        if let Some(e) = self.map.get_mut(root) {
            e.tabs -= 1;
            if e.tabs == 0 {
                self.map.remove(root);
            }
        }
    }

    /// A callback that records changed directories and pings the UI (for watcher threads).
    pub fn reporter(&self) -> impl Fn(Vec<PathBuf>) + Send + 'static {
        let (ping, dirty) = (self.ping.clone(), self.dirty.clone());
        move |dirs| {
            if !dirs.is_empty() {
                dirty.lock().unwrap().extend(dirs);
            }
            let _ = ping.try_send(());
        }
    }

    pub fn take_dirty_dirs(&self) -> Vec<PathBuf> {
        self.dirty.lock().unwrap().drain().collect()
    }

    pub fn status(&self, root: &Path) -> Option<Arc<Status>> {
        self.map.get(root)?.watcher.latest()
    }

    pub fn repo(&self, root: &Path) -> Option<&git::Repo> {
        self.map.get(root).map(|e| &e.repo)
    }

    /// Changes whenever the repo's `git status` ran again (an open diff re-reads then).
    pub fn refreshes(&self, root: &Path) -> u64 {
        self.map.get(root).map_or(0, |e| e.watcher.refreshes())
    }

    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.map.keys()
    }

    pub fn refresh(&self, root: &Path) {
        if let Some(e) = self.map.get(root) {
            e.watcher.refresh();
        }
    }

    #[cfg(test)]
    fn watched(&self) -> Vec<(PathBuf, usize)> {
        let mut v: Vec<_> = self.map.iter().map(|(k, e)| (k.clone(), e.tabs)).collect();
        v.sort();
        v
    }
}

/// What the panel shows for the active tab.
#[derive(Clone)]
pub enum PanelView {
    NoSession,
    /// Repo discovery still running (or the tab isn't spawned).
    Pending,
    NotRepo,
    GitMissing,
    Repo { root: PathBuf, status: Option<Arc<Status>> },
}

impl PanelView {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Repo { root: a, status: sa }, Self::Repo { root: b, status: sb }) => {
                a == b
                    && match (sa, sb) {
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

pub struct GitPanel {
    view: PanelView,
    /// Path of the file whose diff is open.
    selected: Option<String>,
    /// Too narrow for the "S staged · U unstaged" legend (the letters keep their tooltips).
    narrow: bool,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    workspace: WeakEntity<Workspace>,
}

const ROW_H: f32 = 22.;
/// The panel can't be dragged narrower than this.
pub const MIN_WIDTH: f32 = 240.;
/// Below this width the header legend is hidden.
pub const LEGEND_WIDTH: f32 = 330.;

/// Hover text for small labels (gpui shows it after its usual delay, next to the cursor).
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(colors::MODAL))
            .border_1()
            .border_color(rgb(colors::DIVIDER_HOVER))
            .text_xs()
            .text_color(palette::FG)
            .child(self.0.clone())
    }
}

pub fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| Tip(text.into())).into()
}

/// Flex shrink weight: within a row the directory gives up its width long before the file name does.
fn shrink<E: Styled>(mut e: E, weight: f32) -> E {
    e.style().flex_shrink = Some(weight);
    e
}

impl GitPanel {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            view: PanelView::Pending,
            selected: None,
            narrow: false,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            workspace,
        }
    }

    /// Re-renders only if something it shows changed.
    pub fn set(&mut self, view: PanelView, selected: Option<String>, narrow: bool, cx: &mut Context<Self>) {
        if !view.same(&self.view) || selected != self.selected || narrow != self.narrow {
            self.view = view;
            self.selected = selected;
            self.narrow = narrow;
            cx.notify();
        }
    }

    fn row(&self, ix: usize, root: &Path, st: &Status) -> gpui::AnyElement {
        let Some(f) = st.files.get(ix) else {
            return div()
                .h(px(ROW_H))
                .px_3()
                .flex()
                .items_center()
                .text_xs()
                .text_color(rgb(colors::DIM))
                .child(format!("+{} more files not listed", st.more))
                .into_any_element();
        };
        let (name, dir) = match f.path.rsplit_once('/') {
            Some((d, n)) => (n.to_string(), format!("{d}/")),
            None => (f.path.clone(), String::new()),
        };
        let dir = match &f.orig {
            Some(o) => format!("{dir} ← {o}"),
            None => dir,
        };
        let selected = self.selected.as_deref() == Some(f.path.as_str());
        let (file, root, ws) = (f.clone(), root.to_path_buf(), self.workspace.clone());
        let tag = |id: &'static str, label: &'static str, color: u32, hint: &'static str| {
            div().id((id, ix)).flex_none().text_xs().text_color(rgb(color)).child(label).tooltip(tip(hint))
        };
        div()
            .id(("git-file", ix))
            .w_full() // a uniform_list row is laid out as a root: without this it takes its min-content width
            .overflow_hidden()
            .h(px(ROW_H))
            .pl_3()
            .pr(px(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_sm()
            .when(selected, |d| d.bg(rgb(colors::ROW_ACTIVE)))
            .when(!selected, |d| d.hover(|s| s.bg(rgb(colors::ROW_HOVER))))
            // Not `cx.listener`: the workspace updates this panel in response, which must not happen while the
            // panel itself is being updated (gpui panics on re-entrant updates).
            .on_click(move |_: &ClickEvent, window, cx| {
                let (root, file) = (root.clone(), file.clone());
                let _ = ws.update(cx, |w, cx| w.toggle_diff(root, file, window, cx));
            })
            .child(div().flex_none().w(px(12.)).text_xs().text_color(rgb(kind_color(f.kind))).child(f.kind.to_string()))
            // Name and directory share what's left; counts and S/U never shrink. The directory truncates first.
            .child(shrink(div().min_w_0().truncate(), 1.).child(name))
            .child(shrink(div().flex_grow(1.).min_w_0().truncate().text_xs().text_color(rgb(colors::DIM)), 10_000.).child(dir))
            .when(f.staged, |d| d.child(tag("tag-s", "S", colors::DONE, "Staged: has changes in the index")))
            .when(f.unstaged, |d| d.child(tag("tag-u", "U", colors::NEEDS_YOU, "Unstaged: has changes not added to the index")))
            .child(div().flex_none().text_xs().child(line_counts(f.lines)))
            .into_any_element()
    }
}

pub fn kind_color(kind: char) -> u32 {
    match kind {
        'A' | '?' => colors::DONE,
        'D' => colors::EXITED,
        'R' | 'C' => colors::WORKING,
        'U' => colors::NEEDS_YOU,
        _ => 0xd0b060,
    }
}

/// "+12 −3" (colored), "bin" for binary/too-big files.
pub fn line_counts(lines: Option<(u32, u32)>) -> gpui::AnyElement {
    match lines {
        Some((a, d)) => div()
            .flex()
            .gap_1()
            .child(div().text_color(rgb(colors::DONE)).child(format!("+{a}")))
            .child(div().text_color(rgb(colors::EXITED)).child(format!("−{d}")))
            .into_any_element(),
        None => div().text_color(rgb(colors::DIM)).child("bin").into_any_element(),
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("git panel render");
        let note = |text: &'static str| div().px_3().py_2().text_xs().text_color(rgb(colors::DIM)).child(text);
        let mut header = div().flex().items_center().gap_2().px_3().py_2().child(
            div().flex_none().text_xs().text_color(rgb(colors::DIM)).child("Changes"),
        );
        let body = match &self.view {
            PanelView::NoSession => note("No session").into_any_element(),
            PanelView::Pending => note("…").into_any_element(),
            PanelView::NotRepo => note("Not a git repository").into_any_element(),
            PanelView::GitMissing => note("git not found on PATH").into_any_element(),
            PanelView::Repo { status: None, .. } => note("Reading status…").into_any_element(),
            PanelView::Repo { root, status: Some(st) } => {
                header = header.child(div().flex_1().min_w_0().flex().justify_end().child(
                    div().min_w_0().text_xs().truncate().child(format!("⎇ {}", st.branch)),
                ));
                let (n, add, del) = st.totals();
                let (root, st) = (root.clone(), st.clone());
                let summary = div()
                    .flex()
                    .gap_2()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(colors::DIM))
                    .child(format!("{n} file{}", if n == 1 { "" } else { "s" }))
                    .when(n > 0, |d| d.child(line_counts(Some((add.min(u32::MAX as u64) as u32, del.min(u32::MAX as u64) as u32)))))
                    .when(n > 0 && !self.narrow, |d| d.child(div().ml_auto().flex_none().child("S staged · U unstaged")));
                let warning = st.watch_error.clone().map(|e| {
                    let (root, ws) = (root.clone(), self.workspace.clone());
                    div()
                        .id("git-refresh")
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(colors::NEEDS_YOU))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(colors::ROW_HOVER)))
                        .child(format!("{e}. Click to refresh."))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws.update(cx, |w, _| w.refresh_repo(&root));
                        })
                });
                let count = st.files.len() + (st.more > 0) as usize;
                let list = if st.files.is_empty() {
                    note("No changes").into_any_element()
                } else {
                    let rows = uniform_list(
                        "git-files",
                        count,
                        cx.processor(move |p, range: std::ops::Range<usize>, _, _| {
                            range.map(|ix| p.row(ix, &root, &st)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full();
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(rows)
                        .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                        .into_any_element()
                };
                div().flex().flex_col().flex_1().min_h_0().child(summary).children(warning).child(list).into_any_element()
            }
        };
        div().flex().flex_col().size_full().bg(rgb(colors::SIDEBAR)).child(header).child(body)
    }
}

/// A diff ready to show: rows (SharedString: cloning a visible row per frame is a refcount bump), truncated.
pub type Prepared = (Arc<Vec<(LineKind, SharedString)>>, bool);

/// The diff of one file, over the terminal area. Esc closes it.
pub struct DiffView {
    pub root: PathBuf,
    pub file: FileChange,
    /// None while the first read is running; later reads keep showing the previous diff until they finish.
    lines: Option<Arc<Vec<(LineKind, SharedString)>>>,
    truncated: bool,
    failed: bool,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: gpui::Pixels,
}

impl DiffView {
    pub fn new(root: PathBuf, file: FileChange, line_height: gpui::Pixels, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            root,
            file,
            lines: None,
            truncated: false,
            failed: false,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height,
        }
    }

    /// Run off the UI thread. The file header (diff --git, index, ---/+++) is dropped: the view's own header says
    /// it all. Hunk headers show only their section context.
    pub fn prepare(diff: Diff) -> Prepared {
        let rows = diff
            .lines
            .into_iter()
            .filter(|l| l.kind != LineKind::Header)
            .map(|l| {
                let text = if l.kind == LineKind::Hunk { hunk_context(&l.text) } else { l.text };
                (l.kind, text.into())
            })
            .collect();
        (Arc::new(rows), diff.truncated)
    }

    pub fn set_lines(&mut self, lines: Option<Prepared>, file: FileChange, cx: &mut Context<Self>) {
        match lines {
            Some((lines, truncated)) => {
                let same = self.lines.as_ref().is_some_and(|l| **l == *lines) && self.truncated == truncated;
                if same && file == self.file {
                    return;
                }
                self.lines = Some(lines);
                self.truncated = truncated;
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.file = file;
        cx.notify();
    }

    /// Perf harness: how many rows are loaded, and jump to one.
    pub fn loaded_rows(&self) -> usize {
        self.lines.as_ref().map_or(0, |l| l.len())
    }

    pub fn scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Top);
        cx.notify();
    }

}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("diff view render");
        let f = &self.file;
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .py_1()
            .bg(rgb(colors::SIDEBAR))
            .text_sm()
            .child(div().text_color(rgb(kind_color(f.kind))).child(f.kind.to_string()))
            .child(div().min_w_0().truncate().child(match &f.orig {
                Some(o) => format!("{o} → {}", f.path),
                None => f.path.clone(),
            }))
            .child(div().text_xs().child(line_counts(f.lines)))
            .child(div().text_xs().text_color(rgb(colors::DIM)).child(match (f.staged, f.unstaged) {
                (true, true) => "staged + unstaged",
                (true, false) => "staged",
                _ if f.kind == '?' => "untracked",
                _ => "unstaged",
            }))
            .child(div().ml_auto().flex_none().text_xs().text_color(rgb(colors::DIM)).child("Esc to close"));
        let body = match (&self.lines, self.failed) {
            (_, true) => div().p_3().text_color(rgb(colors::EXITED)).child("Couldn't read the diff").into_any_element(),
            (None, _) => div().p_3().text_color(rgb(colors::DIM)).child("Loading…").into_any_element(),
            (Some(lines), _) if lines.is_empty() => {
                div().p_3().text_color(rgb(colors::DIM)).child("No textual changes (mode change or binary)").into_any_element()
            }
            (Some(lines), _) => {
                let (lines, lh, truncated) = (lines.clone(), self.line_height, self.truncated);
                let count = lines.len() + truncated as usize;
                let list = uniform_list("diff-lines", count, move |range, _, _| {
                    range
                        .map(|ix| {
                            let row = div().h(lh).pl_3().pr(px(14.)).whitespace_nowrap().overflow_hidden();
                            let Some((kind, text)) = lines.get(ix) else {
                                return row.text_color(rgb(colors::NEEDS_YOU)).child("… diff truncated (too large to show)");
                            };
                            let (bg, fg) = match kind {
                                LineKind::Add => (Some(0x1d3324), 0xb8e0b0),
                                LineKind::Del => (Some(0x3a1f24), 0xf0b0b0),
                                LineKind::Hunk => (None, 0x5c6a88),
                                LineKind::Header | LineKind::Meta => (None, colors::DIM),
                                LineKind::Context => (None, 0xc8c8c8),
                            };
                            row.when_some(bg, |d, bg| d.bg(rgb(bg))).text_color(rgb(fg)).child(text.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(FONT_FAMILY)
                .text_size(px(FONT_SIZE - 1.))
                .line_height(lh)
                ;
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list.size_full())
                    .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette::BG)
            .track_focus(&self.focus)
            .on_key_down({
                // Plain closure, not `cx.listener`: closing re-enters the workspace, which reads this view.
                let ws = self.workspace.clone();
                move |ev: &KeyDownEvent, window, cx| {
                    let ks = &ev.keystroke;
                    if ks.key == "escape" && !ks.modifiers.modified() {
                        let _ = ws.update(cx, |w, cx| w.close_overlay(window, cx));
                        cx.stop_propagation();
                    }
                }
            })
            .child(header)
            .child(body)
    }
}

/// `@@ -1,2 +1,3 @@ fn main()` → `⋯ fn main()`; just `⋯` when git found no section context.
fn hunk_context(line: &str) -> String {
    match line.splitn(3, "@@").nth(2).map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => format!("⋯ {c}"),
        None => "⋯".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_rows_without_file_header() {
        let raw = git::Diff {
            lines: [
                (LineKind::Header, "diff --git a/x b/x"),
                (LineKind::Header, "index 1..2"),
                (LineKind::Header, "--- a/x"),
                (LineKind::Header, "+++ b/x"),
                (LineKind::Hunk, "@@ -1,2 +1,2 @@ fn main() {"),
                (LineKind::Del, "-a"),
                (LineKind::Hunk, "@@ -9 +9 @@"),
                (LineKind::Add, "+b"),
            ]
            .into_iter()
            .map(|(kind, t)| git::Line { kind, text: t.into() })
            .collect(),
            truncated: false,
        };
        let (rows, _) = DiffView::prepare(raw);
        let got: Vec<_> = rows.iter().map(|(_, t)| t.to_string()).collect();
        assert_eq!(got, vec!["⋯ fn main() {", "-a", "⋯", "+b"]);
    }

    fn repo(name: &str) -> git::Repo {
        let d = std::env::temp_dir().join(format!("agent-ide-repos-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&d).status().unwrap().success());
        match git::discover(&d) {
            git::Discovery::Repo(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn watchers_are_shared_and_dropped_with_the_last_tab() {
        let (a, b) = (repo("a"), repo("b"));
        let (mut repos, _rx) = Repos::new();
        repos.acquire(a.clone());
        repos.acquire(a.clone()); // second tab in the same repo
        assert_eq!(repos.watched(), vec![(a.root.clone(), 2)]);
        // One tab cd's into repo B: A's watcher stays for the other tab.
        repos.release(&a.root);
        repos.acquire(b.clone());
        assert_eq!(repos.watched(), vec![(a.root.clone(), 1), (b.root.clone(), 1)]);
        // The other one follows: A is unused and dropped.
        repos.release(&a.root);
        repos.acquire(b.clone());
        assert_eq!(repos.watched(), vec![(b.root.clone(), 2)]);
        repos.release(&b.root);
        repos.release(&b.root);
        assert!(repos.watched().is_empty());
        for r in [a, b] {
            let _ = std::fs::remove_dir_all(r.root);
        }
    }
}
