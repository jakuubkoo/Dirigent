//! The "Files" tab of the right panel (a lazy, virtualized file tree) and the read-only file preview shown over the
//! terminal. Both are cached entities; the workspace owns the tree state and pushes it in. Handlers that call into
//! the workspace are plain closures with a weak handle, never `cx.listener` (the workspace updates these views back).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use git::text::FileText;
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, Pixels, ScrollStrategy, SharedString,
    StyledText, UniformListScrollHandle, WeakEntity, Window, div, prelude::*, px, uniform_list,
};

use crate::file_tree::{Key, Marks, Row};
use crate::theme::{self, MONO_FONT, TERMINAL_FONT_SIZE, height, radius, space, text_size};
use crate::ui::{self, icon, line_counts, tip};
use crate::scrollbar::{self, scrollbar};
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
        let icon_color = if row.ignored { t.text.faint } else if kind.is_some() { color } else { t.text.dim };
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
            .h(px(height::LIST_ROW))
            .pl(px(space::MD + INDENT * row.depth as f32))
            .pr(px(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap(px(space::XS))
            .cursor_pointer()
            .text_size(px(text_size::MD))
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
                None => div().flex_none().w(px(12.)).into_any_element(),
            })
            .child(ui::icon(file_icon, 14., icon_color))
            .child(div().min_w_0().truncate().text_color(color).child(e.name.clone()))
            .when(e.is_dir && d.marks.dirs.contains(&e.path), |d| {
                d.child(div().flex_none().size(px(5.)).rounded_full().bg(t.git.modified))
            })
            // Touched by this session's agent: a small accent ring (git kinds keep the color).
            .when(!e.is_dir && d.agent.touched.contains(&e.path), |el| {
                el.child(div().flex_none().size(px(6.)).rounded_full().border_1().border_color(t.accent))
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
        let root_name = d.root.as_ref().map(|r| r.file_name().map_or(r.display().to_string(), |n| n.to_string_lossy().into_owned()));
        let ws = self.workspace.clone();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::SM))
            .h(px(height::LIST_ROW))
            .px(px(space::LG))
            .text_size(px(text_size::XS))
            .text_color(t.text.muted)
            .child(ui::icon(icon::FOLDER_OPEN, 12., t.text.dim))
            .child(div().flex_1().min_w_0().truncate().child(root_name.unwrap_or_default()))
            .child(
                div()
                    .id("toggle-ignored")
                    .flex_none()
                    .p(px(space::XXS))
                    .rounded(px(radius::SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg.hover))
                    .child(ui::icon(if d.show_ignored { icon::SHOW } else { icon::HIDE }, 14., t.text.dim))
                    .tooltip(tip(if d.show_ignored { "Ignored files shown: click to hide" } else { "Ignored files hidden: click to show" }))
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

/// A read-only file preview over the terminal. Esc closes it.
pub struct FileView {
    pub path: PathBuf,
    /// The repo it's in: re-read whenever that repo refreshes; its status entry enables "Show diff".
    pub repo: Option<PathBuf>,
    /// The tree root the header shows the path relative to.
    base: Option<PathBuf>,
    change: Option<git::FileChange>,
    content: Option<Content>,
    /// Arrives after the content (or never: unknown language, too big, too slow).
    colors: Option<LineColors>,
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
            colors: None,
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
                self.colors = None;
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.change = change;
        cx.notify();
    }

}

impl FileView {
    /// Perf harness: loaded rows, whether colors arrived, and jump to a row.
    pub fn loaded_rows(&self) -> usize {
        match &self.content {
            Some(Content::Lines { lines, .. }) => lines.len(),
            _ => 0,
        }
    }

    pub fn colored(&self) -> bool {
        self.colors.is_some()
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
        let t = theme::get();
        let ws = self.workspace.clone();
        let show_diff = self.change.clone().zip(self.repo.clone()).map(|(change, root)| {
            let ws = ws.clone();
            div()
                .id("show-diff")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::XS))
                .px(px(space::MD))
                .py(px(space::XXS))
                .rounded(px(radius::SM))
                .cursor_pointer()
                .bg(t.bg.selected)
                .hover(|s| s.bg(t.bg.hover))
                .text_size(px(text_size::XS))
                .child(ui::icon(icon::CHANGES, 12., t.accent))
                .child("Show diff")
                .on_click(move |_: &ClickEvent, window, cx| {
                    let (root, change) = (root.clone(), change.clone());
                    let _ = ws.update(cx, |w, cx| w.toggle_diff(root, change, window, cx));
                })
        });
        let name = self.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let icon_color = self.change.as_ref().map_or(t.text.dim, |c| theme::git_kind(c.kind));
        let header = ui::overlay_header()
            .child(ui::icon(ui::file_icon(&name), 14., icon_color))
            .child(div().min_w_0().truncate().text_color(t.text.primary).child(rel(&self.path, self.base.as_deref())))
            .children(self.change.as_ref().map(|c| div().flex_none().text_size(px(text_size::XS)).child(line_counts(c.lines))))
            .children(show_diff)
            .child(ui::hint("Esc to close"));
        let body = match (&self.content, self.failed) {
            (_, true) => ui::note("Couldn't read the file").text_color(t.state.error).into_any_element(),
            (None, _) => ui::note("Loading…").into_any_element(),
            (Some(Content::Binary), _) => ui::note("Binary file").into_any_element(),
            (Some(Content::Lines { lines, .. }), _) if lines.is_empty() => ui::note("Empty file").into_any_element(),
            (Some(Content::Lines { lines, truncated }), _) => {
                let (lines, lh, truncated) = (lines.clone(), self.line_height, *truncated);
                let colors = self.colors.clone();
                let digits = lines.len().to_string().len().max(3) as f32;
                let gutter = self.cell_width * digits + px(space::XL);
                let count = lines.len() + truncated as usize;
                let list = uniform_list("file-lines", count, move |range, _, _| {
                    let t = theme::get();
                    range
                        .map(|ix| {
                            let row = div().h(lh).flex().whitespace_nowrap().overflow_hidden();
                            let Some(text) = lines.get(ix) else {
                                return row.pl(px(space::LG)).text_color(t.state.needs_you).child("… file truncated (too large to show)");
                            };
                            row.child(
                                div()
                                    .flex_none()
                                    .w(gutter)
                                    .pr(px(space::LG))
                                    .flex()
                                    .justify_end()
                                    .text_color(t.text.faint)
                                    .child((ix + 1).to_string()),
                            )
                            .child(div().pr(px(14.)).text_color(t.diff.context).child(
                                match colors.as_ref().and_then(|c| c.get(ix)).filter(|s| !s.is_empty()) {
                                    Some(spans) => StyledText::new(text.clone())
                                        .with_highlights(spans.iter().filter_map(|(r, c)| {
                                            let color = t.syntax.color(*c)?;
                                            Some((r.start as usize..r.end as usize, HighlightStyle { color: Some(color.into()), ..Default::default() }))
                                        }))
                                        .into_any_element(),
                                    None => text.clone().into_any_element(),
                                },
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(MONO_FONT)
                .font_features(gpui::FontFeatures::disable_ligatures())
                .text_size(px(TERMINAL_FONT_SIZE - 1.))
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
            .bg(t.bg.base)
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
