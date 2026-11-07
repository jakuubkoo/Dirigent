//! The "Files" tab of the right panel (a lazy, virtualized file tree) and the file preview over the terminal, an
//! editor for text files (state in `editor.rs`). Both are cached entities; the workspace owns the tree state and pushes it in. Handlers that call into
//! the workspace are plain closures with a weak handle, never `cx.listener` (the workspace updates these views back).

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use git::text::FileText;
use gpui::{
    Bounds, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, ScrollEvent, ScrollStrategy, SharedString, StyledText, Task,
    UniformListScrollHandle, WeakEntity, Window, canvas, div, fill, prelude::*, size, uniform_list,
};
use ropey::Rope;

use crate::editor::{self, Buffer, DiskChange, Format, Key as EditKey, ReadOnly, Spans};

use crate::file_tree::{Key, Marks, Row};
use crate::scrollbar::{self, scrollbar};
use crate::theme::{self, MONO_FONT, height, radius, space, text_size, z};
use crate::ui::{self, icon, line_counts, tip};
use crate::workspace::Workspace;

const INDENT: f32 = 14.;
/// Rows per PageUp/PageDown. ponytail: fixed; use the list's visible row count if it ever feels off.
const PAGE: usize = 20;

/// Everything the tree shows for the active tab.
#[derive(Clone, Default)]
pub struct TreeData {
    /// None: the tab's repo/cwd isn't resolved yet.
    pub root: Option<PathBuf>,
    pub rows: Arc<Vec<Row>>,
    pub marks: Arc<Marks>,
    /// The active tab's agent: its files get a small mark.
    pub agent: Arc<crate::changes::View>,
    pub selected: Option<PathBuf>,
    /// The file open in the preview.
    pub open: Option<PathBuf>,
    pub show_ignored: bool,
    /// The root's listing isn't there yet.
    pub loading: bool,
}

impl TreeData {
    fn same(&self, o: &Self) -> bool {
        self.root == o.root
            && Arc::ptr_eq(&self.rows, &o.rows)
            && Arc::ptr_eq(&self.marks, &o.marks)
            && Arc::ptr_eq(&self.agent, &o.agent)
            && self.selected == o.selected
            && self.open == o.open
            && self.show_ignored == o.show_ignored
            && self.loading == o.loading
    }
}

pub struct FilesPanel {
    data: TreeData,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl FilesPanel {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            data: TreeData::default(),
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
        }
    }

    /// Re-renders only if something it shows changed; `reveal` scrolls the selection into view.
    pub fn set(&mut self, data: TreeData, reveal: bool, cx: &mut Context<Self>) {
        if reveal
            && let Some(sel) = &data.selected
            && let Some(ix) = data.rows.iter().position(|r| &r.entry().path == sel)
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
        if !data.same(&self.data) {
            self.data = data;
            cx.notify();
        }
    }

    /// Perf harness.
    pub fn scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn row(&self, ix: usize, row: &Row) -> gpui::AnyElement {
        let t = theme::get();
        let e = row.entry();
        let d = &self.data;
        let selected = d.selected.as_ref() == Some(&e.path);
        let open = d.open.as_ref() == Some(&e.path);
        let kind = d.marks.files.get(&e.path).copied();
        let color = match kind {
            _ if row.ignored => t.text.faint,
            Some(k) => theme::git_kind(k),
            None => t.text.primary,
        };
        let icon_color = if row.ignored {
            t.text.faint
        } else if kind.is_some() {
            color
        } else {
            t.text.dim
        };
        let (chevron, file_icon) = match (e.is_dir, row.expanded) {
            (true, true) => (Some(icon::CHEVRON_DOWN), icon::FOLDER_OPEN),
            (true, false) => (Some(icon::CHEVRON_RIGHT), icon::FOLDER),
            (false, _) => (None, ui::file_icon(&e.name)),
        };
        let (path, is_dir, ws, focus) = (e.path.clone(), e.is_dir, self.workspace.clone(), self.focus.clone());
        div()
            .id(("tree-row", ix))
            .w_full()
            .overflow_hidden()
            .h(z(height::LIST_ROW))
            .pl(z(space::MD + INDENT * row.depth as f32))
            .pr(z(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap(z(space::XS))
            .cursor_pointer()
            .text_size(z(text_size::MD))
            .when(selected, |d| d.bg(t.bg.selected_focus))
            .when(!selected && open, |d| d.bg(t.bg.selected))
            .when(!selected && !open, |d| d.hover(|s| s.bg(t.bg.hover)))
            .on_click(move |_: &ClickEvent, window, cx| {
                window.focus(&focus, cx);
                let path = path.clone();
                let _ = ws.update(cx, |w, cx| w.tree_click(path, is_dir, window, cx));
            })
            .child(match chevron {
                Some(c) => ui::icon(c, 12., t.text.dim).into_any_element(),
                None => div().flex_none().w(z(12.)).into_any_element(),
            })
            .child(ui::icon(file_icon, 14., icon_color))
            .child(div().min_w_0().truncate().text_color(color).child(e.name.clone()))
            .when(e.is_dir && d.marks.dirs.contains(&e.path), |d| {
                d.child(div().flex_none().size(z(5.)).rounded_full().bg(t.git.modified))
            })
            // Touched by this session's agent: a small accent ring (git kinds keep the color).
            .when(!e.is_dir && d.agent.touched.contains(&e.path), |el| {
                el.child(div().flex_none().size(z(6.)).rounded_full().border_1().border_color(t.accent))
            })
            .into_any_element()
    }
}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("files panel render");
        let d = &self.data;
        let t = theme::get();
        let root_name = d
            .root
            .as_ref()
            .map(|r| r.file_name().map_or(r.display().to_string(), |n| n.to_string_lossy().into_owned()));
        let ws = self.workspace.clone();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(z(space::SM))
            .h(z(height::LIST_ROW))
            .px(z(space::LG))
            .text_size(z(text_size::XS))
            .text_color(t.text.muted)
            .child(ui::icon(icon::FOLDER_OPEN, 12., t.text.dim))
            .child(div().flex_1().min_w_0().truncate().child(root_name.unwrap_or_default()))
            .child(
                div()
                    .id("toggle-ignored")
                    .flex_none()
                    .p(z(space::XXS))
                    .rounded(z(radius::SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg.hover))
                    .child(ui::icon(if d.show_ignored { icon::SHOW } else { icon::HIDE }, 14., t.text.dim))
                    .tooltip(tip(if d.show_ignored {
                        "Ignored files shown: click to hide"
                    } else {
                        "Ignored files hidden: click to show"
                    }))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = ws.update(cx, |w, cx| w.toggle_show_ignored(cx));
                    }),
            );
        let note = |text: &'static str| ui::note(text);
        let body = if d.root.is_none() {
            note("…").into_any_element()
        } else if d.loading {
            note("Loading…").into_any_element()
        } else if d.rows.is_empty() {
            note("Empty directory").into_any_element()
        } else {
            let rows = d.rows.clone();
            let list = uniform_list(
                "tree",
                rows.len(),
                cx.processor(move |p, range: std::ops::Range<usize>, _, _| {
                    range.map(|ix| p.row(ix, &rows[ix])).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full();
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                .into_any_element()
        };
        let ws = self.workspace.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg.panel)
            .track_focus(&self.focus)
            .on_key_down(move |ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if ks.modifiers.modified() {
                    return;
                }
                let key = match ks.key.as_str() {
                    "up" => Key::Up,
                    "down" => Key::Down,
                    "left" => Key::Left,
                    "right" => Key::Right,
                    "home" => Key::Home,
                    "end" => Key::End,
                    "pageup" => Key::PageUp(PAGE),
                    "pagedown" => Key::PageDown(PAGE),
                    "enter" => Key::Enter,
                    "escape" => {
                        let _ = ws.update(cx, |w, cx| w.focus_main(window, cx));
                        cx.stop_propagation();
                        return;
                    }
                    _ => return,
                };
                let _ = ws.update(cx, |w, cx| w.tree_key(key, window, cx));
                cx.stop_propagation();
            })
            .child(header)
            .child(body)
    }
}

/// Text read off the UI thread for the preview.
pub enum Content {
    Binary,
    Lines { lines: Arc<Vec<SharedString>>, truncated: bool },
}

/// Syntax spans per shown line (display byte ranges, `syntax::Class` ids).
pub type LineColors = Arc<Vec<Vec<(std::ops::Range<u32>, u8)>>>;

impl Content {
    /// The content to show, plus the raw text for syntax highlighting.
    pub fn prepare(t: FileText) -> (Self, Option<String>) {
        match t {
            FileText::Binary => (Self::Binary, None),
            FileText::Lines { lines, truncated, raw } => {
                let lines = Arc::new(lines.into_iter().map(SharedString::from).collect());
                (Self::Lines { lines, truncated }, Some(raw))
            }
        }
    }

    /// Highlighted spans of `raw` mapped onto the shown lines (tabs expanded, long lines cut).
    pub fn colors(raw: &str, h: &syntax::Highlighted) -> LineColors {
        Arc::new(
            raw.lines()
                .zip(&h.lines)
                .map(|(line, spans)| {
                    git::text::display_spans(line, spans).into_iter().map(|(r, c)| (r, u8::from(c))).collect()
                })
                .collect(),
        )
    }
}

/// What a file holds, read off the UI thread.
pub enum Loaded {
    /// Editable text, and the hash of the bytes it came from.
    Edit { rope: Rope, format: Format, hash: u64 },
    /// Shown read-only (`why`), with the raw text for syntax colors.
    View { content: Content, raw: Option<String>, why: ReadOnly },
}

impl Loaded {
    /// Blocking: run off the UI thread.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let bytes = git::text::read_capped(path)?;
        Ok(match editor::load(&bytes) {
            Ok((rope, format)) => Self::Edit { hash: editor::hash(&bytes), rope, format },
            Err(why) => {
                let (content, raw) = Content::prepare(git::text::file_text(bytes));
                Self::View { content, raw, why }
            }
        })
    }
}

/// Above the text when the file and my edits disagree.
enum Banner {
    /// Changed on disk while I have unsaved edits: that version, to reload on request.
    Changed {
        rope: Rope,
        format: Format,
        hash: u64,
    },
    /// Ctrl+S found the file changed since it was loaded.
    SaveConflict,
    Failed(SharedString),
    /// Information only (a silent reload); goes away by itself.
    Notice(SharedString),
}

/// How long a `Banner::Notice` stays.
const NOTICE: std::time::Duration = std::time::Duration::from_secs(4);

/// The editing state of an editable file.
struct Editor {
    buf: Buffer,
    format: Format,
    /// Hash of the bytes we loaded or last saved: a different hash on disk means someone else wrote the file.
    base: u64,
    lang: Option<syntax::Lang>,
    /// Raw-line spans; shifted on each edit, replaced when a re-highlight of the current version lands.
    colors: Vec<Spans>,
    /// A highlight is running (at most one; the newest text runs after it).
    coloring: bool,
    cancel: Arc<AtomicUsize>,
    banner: Option<Banner>,
    /// "Show diff": the file on disk vs my text, shown instead of the text.
    compare: Option<Arc<Vec<(char, SharedString)>>>,
    /// Horizontal scroll of the text (the gutter stays).
    hscroll: Pixels,
    saving: bool,
    notice_gen: u64,
    _notice: Task<()>,
    _watch: Option<git::FileWatcher>,
    _wake: Task<()>,
}

/// The file preview over the terminal; text files up to `editor::MAX_BYTES` are edited right here. Esc closes it
/// (asking first while there are unsaved edits).
pub struct FileView {
    pub path: PathBuf,
    /// The repo it's in: its status entry enables "Show diff" (and re-reads a read-only file on refreshes).
    pub repo: Option<PathBuf>,
    /// The tree root the header shows the path relative to.
    base: Option<PathBuf>,
    change: Option<git::FileChange>,
    /// Read-only files.
    content: Option<Content>,
    read_only: Option<ReadOnly>,
    editor: Option<Editor>,
    /// Read-only files: arrives after the content (or never: unknown language, too big, too slow).
    colors: Option<LineColors>,
    failed: bool,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: Pixels,
    cell_width: Pixels,
    /// One px under the terminal's font size (Settings → Terminal).
    font_size: Pixels,
    /// A line to show once the content has arrived (opened from a `path:line` link).
    go_to: Option<usize>,
    /// A search hit: its 0-based line and the matched text, marked in that line.
    mark: Option<(usize, SharedString)>,
}

impl FileView {
    pub fn new(
        path: PathBuf,
        repo: Option<PathBuf>,
        base: Option<PathBuf>,
        m: &crate::terminal_element::Metrics,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            path,
            repo,
            base,
            change: None,
            content: None,
            read_only: None,
            editor: None,
            colors: None,
            failed: false,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height: m.line_height,
            cell_width: m.cell_width,
            font_size: m.font_size - z(1.),
            go_to: None,
            mark: None,
        }
    }

    /// A Search panel hit: shows 1-based `line` and marks `text` in it.
    pub fn go_to_match(&mut self, line: usize, text: String, cx: &mut Context<Self>) {
        self.mark = Some((line.saturating_sub(1), text.into()));
        self.go_to_line(line, cx);
        cx.notify();
    }

    /// Shows 1-based `line` near the top (a few lines of context above it), now or once the content has loaded.
    /// In the editor the cursor goes there too.
    pub fn go_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        if let Some(e) = &mut self.editor {
            let l = line.saturating_sub(1);
            let at = e.buf.pos(l, 0);
            e.buf.click(at, false);
            // A search hit on that line: select it.
            if let Some((ml, m)) = self.mark.take().filter(|(ml, _)| *ml == l)
                && let Some(col) = e.buf.line(ml).find(m.as_ref())
            {
                e.buf.select(e.buf.pos(ml, col), e.buf.pos(ml, col + m.len()));
            }
            self.scroll_to(line.saturating_sub(4), cx);
        } else if self.content.is_some() {
            self.scroll_to(line.saturating_sub(4), cx);
        } else {
            self.go_to = Some(line);
        }
    }

    pub fn set_metrics(&mut self, m: &crate::terminal_element::Metrics, cx: &mut Context<Self>) {
        (self.line_height, self.cell_width, self.font_size) = (m.line_height, m.cell_width, m.font_size - z(1.));
        cx.notify();
    }

    pub fn editable(&self) -> bool {
        self.editor.is_some()
    }

    /// Unsaved edits.
    pub fn dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.buf.dirty())
    }

    /// The user chose to discard the edits (the view is about to close).
    pub fn forget_edits(&mut self) {
        if let Some(e) = &mut self.editor {
            e.buf.mark_saved();
        }
    }

    /// The git status entry of an open editor's file (its content is watched separately).
    pub fn set_change(&mut self, change: Option<git::FileChange>, cx: &mut Context<Self>) {
        if change != self.change {
            self.change = change;
            cx.notify();
        }
    }

    /// Read-only content (binary, too large, …), or None: the read failed.
    pub fn set_content(
        &mut self,
        content: Option<Content>,
        why: Option<ReadOnly>,
        change: Option<git::FileChange>,
        cx: &mut Context<Self>,
    ) {
        self.read_only = why;
        match content {
            Some(c) => {
                let same = match (&self.content, &c) {
                    (Some(Content::Binary), Content::Binary) => true,
                    (Some(Content::Lines { lines: a, truncated: ta }), Content::Lines { lines: b, truncated: tb }) => {
                        a == b && ta == tb
                    }
                    _ => false,
                };
                if same && change == self.change && !self.failed {
                    return;
                }
                self.content = Some(c);
                self.colors = None;
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.change = change;
        if let Some(line) = self.go_to.take().filter(|_| self.content.is_some()) {
            self.scroll.scroll_to_item(line.saturating_sub(4), ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// An editable file arrived: start editing it, watching it on disk and coloring it.
    pub fn open_editor(
        &mut self,
        rope: Rope,
        format: Format,
        hash: u64,
        change: Option<git::FileChange>,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = async_channel::bounded::<()>(1);
        let watch = git::FileWatcher::start(&self.path, move || {
            let _ = tx.try_send(()); // a pending wakeup already covers this one
        })
        .inspect_err(|e| eprintln!("dirigent: watching {} failed: {e}", self.path.display()))
        .ok();
        let wake = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |v, cx| v.disk_touched(cx)).is_err() {
                    break;
                }
            }
        });
        self.editor = Some(Editor {
            buf: Buffer::new(rope),
            format,
            base: hash,
            lang: syntax::detect(&self.path),
            colors: Vec::new(),
            coloring: false,
            cancel: Default::default(),
            banner: None,
            compare: None,
            hscroll: Pixels::ZERO,
            saving: false,
            notice_gen: 0,
            _notice: Task::ready(()),
            _watch: watch,
            _wake: wake,
        });
        self.content = None;
        self.read_only = None;
        self.change = change;
        if let Some(line) = self.go_to.take() {
            self.go_to_line(line, cx);
        }
        self.recolor(cx);
        cx.notify();
    }

    /// Re-highlights the whole text off the UI thread: one run at a time, the newest version after it; a result for
    /// an older version is dropped (the shifted spans stay meanwhile). Notifies only when the colors differ.
    fn recolor(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        let Some(lang) = e.lang.filter(|_| !crate::diff_syntax::syntax_off()) else { return };
        if e.coloring {
            return; // the running one sees the version changed and runs again
        }
        e.coloring = true;
        let (rope, version, cancel) = (e.buf.rope.clone(), e.buf.version(), e.cancel.clone());
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let colors = cx
                .background_executor()
                .spawn(async move {
                    let text = rope.to_string();
                    let h = syntax::highlight(lang, &text, &cancel).ok()?;
                    Some(
                        h.lines
                            .into_iter()
                            .map(|l| l.into_iter().map(|(r, c)| (r, u8::from(c))).collect())
                            .collect::<Vec<Spans>>(),
                    )
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                let Some(e) = &mut v.editor else { return };
                e.coloring = false;
                crate::perf::event_with(|| format!("editor recolor {:.1} ms", started.elapsed().as_secs_f64() * 1e3));
                if e.buf.version() != version {
                    return v.recolor(cx);
                }
                if let Some(c) = colors
                    && c != e.colors
                {
                    e.colors = c;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The watcher saw the file touched: read it off the UI thread and compare.
    fn disk_touched(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let bytes = cx.background_executor().spawn(async move { git::text::read_capped(&path).ok() }).await;
            // Gone (deleted, mid-rename): keep my text; Ctrl+S writes it again.
            let Some(bytes) = bytes else { return };
            let _ = this.update(cx, |v, cx| v.on_disk(bytes, cx));
        })
        .detach();
    }

    fn on_disk(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        let hash = editor::hash(&bytes);
        if hash != e.base && editor::save_bytes(&e.buf.rope, e.format) == bytes {
            // Someone wrote exactly my text: it's saved now.
            e.base = hash;
            e.buf.mark_saved();
            e.banner = None;
            return cx.notify();
        }
        match editor::on_disk_change(e.buf.dirty(), hash, e.base) {
            DiskChange::Ignore => {}
            DiskChange::Reload => match editor::load(&bytes) {
                Ok((rope, format)) => {
                    e.buf.reload(rope);
                    (e.format, e.base, e.compare) = (format, hash, None);
                    e.buf.take_edits();
                    crate::perf::event("editor reloaded from disk");
                    self.notice("Reloaded: file changed on disk", cx);
                    self.recolor(cx);
                    cx.notify();
                }
                Err(why) => {
                    // Not editable any more (binary now, too large): show it read-only.
                    self.editor = None;
                    self.set_content(
                        Some(Content::prepare(git::text::file_text(bytes)).0),
                        Some(why),
                        self.change.clone(),
                        cx,
                    );
                }
            },
            DiskChange::Banner => {
                e.banner = Some(match editor::load(&bytes) {
                    Ok((rope, format)) => Banner::Changed { rope, format, hash },
                    Err(why) => Banner::Failed(
                        format!("Changed on disk, and the new version can't be edited ({}).", why.label()).into(),
                    ),
                });
                cx.notify();
            }
        }
    }

    /// A short notice in the banner row, gone after `NOTICE` (unless something replaced it meanwhile).
    fn notice(&mut self, text: &'static str, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        e.banner = Some(Banner::Notice(text.into()));
        e.notice_gen += 1;
        let generation = e.notice_gen;
        e._notice = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE).await;
            let _ = this.update(cx, |v, cx| {
                if let Some(e) = &mut v.editor
                    && e.notice_gen == generation
                    && matches!(e.banner, Some(Banner::Notice(_)))
                {
                    e.banner = None;
                    cx.notify();
                }
            });
        });
    }

    /// Banner: take the disk version, dropping my edits (undo doesn't bring them back: see `Buffer::reload`).
    fn reload_from_banner(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        let Some(Banner::Changed { rope, format, hash }) = e.banner.take() else { return };
        e.buf.reload(rope);
        (e.format, e.base, e.compare) = (format, hash, None);
        e.buf.take_edits();
        self.recolor(cx);
        cx.notify();
    }

    /// Ctrl+S (`force`: the banner's Overwrite). Off the UI thread: re-check the disk, then write in place.
    fn save(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        if e.saving {
            return;
        }
        e.saving = true;
        let bytes = editor::save_bytes(&e.buf.rope, e.format);
        let (path, base, version) = (self.path.clone(), e.base, e.buf.version());
        cx.spawn(async move |this, cx| {
            let res =
                cx.background_executor().spawn(async move { editor::write_checked(&path, &bytes, base, force) }).await;
            let _ = this.update(cx, |v, cx| {
                let Some(e) = &mut v.editor else { return };
                e.saving = false;
                match res {
                    Ok(Some(hash)) => {
                        e.base = hash;
                        e.buf.mark_saved_at(version);
                        e.banner = None;
                        e.compare = None;
                        crate::perf::event("editor saved");
                    }
                    Ok(None) => e.banner = Some(Banner::SaveConflict),
                    Err(err) => e.banner = Some(Banner::Failed(format!("Couldn't save: {err}").into())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Show diff": the file on disk vs my text, computed off the UI thread.
    fn show_compare(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.editor else { return };
        let (path, mine) = (self.path.clone(), e.buf.rope.clone());
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move {
                    let disk = git::text::read_capped(&path).ok().and_then(|b| editor::load(&b).ok());
                    let disk = disk.map(|(r, _)| r.to_string()).unwrap_or_default();
                    git::text::line_diff(&disk, &mine.to_string())
                        .into_iter()
                        .map(|(k, l)| (k, SharedString::from(git::text::display_line(&l, 0))))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if let Some(e) = &mut v.editor {
                    e.compare = Some(Arc::new(rows));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Rows that fit the view (PageUp/PageDown).
    fn page_rows(&self) -> usize {
        let h = self.scroll.0.borrow().last_item_size.map_or(Pixels::ZERO, |s| s.item.height);
        ((h / self.line_height) as usize).saturating_sub(1).max(1)
    }

    /// The preview font's advance (monospace): the terminal's cell scaled to the 1 px smaller size.
    fn char_width(&self) -> Pixels {
        self.cell_width * (f32::from(self.font_size) / f32::from(self.font_size + z(1.)))
    }

    fn gutter_width(&self, lines: usize) -> Pixels {
        self.cell_width * lines.to_string().len().max(3) as f32 + z(space::XL)
    }

    /// Scrolls so the cursor is visible (vertically next frame via the list, horizontally now).
    fn reveal_cursor(&mut self) {
        let lines = self.editor.as_ref().map_or(0, |e| e.buf.len_lines());
        let gutter = self.gutter_width(lines);
        let cw = self.char_width();
        let width = self.scroll.0.borrow().last_item_size.map_or(Pixels::ZERO, |s| s.item.width) - gutter - z(14.);
        let Some(e) = &mut self.editor else { return };
        let line = e.buf.line_col(e.buf.cursor()).0;
        self.scroll.scroll_to_item(line, ScrollStrategy::Nearest);
        let x = cw * e.buf.cursor_disp_col() as f32;
        let margin = cw * 4.;
        if x < e.hscroll + margin {
            e.hscroll = (x - margin).max(Pixels::ZERO);
        } else if width > margin * 2. && x > e.hscroll + width - margin {
            e.hscroll = x - width + margin;
        }
    }

    /// A key in the editor; false when it isn't the editor's (Esc, app shortcuts, a read-only file).
    fn editor_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let page = self.page_rows();
        let Some(e) = &mut self.editor else { return false };
        let m = &ks.modifiers;
        if e.compare.is_some() || m.platform || m.function {
            return false;
        }
        let v0 = e.buf.version();
        match (ks.key.as_str(), m.control) {
            ("s", true) if !m.shift && !m.alt => {
                self.save(false, cx);
                return true;
            }
            ("v", true) if !m.shift && !m.alt => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    e.buf.paste(&text);
                }
            }
            _ => match e.buf.key(&ks.key, ks.key_char.as_deref(), m.control, m.shift, page) {
                EditKey::Ignored => return false,
                EditKey::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                EditKey::Changed | EditKey::Moved => {}
            },
        }
        crate::perf::key_pressed();
        if e.buf.version() != v0 {
            for edit in e.buf.take_edits() {
                editor::shift_spans(&mut e.colors, &edit);
            }
            self.recolor(cx);
        }
        self.reveal_cursor();
        cx.notify();
        true
    }

    /// Perf harness: types `ch` through the editor's key path.
    pub fn perf_type(&mut self, ch: char, cx: &mut Context<Self>) {
        match ch {
            '\n' => self.perf_key("enter", false, cx),
            c => self.perf_key(&c.to_string(), false, cx),
        }
    }

    /// Harness: a key by gpui name (one char without Ctrl types it).
    pub fn perf_key(&mut self, key: &str, ctrl: bool, cx: &mut Context<Self>) {
        let key_char = (!ctrl && key.chars().count() == 1).then(|| key.to_string());
        let modifiers = gpui::Modifiers { control: ctrl, ..Default::default() };
        self.editor_key(&gpui::Keystroke { key: key.into(), key_char, modifiers }, cx);
    }

    /// Harness: the editor's text, dirty flag and banner kind.
    pub fn perf_state(&self) -> Option<(String, bool, &'static str)> {
        let e = self.editor.as_ref()?;
        let banner = match &e.banner {
            None => "none",
            Some(Banner::Changed { .. }) => "changed",
            Some(Banner::SaveConflict) => "conflict",
            Some(Banner::Failed(_)) => "failed",
            Some(Banner::Notice(_)) => "notice",
        };
        Some((e.buf.rope.to_string(), e.buf.dirty(), banner))
    }

    /// Harness: why the open file is read-only.
    pub fn perf_read_only(&self) -> Option<ReadOnly> {
        self.read_only
    }

    /// Harness: the conflict banner's Overwrite.
    pub fn perf_overwrite(&mut self, cx: &mut Context<Self>) {
        self.save(true, cx);
    }

    /// Mouse on a text row: place the cursor / select (double click a word, triple click the line).
    fn row_mouse(&mut self, line: usize, raw_col: usize, clicks: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(e) = &mut self.editor else { return };
        let at = e.buf.pos(line, raw_col);
        let before = (e.buf.cursor(), e.buf.selection());
        match clicks {
            2 => e.buf.select_word(at),
            3.. => e.buf.select_line(at),
            _ => e.buf.click(at, extend),
        }
        if (e.buf.cursor(), e.buf.selection()) != before {
            cx.notify();
        }
    }

    /// One editor row: gutter, text with syntax colors and the selection, the caret, mouse handling.
    fn edit_row(&self, ix: usize, gutter: Pixels, focused: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let lh = self.line_height;
        let Some(e) = &self.editor else { return div().into_any_element() };
        let raw = e.buf.line(ix);
        let disp = git::text::display_line(&raw, 0);
        // Syntax colors, then the selection's background split over them (gpui wants sorted, disjoint ranges).
        let syn: Vec<(Range<usize>, gpui::Hsla)> = e
            .colors
            .get(ix)
            .map(|s| {
                git::text::display_spans(&raw, s)
                    .into_iter()
                    .filter_map(|(r, c)| Some((r.start as usize..r.end as usize, t.syntax.color(c)?.into())))
                    .collect()
            })
            .unwrap_or_default();
        let sel = e.buf.selection();
        let start = e.buf.rope.line_to_char(ix);
        let content_end = start + raw.chars().count();
        let sel = (!sel.is_empty() && sel.start <= content_end && sel.end > start).then(|| {
            let a = if sel.start <= start { 0 } else { editor::to_display(&raw, e.buf.line_col(sel.start).1) };
            // Past the line's end: its newline, shown as one selected cell.
            let b = if sel.end > content_end {
                disp.len() + 1
            } else {
                editor::to_display(&raw, e.buf.line_col(sel.end).1)
            };
            a..b
        });
        let highlights = merge_runs(&syn, sel, t.bg.selected_focus.into(), disp.len() + 1);
        let text = StyledText::new(format!("{disp} ")).with_highlights(highlights);
        let layout = text.layout().clone();
        let (cur_line, cur_col) = e.buf.line_col(e.buf.cursor());
        let caret = (focused && cur_line == ix).then(|| editor::to_display(&raw, cur_col));
        let raw: Arc<str> = raw.into();
        let hit = {
            let (layout, raw) = (layout.clone(), raw.clone());
            move |pos: gpui::Point<Pixels>| {
                let i = match layout.index_for_position(pos) {
                    Ok(i) | Err(i) => i,
                };
                editor::from_display(&raw, i)
            }
        };
        let hit2 = hit.clone();
        let accent = t.accent;
        div()
            .h(lh)
            .flex()
            .w_full()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .w(gutter)
                    .pr(z(space::LG))
                    .flex()
                    .justify_end()
                    .text_color(if caret.is_some() { t.text.muted } else { t.text.faint })
                    .child((ix + 1).to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |v, ev: &MouseDownEvent, window, cx| {
                            window.focus(&v.focus, cx);
                            v.row_mouse(ix, hit(ev.position), ev.click_count, ev.modifiers.shift, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |v, ev: &MouseMoveEvent, _, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            v.row_mouse(ix, hit2(ev.position), 1, true, cx);
                        }
                    }))
                    .child(
                        div()
                            .relative()
                            .ml(-e.hscroll)
                            .whitespace_nowrap()
                            .text_color(t.diff.context)
                            .child(text)
                            .children(caret.map(|col| {
                                canvas(
                                    |_, _, _| {},
                                    move |_, _, window, _| {
                                        if let Some(p) = layout.position_for_index(col) {
                                            window.paint_quad(fill(Bounds::new(p, size(z(1.5), lh)), accent));
                                        }
                                        crate::perf::key_painted();
                                    },
                                )
                                .absolute()
                                .size_full()
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// Syntax color runs plus a selection background, as sorted, disjoint highlight ranges over `0..len`.
fn merge_runs(
    syn: &[(Range<usize>, gpui::Hsla)],
    sel: Option<Range<usize>>,
    sel_bg: gpui::Hsla,
    len: usize,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let style = |color: Option<gpui::Hsla>, selected: bool| HighlightStyle {
        color,
        background_color: selected.then_some(sel_bg),
        ..Default::default()
    };
    let Some(sel) = sel.filter(|s| !s.is_empty()) else {
        return syn.iter().map(|(r, c)| (r.clone(), style(Some(*c), false))).collect();
    };
    let mut cuts: Vec<usize> = vec![0, len, sel.start, sel.end];
    cuts.extend(syn.iter().flat_map(|(r, _)| [r.start, r.end]));
    cuts.retain(|&c| c <= len);
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let color = syn.iter().find(|(r, _)| r.start <= a && b <= r.end).map(|(_, c)| *c);
        let selected = sel.start <= a && b <= sel.end;
        if color.is_some() || selected {
            out.push((a..b, style(color, selected)));
        }
    }
    out
}

impl FileView {
    /// Perf harness: loaded rows, whether colors arrived, and jump to a row.
    pub fn loaded_rows(&self) -> usize {
        match (&self.editor, &self.content) {
            (Some(e), _) => e.buf.len_lines(),
            (_, Some(Content::Lines { lines, .. })) => lines.len(),
            _ => 0,
        }
    }

    pub fn colored(&self) -> bool {
        match &self.editor {
            Some(e) => !e.colors.is_empty(),
            None => self.colors.is_some(),
        }
    }

    pub fn scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    pub fn set_colors(&mut self, colors: LineColors, cx: &mut Context<Self>) {
        self.colors = Some(colors);
        cx.notify();
    }

    /// Screenshot harness: jump to the last row once loaded; returns whether it could.
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) -> bool {
        let n = match (&self.editor, &self.content) {
            (Some(e), _) => e.buf.len_lines(),
            (_, Some(Content::Lines { lines, truncated })) => lines.len() + *truncated as usize,
            _ => return false,
        };
        self.scroll.scroll_to_item(n, ScrollStrategy::Bottom);
        cx.notify();
        true
    }
}

impl Drop for FileView {
    fn drop(&mut self) {
        if let Some(e) = &self.editor {
            e.cancel.store(1, Ordering::Relaxed); // a running highlight stops
        }
    }
}

impl Focusable for FileView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// `path` relative to `base` for display.
fn rel(path: &Path, base: Option<&Path>) -> String {
    base.and_then(|b| path.strip_prefix(b).ok()).unwrap_or(path).display().to_string()
}

/// A small header/banner button.
fn chip(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex_none()
        .px(z(space::MD))
        .py(z(space::XXS))
        .rounded(z(radius::SM))
        .cursor_pointer()
        .bg(t.bg.selected)
        .hover(|s| s.bg(t.bg.hover))
        .text_size(z(text_size::XS))
        .text_color(t.text.primary)
        .child(label)
}

impl FileView {
    fn banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let e = self.editor.as_ref()?;
        let (text, buttons): (SharedString, Vec<gpui::AnyElement>) = match e.banner.as_ref()? {
            Banner::Changed { .. } => (
                "Changed on disk. You have unsaved edits.".into(),
                vec![
                    chip("reload", "Reload (lose mine)")
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.reload_from_banner(cx)))
                        .into_any_element(),
                    chip("keep", "Keep mine")
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                            if let Some(e) = &mut v.editor {
                                e.banner = None;
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                ],
            ),
            Banner::SaveConflict => (
                "Not saved: the file changed on disk since you opened it.".into(),
                vec![
                    chip("overwrite", "Overwrite")
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.save(true, cx)))
                        .into_any_element(),
                    chip("cancel", "Cancel")
                        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                            if let Some(e) = &mut v.editor {
                                e.banner = None;
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                ],
            ),
            Banner::Failed(msg) | Banner::Notice(msg) => (msg.clone(), Vec::new()),
        };
        let notice = matches!(e.banner, Some(Banner::Notice(_)));
        let diff = matches!(e.banner, Some(Banner::Changed { .. } | Banner::SaveConflict)).then(|| {
            chip("disk-diff", if e.compare.is_some() { "Back to editor" } else { "Show diff" })
                .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    let open = v.editor.as_ref().is_some_and(|e| e.compare.is_some());
                    if open {
                        if let Some(e) = &mut v.editor {
                            e.compare = None;
                        }
                        cx.notify();
                    } else {
                        v.show_compare(cx);
                    }
                }))
                .into_any_element()
        });
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(z(space::MD))
                .px(z(space::LG))
                .py(z(space::SM))
                .bg(t.bg.elevated)
                .border_b_1()
                .border_color(t.border.subtle)
                .text_size(z(text_size::SM))
                .text_color(t.text.primary)
                .child(if notice {
                    ui::icon(icon::REFRESH, 14., t.text.muted)
                } else {
                    ui::icon(icon::WARNING, 14., t.state.needs_you)
                })
                .child(div().flex_1().min_w_0().truncate().child(text))
                .children(buttons)
                .children(diff)
                .into_any_element(),
        )
    }

    /// "Show diff": unified rows of the file on disk (-) vs my text (+).
    fn compare_list(&self, rows: Arc<Vec<(char, SharedString)>>) -> gpui::AnyElement {
        let lh = self.line_height;
        if rows.is_empty() {
            return ui::note("No differences").into_any_element();
        }
        uniform_list("disk-diff", rows.len(), move |range, _, _| {
            let t = theme::get();
            range
                .map(|ix| {
                    let (kind, text) = &rows[ix];
                    let (bg, fg) = match kind {
                        '-' => (Some(t.diff.del_bg), t.diff.del_fg),
                        '+' => (Some(t.diff.add_bg), t.diff.add_fg),
                        '⋯' => (None, t.text.faint),
                        _ => (None, t.diff.context),
                    };
                    div()
                        .h(lh)
                        .px(z(space::LG))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when_some(bg, |d, bg| d.bg(bg))
                        .text_color(fg)
                        .child(format!("{kind} {text}"))
                })
                .collect::<Vec<_>>()
        })
        .font_family(MONO_FONT)
        .font_features(gpui::FontFeatures::disable_ligatures())
        .text_size(self.font_size)
        .line_height(lh)
        .size_full()
        .into_any_element()
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::editor_render();
        let t = theme::get();
        let ws = self.workspace.clone();
        let show_diff = self.change.clone().zip(self.repo.clone()).map(|(change, root)| {
            let ws = ws.clone();
            div()
                .id("show-diff")
                .flex_none()
                .flex()
                .items_center()
                .gap(z(space::XS))
                .px(z(space::MD))
                .py(z(space::XXS))
                .rounded(z(radius::SM))
                .cursor_pointer()
                .bg(t.bg.selected)
                .hover(|s| s.bg(t.bg.hover))
                .text_size(z(text_size::XS))
                .child(ui::icon(icon::CHANGES, 12., t.accent))
                .child("Show diff")
                .on_click(move |_: &ClickEvent, window, cx| {
                    let (root, change) = (root.clone(), change.clone());
                    let _ = ws.update(cx, |w, cx| w.toggle_diff(root, change, window, cx));
                })
        });
        let name = self.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let icon_color = self.change.as_ref().map_or(t.text.dim, |c| theme::git_kind(c.kind));
        let dirty = self.dirty();
        crate::perf::event_with(|| {
            let banner = self.editor.as_ref().and_then(|e| e.banner.as_ref()).is_some();
            format!("file view render: dirty {dirty}, banner {banner}")
        });
        let hint = match &self.editor {
            Some(e) if e.compare.is_some() => "Esc back to the editor",
            Some(_) => "Ctrl+S save · Esc close",
            None => "Esc to close",
        };
        let header = ui::overlay_header()
            .child(ui::icon(ui::file_icon(&name), 14., icon_color))
            .child(div().min_w_0().truncate().text_color(t.text.primary).child(rel(&self.path, self.base.as_deref())))
            // Unsaved edits: a dot after the name.
            .when(dirty, |d| d.child(div().flex_none().size(z(7.)).rounded_full().bg(t.text.primary)))
            .children(
                self.change.as_ref().map(|c| div().flex_none().text_size(z(text_size::XS)).child(line_counts(c.lines))),
            )
            .children(self.read_only.map(|r| ui::pill(r.label())))
            .children(show_diff)
            .child(ui::hint(hint));
        let body = if let Some(e) = &self.editor {
            if let Some(rows) = e.compare.clone() {
                div().relative().flex_1().min_h_0().child(self.compare_list(rows)).into_any_element()
            } else {
                let lh = self.line_height;
                let gutter = self.gutter_width(e.buf.len_lines());
                let focused = self.focus.is_focused(window);
                let list = uniform_list(
                    "file-lines",
                    e.buf.len_lines(),
                    cx.processor(move |v, range: Range<usize>, _, cx| {
                        range.map(|ix| v.edit_row(ix, gutter, focused, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .font_family(MONO_FONT)
                .font_features(gpui::FontFeatures::disable_ligatures())
                .text_size(self.font_size)
                .line_height(lh);
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Horizontal wheel (or Shift+wheel) scrolls the text; the gutter stays.
                    .on_scroll(cx.listener(move |v, ev: &ScrollEvent, _, cx| {
                        let d = ev.delta.pixel_delta(lh);
                        let dx = if d.x == Pixels::ZERO && ev.modifiers.shift { d.y } else { d.x };
                        if dx != Pixels::ZERO
                            && let Some(e) = &mut v.editor
                        {
                            e.hscroll = (e.hscroll - dx).max(Pixels::ZERO);
                            cx.notify();
                        }
                    }))
                    .child(list.size_full())
                    .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                    .into_any_element()
            }
        } else {
            self.read_only_body(cx)
        };
        let banner = self.banner(cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg.base)
            .track_focus(&self.focus)
            // A plain closure, not `cx.listener`: Esc calls into the workspace, which reads this view.
            .on_key_down({
                let (ws, me) = (self.workspace.clone(), cx.entity().downgrade());
                move |ev: &KeyDownEvent, window, cx| {
                    let ks = &ev.keystroke;
                    if ks.key == "escape" && !ks.modifiers.modified() {
                        let compare = me.update(cx, |v, cx| {
                            let e = v.editor.as_mut().filter(|e| e.compare.is_some())?;
                            e.compare = None;
                            cx.notify();
                            Some(())
                        });
                        if !matches!(compare, Ok(Some(()))) {
                            let _ = ws.update(cx, |w, cx| w.close_overlay(window, cx));
                        }
                        cx.stop_propagation();
                    } else if me.update(cx, |v, cx| v.editor_key(ks, cx)).unwrap_or(false) {
                        cx.stop_propagation();
                    }
                }
            })
            .child(header)
            .children(banner)
            .child(body)
    }
}

impl FileView {
    fn read_only_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        match (&self.content, self.failed) {
            (_, true) => ui::note("Couldn't read the file").text_color(t.state.error).into_any_element(),
            (None, _) => ui::note("Loading…").into_any_element(),
            (Some(Content::Binary), _) => ui::note("Binary file").into_any_element(),
            (Some(Content::Lines { lines, .. }), _) if lines.is_empty() => ui::note("Empty file").into_any_element(),
            (Some(Content::Lines { lines, truncated }), _) => {
                let (lines, lh, truncated) = (lines.clone(), self.line_height, *truncated);
                let colors = self.colors.clone();
                let mark = self.mark.clone();
                let gutter = self.gutter_width(lines.len());
                let count = lines.len() + truncated as usize;
                let list = uniform_list("file-lines", count, move |range, _, _| {
                    let t = theme::get();
                    range
                        .map(|ix| {
                            let row = div().h(lh).flex().whitespace_nowrap().overflow_hidden();
                            let Some(text) = lines.get(ix) else {
                                return row
                                    .pl(z(space::LG))
                                    .text_color(t.state.needs_you)
                                    .child("… file truncated (too large to show)");
                            };
                            row.child(
                                div()
                                    .flex_none()
                                    .w(gutter)
                                    .pr(z(space::LG))
                                    .flex()
                                    .justify_end()
                                    .text_color(t.diff.gutter)
                                    .child((ix + 1).to_string()),
                            )
                            .child(div().pr(z(14.)).text_color(t.diff.context).child({
                                // The marked search hit: the match on a highlight (syntax colors left out on that line).
                                let marked = mark
                                    .as_ref()
                                    .filter(|(l, _)| *l == ix)
                                    .and_then(|(_, m)| text.find(m.as_ref()).map(|s| s..s + m.len()));
                                match colors
                                    .as_ref()
                                    .and_then(|c| c.get(ix))
                                    .filter(|s| !s.is_empty() && marked.is_none())
                                {
                                    Some(spans) => StyledText::new(text.clone())
                                        .with_highlights(spans.iter().filter_map(|(r, c)| {
                                            let color = t.syntax.color(*c)?;
                                            Some((
                                                r.start as usize..r.end as usize,
                                                HighlightStyle { color: Some(color.into()), ..Default::default() },
                                            ))
                                        }))
                                        .into_any_element(),
                                    None => match marked {
                                        Some(r) => StyledText::new(text.clone())
                                            .with_highlights([(
                                                r,
                                                HighlightStyle {
                                                    background_color: Some(t.search_match.into()),
                                                    color: Some(t.text.primary.into()),
                                                    ..Default::default()
                                                },
                                            )])
                                            .into_any_element(),
                                        None => text.clone().into_any_element(),
                                    },
                                }
                            }))
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(MONO_FONT)
                .font_features(gpui::FontFeatures::disable_ligatures())
                .text_size(self.font_size)
                .line_height(lh);
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list.size_full())
                    .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                    .into_any_element()
            }
        }
    }
}
