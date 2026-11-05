//! The "Files" tab of the right panel (a lazy, virtualized file tree) and the read-only file preview shown over the
//! terminal. Both are cached entities; the workspace owns the tree state and pushes it in. Handlers that call into
//! the workspace are plain closures with a weak handle, never `cx.listener` (the workspace updates these views back).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use git::text::FileText;
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, KeyDownEvent, Pixels, ScrollStrategy, SharedString,
    UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::file_tree::{Key, Marks, Row};
use crate::git_panel::{kind_color, line_counts};
use crate::scrollbar::{self, scrollbar};
use crate::terminal_element::{FONT_FAMILY, FONT_SIZE, palette};
use crate::workspace::{Workspace, colors};

const ROW_H: f32 = 22.;
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
        let e = row.entry();
        let d = &self.data;
        let selected = d.selected.as_ref() == Some(&e.path);
        let open = d.open.as_ref() == Some(&e.path);
        let kind = d.marks.files.get(&e.path).copied();
        let color = match kind {
            _ if row.ignored => 0x5a5a66,
            Some(k) => kind_color(k),
            None => 0xc8c8c8,
        };
        let chevron = if !e.is_dir {
            ""
        } else if row.expanded {
            "▾"
        } else {
            "▸"
        };
        let (path, is_dir, ws, focus) = (e.path.clone(), e.is_dir, self.workspace.clone(), self.focus.clone());
        div()
            .id(("tree-row", ix))
            .w_full()
            .overflow_hidden()
            .h(px(ROW_H))
            .pl(px(8. + INDENT * row.depth as f32))
            .pr(px(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .text_sm()
            .when(selected, |d| d.bg(rgb(0x2f3a52)))
            .when(!selected && open, |d| d.bg(rgb(colors::ROW_ACTIVE)))
            .when(!selected && !open, |d| d.hover(|s| s.bg(rgb(colors::ROW_HOVER))))
            .on_click(move |_: &ClickEvent, window, cx| {
                window.focus(&focus, cx);
                let path = path.clone();
                let _ = ws.update(cx, |w, cx| w.tree_click(path, is_dir, window, cx));
            })
            .child(div().flex_none().w(px(12.)).text_xs().text_color(rgb(colors::DIM)).child(chevron))
            .child(div().min_w_0().truncate().text_color(rgb(color)).child(e.name.clone()))
            .when(e.is_dir && d.marks.dirs.contains(&e.path), |d| {
                d.child(div().flex_none().size(px(5.)).rounded_full().bg(rgb(0xd0b060)))
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
        let note = |text: &'static str| div().px_3().py_2().text_xs().text_color(rgb(colors::DIM)).child(text);
        let root_name = d.root.as_ref().map(|r| r.file_name().map_or(r.display().to_string(), |n| n.to_string_lossy().into_owned()));
        let ws = self.workspace.clone();
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(rgb(colors::DIM)).child(root_name.unwrap_or_default()))
            .child(
                div()
                    .id("toggle-ignored")
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(rgb(colors::DIM))
                    .hover(|s| s.bg(rgb(colors::ROW_HOVER)))
                    .child(if d.show_ignored { "Hide ignored" } else { "Show ignored" })
                    .on_click(move |_: &ClickEvent, _, cx| {
                        let _ = ws.update(cx, |w, cx| w.toggle_show_ignored(cx));
                    }),
            );
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
            .bg(rgb(colors::SIDEBAR))
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

impl Content {
    pub fn prepare(t: FileText) -> Self {
        match t {
            FileText::Binary => Self::Binary,
            FileText::Lines { lines, truncated } => {
                Self::Lines { lines: Arc::new(lines.into_iter().map(SharedString::from).collect()), truncated }
            }
        }
    }
}

/// A read-only file preview over the terminal. Esc closes it.
pub struct FileView {
    pub path: PathBuf,
    /// The repo it's in: re-read whenever that repo refreshes; its status entry enables "Show diff".
    pub repo: Option<PathBuf>,
    /// The tree root the header shows the path relative to.
    base: Option<PathBuf>,
    change: Option<git::FileChange>,
    content: Option<Content>,
    failed: bool,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: Pixels,
    cell_width: Pixels,
}

impl FileView {
    pub fn new(
        path: PathBuf,
        repo: Option<PathBuf>,
        base: Option<PathBuf>,
        metrics: (Pixels, Pixels),
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            path,
            repo,
            base,
            change: None,
            content: None,
            failed: false,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height: metrics.0,
            cell_width: metrics.1,
        }
    }

    pub fn set_content(&mut self, content: Option<Content>, change: Option<git::FileChange>, cx: &mut Context<Self>) {
        match content {
            Some(c) => {
                let same = match (&self.content, &c) {
                    (Some(Content::Binary), Content::Binary) => true,
                    (Some(Content::Lines { lines: a, truncated: ta }), Content::Lines { lines: b, truncated: tb }) => a == b && ta == tb,
                    _ => false,
                };
                if same && change == self.change && !self.failed {
                    return;
                }
                self.content = Some(c);
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.change = change;
        cx.notify();
    }

}

impl FileView {
    /// Screenshot harness: jump to the last row once loaded; returns whether it could.
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(Content::Lines { lines, truncated }) = &self.content else { return false };
        self.scroll.scroll_to_item(lines.len() + *truncated as usize, ScrollStrategy::Bottom);
        cx.notify();
        true
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

impl Render for FileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("file view render");
        let ws = self.workspace.clone();
        let show_diff = self.change.clone().zip(self.repo.clone()).map(|(change, root)| {
            let ws = ws.clone();
            div()
                .id("show-diff")
                .flex_none()
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .bg(rgb(colors::ROW_ACTIVE))
                .hover(|s| s.bg(rgb(colors::DIVIDER_HOVER)))
                .text_xs()
                .child("Show diff")
                .on_click(move |_: &ClickEvent, window, cx| {
                    let (root, change) = (root.clone(), change.clone());
                    let _ = ws.update(cx, |w, cx| w.toggle_diff(root, change, window, cx));
                })
        });
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .py_1()
            .bg(rgb(colors::SIDEBAR))
            .text_sm()
            .children(self.change.as_ref().map(|c| div().text_color(rgb(kind_color(c.kind))).child(c.kind.to_string())))
            .child(div().min_w_0().truncate().child(rel(&self.path, self.base.as_deref())))
            .children(self.change.as_ref().map(|c| div().flex_none().text_xs().child(line_counts(c.lines))))
            .children(show_diff)
            .child(div().ml_auto().flex_none().text_xs().text_color(rgb(colors::DIM)).child("Esc to close"));
        let note = |text: &'static str, color: u32| div().p_3().text_color(rgb(color)).child(text).into_any_element();
        let body = match (&self.content, self.failed) {
            (_, true) => note("Couldn't read the file", colors::EXITED),
            (None, _) => note("Loading…", colors::DIM),
            (Some(Content::Binary), _) => note("Binary file", colors::DIM),
            (Some(Content::Lines { lines, .. }), _) if lines.is_empty() => note("Empty file", colors::DIM),
            (Some(Content::Lines { lines, truncated }), _) => {
                let (lines, lh, truncated) = (lines.clone(), self.line_height, *truncated);
                let digits = lines.len().to_string().len().max(3) as f32;
                let gutter = self.cell_width * digits + px(16.);
                let count = lines.len() + truncated as usize;
                let list = uniform_list("file-lines", count, move |range, _, _| {
                    range
                        .map(|ix| {
                            let row = div().h(lh).flex().whitespace_nowrap().overflow_hidden();
                            let Some(text) = lines.get(ix) else {
                                return row.pl_3().text_color(rgb(colors::NEEDS_YOU)).child("… file truncated (too large to show)");
                            };
                            row.child(
                                div().flex_none().w(gutter).pr_3().flex().justify_end().text_color(rgb(0x5a5a66)).child((ix + 1).to_string()),
                            )
                            .child(div().pr(px(14.)).text_color(rgb(0xc8c8c8)).child(text.clone()))
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(FONT_FAMILY)
                .text_size(px(FONT_SIZE - 1.))
                .line_height(lh);
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
            .on_key_down(move |ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if ks.key == "escape" && !ks.modifiers.modified() {
                    let _ = ws.update(cx, |w, cx| w.close_overlay(window, cx));
                    cx.stop_propagation();
                }
            })
            .child(header)
            .child(body)
    }
}
