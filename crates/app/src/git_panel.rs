//! Read-only git UI: the repo store (one watcher per repo, shared by its tabs), the right-side changes panel and
//! the diff view shown over the terminal. Both views are separate entities rendered with `.cached(..)` and only
//! virtualized lists (`uniform_list`): a 10k-line diff shapes only the rows on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use git::layout::{DLine, Layout, SplitRow};
use git::{FileChange, LineKind, Status};
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, StyledText,
    UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px, uniform_list,
};

use crate::scrollbar::{self, scrollbar};
use crate::theme::{self, MONO_FONT, TERMINAL_FONT_SIZE, height, radius, space, text_size};
use crate::ui::{self, icon, line_counts, tip};
use crate::persist::{DiffMode, SplitStyle};
use crate::workspace::Workspace;

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

/// The panel can't be dragged narrower than this.
pub const MIN_WIDTH: f32 = 240.;
/// Below this width the header legend is hidden.
pub const LEGEND_WIDTH: f32 = 330.;

/// Flex shrink weight: within a row the directory gives up its width long before the file name does.
pub fn shrink<E: Styled>(mut e: E, weight: f32) -> E {
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
        let t = theme::get();
        let Some(f) = st.files.get(ix) else {
            return ui::note(format!("+{} more files not listed", st.more)).h(px(height::LIST_ROW)).into_any_element();
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
        let tag = |id: &'static str, label: &'static str, color, hint: &'static str| {
            div().id((id, ix)).flex_none().text_size(px(text_size::XS)).text_color(color).child(label).tooltip(tip(hint))
        };
        let kind_color = theme::git_kind(f.kind);
        div()
            .id(("git-file", ix))
            .w_full() // a uniform_list row is laid out as a root: without this it takes its min-content width
            .overflow_hidden()
            .h(px(height::LIST_ROW))
            .pl(px(space::LG))
            .pr(px(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap(px(space::SM))
            .cursor_pointer()
            .text_size(px(text_size::MD))
            .when(selected, |d| d.bg(t.bg.selected))
            .when(!selected, |d| d.hover(|s| s.bg(t.bg.hover)))
            // Not `cx.listener`: the workspace updates this panel in response, which must not happen while the
            // panel itself is being updated (gpui panics on re-entrant updates).
            .on_click(move |_: &ClickEvent, window, cx| {
                let (root, file) = (root.clone(), file.clone());
                let _ = ws.update(cx, |w, cx| w.toggle_diff(root, file, window, cx));
            })
            .child(ui::icon(ui::file_icon(&name), 14., kind_color))
            // Name and directory share what's left; counts and S/U never shrink. The directory truncates first.
            .child(shrink(div().min_w_0().truncate().text_color(t.text.primary), 1.).child(name))
            .child(
                shrink(div().flex_grow(1.).min_w_0().truncate().text_size(px(text_size::XS)).text_color(t.text.dim), 10_000.)
                    .child(dir),
            )
            .when(f.staged, |d| d.child(tag("tag-s", "S", t.git.added, "Staged: has changes in the index")))
            .when(f.unstaged, |d| d.child(tag("tag-u", "U", t.git.modified, "Unstaged: has changes not added to the index")))
            .child(div().flex_none().text_size(px(text_size::XS)).child(line_counts(f.lines)))
            .into_any_element()
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("git panel render");
        let t = theme::get();
        let body = match &self.view {
            PanelView::NoSession => ui::note("No session").into_any_element(),
            PanelView::Pending => ui::note("…").into_any_element(),
            PanelView::NotRepo => ui::note("Not a git repository").into_any_element(),
            PanelView::GitMissing => ui::note("git not found on PATH").into_any_element(),
            PanelView::Repo { status: None, .. } => ui::note("Reading status…").into_any_element(),
            PanelView::Repo { root, status: Some(st) } => {
                let (n, add, del) = st.totals();
                let (root, st) = (root.clone(), st.clone());
                let clamp = |v: u64| v.min(u32::MAX as u64) as u32;
                let summary = div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::LG))
                    .py(px(space::SM))
                    .text_size(px(text_size::XS))
                    .text_color(t.text.muted)
                    .child(ui::icon(icon::BRANCH, 12., t.text.muted))
                    .child(div().min_w_0().truncate().text_color(t.text.primary).child(st.branch.clone()))
                    .child(div().flex_none().text_color(t.text.dim).child(format!("{n} file{}", if n == 1 { "" } else { "s" })))
                    .when(n > 0, |d| d.child(line_counts(Some((clamp(add), clamp(del))))))
                    .when(n > 0 && !self.narrow, |d| d.child(ui::hint("S staged · U unstaged")));
                let warning = st.watch_error.clone().map(|e| {
                    let (root, ws) = (root.clone(), self.workspace.clone());
                    div()
                        .id("git-refresh")
                        .mx(px(space::MD))
                        .px(px(space::MD))
                        .py(px(space::XS))
                        .flex()
                        .gap(px(space::SM))
                        .rounded(px(radius::MD))
                        .bg(t.bg.elevated)
                        .text_size(px(text_size::XS))
                        .text_color(t.state.needs_you)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.bg.hover))
                        .child(ui::icon(icon::REFRESH, 12., t.state.needs_you))
                        .child(format!("{e}. Click to refresh."))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws.update(cx, |w, _| w.refresh_repo(&root));
                        })
                });
                let count = st.files.len() + (st.more > 0) as usize;
                let list = if st.files.is_empty() {
                    ui::note("No changes").into_any_element()
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
        div().flex().flex_col().size_full().bg(t.bg.panel).child(body)
    }
}

/// Below this many terminal columns in the main area, split falls back to unified (each side needs ~50 columns
/// of code plus its gutter).
pub const MIN_SPLIT_COLS: u16 = 120;

/// The diff of one file, over the terminal area. Esc closes it.
pub struct DiffView {
    pub root: PathBuf,
    pub file: FileChange,
    /// None while the first read is running; later reads keep showing the previous diff until they finish.
    layout: Option<Arc<Layout>>,
    failed: bool,
    /// The saved preference; `narrow` forces unified without changing it.
    mode: DiffMode,
    narrow: bool,
    split_style: SplitStyle,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: gpui::Pixels,
}

impl DiffView {
    pub fn new(
        root: PathBuf,
        file: FileChange,
        line_height: gpui::Pixels,
        (mode, split_style, narrow): (DiffMode, SplitStyle, bool),
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            split_style,
            root,
            file,
            layout: None,
            failed: false,
            mode,
            narrow,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height,
        }
    }

    /// The current layout, handed to the next read so an unchanged diff isn't laid out again.
    pub fn layout(&self) -> Option<Arc<Layout>> {
        self.layout.clone()
    }

    pub fn set_layout(&mut self, layout: Option<Arc<Layout>>, file: FileChange, cx: &mut Context<Self>) {
        match layout {
            Some(l) => {
                if self.layout.as_ref().is_some_and(|old| Arc::ptr_eq(old, &l)) && file == self.file && !self.failed {
                    return;
                }
                self.layout = Some(l);
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.file = file;
        cx.notify();
    }

    fn split(&self) -> bool {
        self.mode == DiffMode::Split && !self.narrow
    }

    /// Mode, split style or width changed: keep the line at the top in view across a mode switch.
    pub fn set_mode(&mut self, mode: DiffMode, split_style: SplitStyle, narrow: bool, cx: &mut Context<Self>) {
        if (mode, split_style, narrow) == (self.mode, self.split_style, self.narrow) {
            return;
        }
        self.split_style = split_style;
        let was_split = self.split();
        (self.mode, self.narrow) = (mode, narrow);
        if was_split != self.split()
            && let Some(l) = &self.layout
        {
            let top = self.scroll.0.borrow().base_handle.logical_scroll_top().0;
            let row = if was_split { split_to_line(l, top) } else { line_to_split(l, top) };
            self.scroll.scroll_to_item_strict(row, gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Perf harness: syntax colors arrived.
    pub fn colored(&self) -> bool {
        self.layout.as_ref().is_some_and(|l| l.colored)
    }

    /// Perf harness: how many rows are loaded, and jump to one.
    pub fn loaded_rows(&self) -> usize {
        self.layout.as_ref().map_or(0, |l| if self.split() { l.split.len() } else { l.lines.len() })
    }

    pub fn scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn mode_switch(&self) -> impl IntoElement + use<> {
        let (split, narrow) = (self.split(), self.narrow);
        let (ws, ws2) = (self.workspace.clone(), self.workspace.clone());
        segmented()
            .child(
                segment("diff-unified", "Unified", !split, true)
                    .tooltip(tip("Unified diff (Ctrl+Shift+D)"))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = ws.update(cx, |w, cx| w.set_diff_mode(DiffMode::Unified, cx));
                    }),
            )
            .child(
                segment("diff-split", "Split", split, !narrow)
                    .tooltip(tip(if narrow { "Too narrow for split" } else { "Side by side (Ctrl+Shift+D)" }))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if !narrow {
                            let _ = ws2.update(cx, |w, cx| w.set_diff_mode(DiffMode::Split, cx));
                        }
                    }),
            )
    }

    /// Classic / Clean, shown only while the split view is.
    fn style_switch(&self) -> impl IntoElement + use<> {
        let clean = self.split_style == SplitStyle::Clean;
        let (ws, ws2) = (self.workspace.clone(), self.workspace.clone());
        segmented()
            .child(
                segment("split-classic", "Classic", !clean, true)
                    .tooltip(tip("Line backgrounds and +/- markers"))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = ws.update(cx, |w, cx| w.set_split_style(SplitStyle::Classic, cx));
                    }),
            )
            .child(
                segment("split-clean", "Clean", clean, true)
                    .tooltip(tip("Plain code, only the changed tokens colored"))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = ws2.update(cx, |w, cx| w.set_split_style(SplitStyle::Clean, cx));
                    }),
            )
    }
}

/// A small segmented control (the diff header's switches).
fn segmented() -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_none()
        .p(px(space::XXS))
        .gap(px(space::XXS))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(t.border.subtle)
}

fn segment(id: &'static str, label: &'static str, on: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .px(px(space::SM))
        .rounded(px(radius::SM))
        .text_size(px(text_size::XS))
        .when(on, |d| d.bg(t.bg.selected).text_color(t.text.primary))
        .when(!on && enabled, |d| d.text_color(t.text.muted).cursor_pointer().hover(|s| s.bg(t.bg.hover)))
        .when(!enabled, |d| d.text_color(t.text.faint).cursor_not_allowed())
        .child(label)
}

/// First split row showing unified line `ix` (on either side).
fn line_to_split(l: &Layout, ix: usize) -> usize {
    let ix = ix as u32;
    l.split
        .iter()
        .position(|r| match *r {
            SplitRow::Full(i) => i >= ix,
            SplitRow::Pair(a, b) => a.is_some_and(|i| i >= ix) || b.is_some_and(|i| i >= ix),
        })
        .unwrap_or(0)
}

/// The unified line at split row `ix`.
fn split_to_line(l: &Layout, ix: usize) -> usize {
    match l.split.get(ix) {
        Some(SplitRow::Full(i)) => *i as usize,
        Some(SplitRow::Pair(a, b)) => a.or(*b).unwrap_or(0) as usize,
        None => 0,
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Digit columns of the old/new line-number gutters.
#[derive(Clone, Copy)]
struct Geom {
    lh: gpui::Pixels,
    old: usize,
    new: usize,
}

fn digits(n: u32) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn line_colors(kind: LineKind) -> (Option<gpui::Rgba>, gpui::Rgba, Option<gpui::Rgba>, char) {
    let t = theme::get();
    let d = &t.diff;
    match kind {
        LineKind::Add => (Some(d.add_bg), d.add_fg, Some(d.add_word_bg), '+'),
        LineKind::Del => (Some(d.del_bg), d.del_fg, Some(d.del_word_bg), '-'),
        LineKind::Hunk => (None, d.hunk, None, ' '),
        LineKind::Header | LineKind::Meta => (None, t.text.dim, None, ' '),
        LineKind::Context => (None, d.context, None, ' '),
    }
}

/// How a line is drawn. Clean (split only): no line background or marker; a paired changed line shows syntax
/// colors with only its changed tokens in red/green (bold, faint background); an unpaired one is all red/green.
#[derive(Clone, Copy, PartialEq)]
enum Look {
    Classic,
    CleanPaired,
    CleanWhole,
}

/// Clean style colors of a changed line: (text/number color, token background); None for other kinds.
fn clean_tone(kind: LineKind) -> Option<(gpui::Rgba, gpui::Rgba)> {
    let d = &theme::get().diff;
    match kind {
        LineKind::Del => Some((d.clean_del, d.clean_del_bg)),
        LineKind::Add => Some((d.clean_add, d.clean_add_bg)),
        _ => None,
    }
}

/// One line as a single text run: right-aligned line numbers (`nums`: number, width), marker, text; gutter
/// color, syntax colors and word highlights as highlight ranges. One shaped line per row keeps scrolling cheap.
fn line_text(l: &DLine, nums: &[(Option<u32>, usize)], look: Look) -> gpui::Div {
    use std::fmt::Write;
    let t = theme::get();
    let (_, mut fg, word_bg, marker) = line_colors(l.kind);
    let tone = clean_tone(l.kind).filter(|_| look != Look::Classic);
    let marker = if tone.is_some() { ' ' } else { marker };
    let mut s = String::with_capacity(l.text.len() + 16);
    for &(n, w) in nums {
        match n {
            Some(n) => write!(s, "{n:>w$} ").unwrap(),
            None => write!(s, "{:w$} ", "").unwrap(),
        }
    }
    let gutter = s.len();
    write!(s, " {marker} ").unwrap();
    let start = s.len();
    s.push_str(&l.text);
    let gutter_color = tone.map_or(t.diff.gutter, |(c, _)| c);
    let mut hl = vec![(0..gutter, HighlightStyle { color: Some(gutter_color.into()), ..Default::default() })];
    let range = |r: &git::layout::Run| start + r.range.start as usize..start + r.range.end as usize;
    match (look, tone) {
        // Whole added/removed line: all of it in the tone, nothing else.
        (Look::CleanWhole, Some((c, _))) => fg = c,
        (Look::CleanPaired, Some((c, bg))) => {
            fg = t.diff.context;
            hl.extend(l.runs.iter().map(|r| {
                let style = if r.word {
                    HighlightStyle {
                        color: Some(c.into()),
                        background_color: Some(bg.into()),
                        font_weight: Some(gpui::FontWeight::BOLD),
                        ..Default::default()
                    }
                } else {
                    HighlightStyle { color: r.class.and_then(|c| t.syntax.color(c)).map(Into::into), ..Default::default() }
                };
                (range(r), style)
            }));
        }
        // Classic (and context lines): syntax sets the text color; the word-diff background sits on top.
        _ => hl.extend(l.runs.iter().map(|r| {
            let style = HighlightStyle {
                color: r.class.and_then(|c| t.syntax.color(c)).map(Into::into),
                background_color: word_bg.filter(|_| r.word).map(Into::into),
                ..Default::default()
            };
            (range(r), style)
        })),
    }
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(fg)
        .child(StyledText::new(s).with_highlights(hl))
}

fn unified_row(l: &DLine, g: Geom) -> gpui::Div {
    let (bg, ..) = line_colors(l.kind);
    div()
        .flex()
        .w_full()
        .h(g.lh)
        .pr(px(14.))
        .when_some(bg, |r, bg| r.bg(bg))
        .child(line_text(l, &[(l.old, g.old), (l.new, g.new)], Look::Classic))
}

fn split_half(l: Option<&DLine>, old_side: bool, g: Geom, look: Look) -> gpui::Div {
    let half = div().flex().flex_1().flex_basis(px(0.)).min_w_0().h_full().overflow_hidden();
    let Some(l) = l else {
        return half.when(look == Look::Classic, |h| h.bg(theme::get().diff.filler));
    };
    let num = if old_side { (l.old, g.old) } else { (l.new, g.new) };
    if look == Look::Classic {
        let (bg, ..) = line_colors(l.kind);
        return half.when_some(bg, |h, bg| h.bg(bg)).child(line_text(l, &[num], look));
    }
    // Clean: a thin bar by the number marks a whole added/removed line (same width everywhere, so text aligns).
    let bar = clean_tone(l.kind).filter(|_| look == Look::CleanWhole).map(|(c, _)| c);
    half.child(div().flex_none().w(px(2.)).h_full().when_some(bar, |b, c| b.bg(c))).child(line_text(l, &[num], look))
}

/// Clean style of a split row: a removed/added pair with changed tokens to show is drawn token-wise; anything else
/// (one-sided, too different, nothing token-level changed) as whole lines.
fn clean_look(left: Option<&DLine>, right: Option<&DLine>) -> Look {
    match (left, right) {
        (Some(x), Some(y))
            if x.kind == LineKind::Del && y.kind == LineKind::Add && !(x.words.is_empty() && y.words.is_empty()) =>
        {
            Look::CleanPaired
        }
        _ => Look::CleanWhole,
    }
}

fn split_row(l: &Layout, r: SplitRow, g: Geom, style: SplitStyle) -> gpui::Div {
    match r {
        SplitRow::Full(i) => {
            div().flex().w_full().h(g.lh).child(line_text(&l.lines[i as usize], &[(None, g.old)], Look::Classic))
        }
        SplitRow::Pair(a, b) => {
            let get = |i: Option<u32>| i.map(|i| &l.lines[i as usize]);
            let (left, right) = (get(a), get(b));
            let look = match style {
                SplitStyle::Classic => Look::Classic,
                SplitStyle::Clean => clean_look(left, right),
            };
            div()
                .flex()
                .w_full()
                .h(g.lh)
                .pr(px(14.))
                .child(split_half(left, true, g, look))
                .child(div().flex_none().w(px(1.)).h_full().bg(theme::get().border.subtle))
                .child(split_half(right, false, g, look))
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("diff view render");
        let t = theme::get();
        let f = &self.file;
        let name = f.path.rsplit_once('/').map_or(f.path.as_str(), |(_, n)| n);
        let header = ui::overlay_header()
            .child(ui::icon(ui::file_icon(name), 14., theme::git_kind(f.kind)))
            .child(div().min_w_0().truncate().text_color(t.text.primary).child(match &f.orig {
                Some(o) => format!("{o} → {}", f.path),
                None => f.path.clone(),
            }))
            .child(div().text_size(px(text_size::XS)).child(line_counts(f.lines)))
            .child(ui::pill(match (f.staged, f.unstaged) {
                (true, true) => "staged + unstaged",
                (true, false) => "staged",
                _ if f.kind == '?' => "untracked",
                _ => "unstaged",
            }))
            .child(div().ml_auto().flex().gap(px(space::SM)).when(self.split(), |d| d.child(self.style_switch())).child(self.mode_switch()))
            .child(div().flex_none().text_size(px(text_size::XS)).text_color(t.text.dim).child("Esc to close"));
        let body = match (&self.layout, self.failed) {
            (_, true) => ui::note("Couldn't read the diff").text_color(t.state.error).into_any_element(),
            (None, _) => ui::note("Loading…").into_any_element(),
            (Some(l), _) if l.lines.is_empty() => ui::note("No textual changes (mode change or binary)").into_any_element(),
            (Some(l), _) => {
                let (l, split, style) = (l.clone(), self.split(), self.split_style);
                let g = Geom { lh: self.line_height, old: digits(l.max_old), new: digits(l.max_new) };
                let rows = if split { l.split.len() } else { l.lines.len() };
                let count = rows + l.truncated as usize;
                let list = uniform_list("diff-lines", count, move |range, _, _| {
                    range
                        .map(|ix| {
                            if ix >= rows {
                                return div()
                                    .h(g.lh)
                                    .pl(px(space::LG))
                                    .text_color(theme::get().state.needs_you)
                                    .child("… diff truncated (too large to show)");
                            }
                            if split { split_row(&l, l.split[ix], g, style) } else { unified_row(&l.lines[ix], g) }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(MONO_FONT)
                // One glyph per character: a ligature (`>=` → `≥`) would split under a word highlight.
                .font_features(gpui::FontFeatures::disable_ligatures())
                .text_size(px(TERMINAL_FONT_SIZE - 1.))
                .line_height(self.line_height);
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
            .bg(t.bg.base)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_style_pairs_only_rows_with_token_changes() {
        let line = |kind, words: Vec<std::ops::Range<u32>>| DLine {
            kind,
            old: None,
            new: None,
            text: "x".into(),
            words,
            runs: Vec::new(),
        };
        let (del, add) = (line(LineKind::Del, vec![]), line(LineKind::Add, std::iter::once(0..1).collect()));
        assert!(clean_look(Some(&del), Some(&add)) == Look::CleanPaired, "one side's change is enough");
        let plain_add = line(LineKind::Add, vec![]);
        assert!(clean_look(Some(&del), Some(&plain_add)) == Look::CleanWhole, "zipped/too different: whole lines");
        assert!(clean_look(None, Some(&add)) == Look::CleanWhole);
        let ctx = line(LineKind::Context, vec![]);
        assert!(clean_look(Some(&ctx), Some(&ctx)) == Look::CleanWhole, "context has no tone anyway");
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
