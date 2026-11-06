//! The Command Center: a search box centred in the title bar (a slim bar above the main area with server-side
//! decorations) and its dropdown. No prefix = files of the active session's project (quick open), `>` = commands
//! (the shortcut table), `#` = text in files (results in the Search panel), `:` = a line in the open preview.

use std::cell::Cell;
use std::rc::Rc;

use git::index::Entry;
use gpui::{
    AnyElement, Bounds, ClickEvent, HighlightStyle, Pixels, ScrollStrategy, SharedString, StyledText,
    UniformListScrollHandle, canvas, div, uniform_list,
};

use super::*;
use crate::quick_open::{self, Mode};
use crate::theme::{self, MONO_FONT, height, radius, space, text_size, z};
use crate::ui::{self, icon};

const WIDTH: f32 = 640.;
const ROWS: f32 = 12.;
const ROW: f32 = 34.;
/// Files shown for a query.
const LIMIT: usize = 100;
const RECENT: usize = 20;

pub(super) struct Cc {
    pub edit: LineEdit,
    /// The project searched (the active session's repo root, else its directory).
    root: Option<PathBuf>,
    files: Arc<Vec<Entry>>,
    capped: bool,
    loading: bool,
    /// Include gitignored files.
    ignored: bool,
    rows: Vec<Row>,
    sel: usize,
    scroll: UniformListScrollHandle,
    /// Newest filter run; older results are dropped.
    run: u64,
    commands: Vec<(&'static str, &'static str, Box<dyn gpui::Action>)>,
}

enum Row {
    File(quick_open::Hit),
    /// Index into `commands`, matched chars of its label.
    Command(usize, Vec<usize>),
}

/// Where the box was drawn (the dropdown hangs under it).
pub(super) type Anchor = Rc<Cell<Option<Bounds<Pixels>>>>;

impl Workspace {
    /// Ctrl+P (files), Ctrl+Shift+P (`>`), Ctrl+Shift+F (`#`), or a click on the box: opens the dropdown with
    /// `prefix` typed (or switches an open one to it).
    pub(super) fn open_cc(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(Modal::CommandCenter(cc)) = &mut self.modal {
            if quick_open::parse(&cc.edit.text).0 != quick_open::parse(prefix).0 {
                cc.edit = LineEdit::new(prefix);
            }
            return self.cc_changed(cx);
        }
        let cc = Cc {
            edit: LineEdit::new(prefix),
            root: None,
            files: Default::default(),
            capped: false,
            loading: true,
            ignored: false,
            rows: Vec::new(),
            sel: 0,
            scroll: UniformListScrollHandle::new(),
            run: 0,
            commands: crate::keys::commands(),
        };
        self.modal = Some(Modal::CommandCenter(Box::new(cc)));
        window.focus(&self.focus, cx);
        self.cc_load(cx);
        self.cc_changed(cx);
    }

    /// (Re)loads the file list off-thread; rows refresh when it arrives.
    fn cc_load(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::CommandCenter(cc)) = &self.modal else { return };
        let Some((root, task)) = self.project_files(cc.ignored, cx) else {
            if let Some(Modal::CommandCenter(cc)) = &mut self.modal {
                cc.loading = false;
            }
            return;
        };
        if let Some(Modal::CommandCenter(cc)) = &mut self.modal {
            (cc.root, cc.loading) = (Some(root), true);
        }
        cx.spawn(async move |this, cx| {
            let (files, capped) = task.await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::CommandCenter(cc)) = &mut w.modal {
                    (cc.files, cc.capped, cc.loading) = (files, capped, false);
                    w.cc_changed(cx);
                }
            });
        })
        .detach();
    }

    /// The input changed (or the file list arrived): new rows; `#` text goes to the Search panel.
    fn cc_changed(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::CommandCenter(cc)) = &mut self.modal else { return };
        let (mode, rest) = quick_open::parse(&cc.edit.text);
        cc.sel = 0;
        cc.run += 1;
        match mode {
            Mode::Files => {
                let (name, _) = quick_open::split_line(rest);
                let (name, files, run) = (name.to_string(), cc.files.clone(), cc.run);
                let recent = cc.root.as_ref().and_then(|r| self.projects.get(r)).map(|p| p.recent.clone()).unwrap_or_default();
                cx.spawn(async move |this, cx| {
                    let hits = cx.background_executor().spawn(async move { quick_open::filter(&name, &files, &recent, LIMIT) }).await;
                    let _ = this.update(cx, |w, cx| {
                        if let Some(Modal::CommandCenter(cc)) = &mut w.modal
                            && cc.run == run
                        {
                            cc.rows = hits.into_iter().map(Row::File).collect();
                            cc.scroll.scroll_to_item(0, ScrollStrategy::Top);
                            crate::perf::event("cc rows ready");
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Mode::Commands => {
                let mut rows: Vec<(i32, Row)> = cc
                    .commands
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (_, label, _))| crate::projects::fuzzy(rest, label).map(|m| (m.score, Row::Command(i, m.chars))))
                    .collect();
                if !rest.is_empty() {
                    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
                }
                cc.rows = rows.into_iter().map(|(_, r)| r).collect();
            }
            Mode::Content => {
                cc.rows.clear();
                let text = rest.to_string();
                if !text.is_empty() {
                    self.reveal_panel_tab(PanelTab::Search, cx);
                }
                self.search_panel.update(cx, |p, cx| p.set_text(&text, cx));
            }
            Mode::Line => {
                if let Some(Modal::CommandCenter(cc)) = &mut self.modal {
                    cc.rows.clear();
                }
            }
        }
        cx.notify();
    }

    pub(super) fn cc_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let ks = &ev.keystroke;
        let Some(Modal::CommandCenter(cc)) = &mut self.modal else { return };
        let n = cc.rows.len();
        match ks.key.as_str() {
            "escape" => return self.close_modal(window, cx),
            "up" | "down" | "pageup" | "pagedown" if n > 0 => {
                cc.sel = match ks.key.as_str() {
                    "up" => (cc.sel + n - 1) % n,
                    "down" => (cc.sel + 1) % n,
                    "pageup" => cc.sel.saturating_sub(10),
                    _ => (cc.sel + 10).min(n - 1),
                };
                cc.scroll.scroll_to_item(cc.sel, ScrollStrategy::Nearest);
                return cx.notify();
            }
            "enter" => return self.cc_run(None, ks.modifiers.control, window, cx),
            "i" if ks.modifiers.alt && quick_open::parse(&cc.edit.text).0 == Mode::Files => {
                cc.ignored = !cc.ignored;
                self.cc_load(cx);
                return cx.notify();
            }
            _ => {}
        }
        let result = if ks.modifiers.control && ks.key == "v" {
            match cx.read_from_clipboard().and_then(|c| c.text()) {
                Some(text) => {
                    cc.edit.insert(text.lines().next().unwrap_or("").trim());
                    Edit::Changed
                }
                None => Edit::Ignored,
            }
        } else {
            cc.edit.key(&ks.key, ks.key_char.as_deref(), ks.modifiers.control)
        };
        match result {
            Edit::Changed => self.cc_changed(cx),
            Edit::Cancel => self.close_modal(window, cx),
            Edit::Submit => self.cc_run(None, ks.modifiers.control, window, cx),
            Edit::Ignored => {}
        }
    }

    /// Enter (or a click on row `ix`): open the file (Ctrl: its diff), run the command, show the search results,
    /// or go to the line.
    fn cc_run(&mut self, ix: Option<usize>, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::CommandCenter(cc)) = self.modal.take() else { return };
        self.close_modal(window, cx);
        let (mode, rest) = quick_open::parse(&cc.edit.text);
        let row = cc.rows.get(ix.unwrap_or(cc.sel));
        match (mode, row) {
            (Mode::Files, Some(Row::File(hit))) => {
                let (Some(root), Some(entry)) = (cc.root.clone(), cc.files.get(hit.ix)) else { return };
                let rel = entry.rel.clone();
                let (_, line) = quick_open::split_line(rest);
                if let Some(p) = self.projects.get_mut(&root) {
                    p.recent.retain(|r| *r != rel);
                    p.recent.insert(0, rel.clone());
                    p.recent.truncate(RECENT);
                    self.mark_dirty(cx);
                }
                if ctrl {
                    match self.repos.status(&root).and_then(|s| s.files.iter().find(|f| f.path == rel).cloned()) {
                        Some(change) => self.toggle_diff(root, change, window, cx),
                        None => self.flash(format!("No changes in {rel}"), cx),
                    }
                } else {
                    self.open_file(root.join(&rel), false, window, cx);
                    if let (Some(line), Some(Overlay::File(f))) = (line, &self.overlay) {
                        f.update(cx, |f, cx| f.go_to_line(line as usize, cx));
                    }
                }
            }
            (Mode::Commands, Some(Row::Command(i, _))) => {
                let action = cc.commands[*i].2.boxed_clone();
                window.dispatch_action(action, cx);
            }
            (Mode::Content, _) => {
                if !rest.is_empty() {
                    self.reveal_panel_tab(PanelTab::Search, cx);
                    self.search_panel.update(cx, |p, cx| p.focus_results(window, cx));
                }
            }
            (Mode::Line, _) => match (rest.trim().parse::<usize>(), &self.overlay) {
                (Ok(line), Some(Overlay::File(f))) => f.update(cx, |f, cx| f.go_to_line(line, cx)),
                (Ok(_), _) => self.flash("Go to line: open a file first".into(), cx),
                _ => {}
            },
            _ => {}
        }
    }

    /// The box: a search icon and the active project's name; a click opens the dropdown.
    pub(super) fn cc_box(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        let project = self.tabs.get(self.active).and_then(Self::tree_root).map(|root| {
            self.projects.get(&root).map(|p| p.display()).unwrap_or_else(|| {
                root.file_name().map_or(root.display().to_string(), |n| n.to_string_lossy().into_owned())
            })
        });
        let open = matches!(self.modal, Some(Modal::CommandCenter(_)));
        let anchor = self.cc_anchor.clone();
        div()
            .id("cc-box")
            .relative()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .w(z(WIDTH * 0.7))
            .h(z(24.))
            .px(z(space::MD))
            .rounded(z(radius::MD))
            .bg(t.bg.input)
            .border_1()
            .border_color(if open { t.accent } else { t.border.subtle })
            .text_size(z(text_size::SM))
            .text_color(t.text.dim)
            .cursor_pointer()
            .hover(|s| s.border_color(t.border.strong))
            .child(canvas(move |b, _, _| anchor.set(Some(b)), |_, _, _, _| {}).absolute().size_full())
            .child(ui::icon(icon::SEARCH, 12., t.text.dim))
            .child(div().flex_1().min_w_0().truncate().child(project.unwrap_or_else(|| "Search".into())))
            .child(div().flex_none().text_size(z(text_size::XS)).text_color(t.text.faint).child("Ctrl+P"))
            // Not a window drag / maximize.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_cc("", window, cx)))
    }

    /// Server-side decorations: the box in a slim bar on top of the main area.
    pub(super) fn cc_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(z(height::TITLE_BAR))
            .bg(t.bg.panel)
            .border_b_1()
            .border_color(t.border.subtle)
            .child(self.cc_box(cx))
    }

    /// The dropdown under the box, over a full-window layer that closes it on a click outside.
    pub(super) fn cc_view(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Modal::CommandCenter(cc)) = &self.modal else { return None };
        let t = theme::get();
        let vp = window.viewport_size();
        let w = z(WIDTH).min(vp.width - z(16.));
        let (left, top) = match self.cc_anchor.get() {
            Some(b) => ((b.center().x - w / 2.).max(z(8.)).min(vp.width - w - z(8.)), b.bottom() + z(4.)),
            None => ((vp.width - w) / 2., z(height::TITLE_BAR + 4.)),
        };
        let (mode, rest) = quick_open::parse(&cc.edit.text);
        let input = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .px(z(space::MD))
            .h(z(34.))
            .border_b_1()
            .border_color(t.border.subtle)
            .child(ui::icon(icon::SEARCH, 13., t.text.dim))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(cc.edit.before_cursor().to_string())
                    .child(div().flex_none().w(z(1.5)).h(z(15.)).bg(t.accent))
                    .child(cc.edit.after_cursor().to_string())
                    .when(cc.edit.text.is_empty(), |d| d.child(div().text_color(t.text.faint).child("Go to file  ·  > command  ·  # search in files  ·  : line"))),
            );
        let note = |s: String| div().px(z(space::MD)).py(z(space::MD)).text_size(z(text_size::SM)).text_color(t.text.dim).child(s);
        let body: AnyElement = match mode {
            Mode::Files | Mode::Commands if !cc.rows.is_empty() => {
                let h = z(ROW) * (cc.rows.len() as f32).min(ROWS);
                uniform_list(
                    "cc-rows",
                    cc.rows.len(),
                    cx.processor(|w: &mut Workspace, range: std::ops::Range<usize>, _, cx| {
                        let Some(Modal::CommandCenter(cc)) = &w.modal else { return Vec::new() };
                        range.map(|i| cc_row(cc, i, cx)).collect()
                    }),
                )
                .track_scroll(&cc.scroll)
                .h(h)
                .into_any_element()
            }
            Mode::Files if cc.loading => note("Indexing files…".into()).into_any_element(),
            Mode::Files if cc.root.is_none() => note("No project: open a session first.".into()).into_any_element(),
            Mode::Files => note("No file matches.".into()).into_any_element(),
            Mode::Commands => note("No command matches.".into()).into_any_element(),
            Mode::Content => note(if rest.is_empty() {
                "Type to search in files; results show in the Search panel. Enter: go to the results.".into()
            } else {
                format!("Searching for \"{rest}\" in the Search panel. Enter: go to the results.")
            })
            .into_any_element(),
            Mode::Line => note(match (&self.overlay, rest.trim().parse::<usize>()) {
                (Some(Overlay::File(_)), Ok(n)) => format!("Enter: go to line {n} in the preview"),
                (Some(Overlay::File(_)), Err(_)) => "Type a line number".into(),
                _ => "Go to line works in an open file preview".into(),
            })
            .into_any_element(),
        };
        let footer = (mode == Mode::Files).then(|| {
            div()
                .flex()
                .items_center()
                .gap(z(space::LG))
                .px(z(space::MD))
                .h(z(26.))
                .border_t_1()
                .border_color(t.border.subtle)
                .text_size(z(text_size::XS))
                .text_color(t.text.faint)
                .child("Enter open · Ctrl+Enter diff · name:42 line")
                .child(div().flex_1())
                .when(cc.capped, |d| d.child(div().text_color(t.state.needs_you).child("Too many files: not all indexed")))
                .child(
                    div()
                        .id("cc-ignored")
                        .flex()
                        .items_center()
                        .gap(z(space::XS))
                        .cursor_pointer()
                        .child(ui::check_box(cc.ignored))
                        .child("Include ignored  Alt+I")
                        .on_click(cx.listener(|w, _: &ClickEvent, _, cx| {
                            if let Some(Modal::CommandCenter(cc)) = &mut w.modal {
                                cc.ignored = !cc.ignored;
                                w.cc_load(cx);
                                cx.notify();
                            }
                        })),
                )
        });
        let card = div()
            .occlude()
            .w(w)
            .flex()
            .flex_col()
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .text_color(t.text.primary)
            .text_size(z(text_size::MD))
            .child(input)
            .child(body)
            .children(footer);
        Some(
            div()
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|w, _, window, cx| w.close_modal(window, cx)))
                .child(div().absolute().left(left).top(top).child(card))
                .into_any_element(),
        )
    }
}

/// Matched byte offsets → highlight ranges (whole chars) within `text`, starting at byte `from`.
fn ranges(text: &str, hl: &[usize], from: usize, style: HighlightStyle) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    hl.iter()
        .filter(|&&b| b >= from && b < from + text.len() && text.is_char_boundary(b - from))
        .map(|&b| {
            let s = b - from;
            let len = text[s..].chars().next().map_or(1, char::len_utf8);
            (s..s + len, style)
        })
        .collect()
}

fn cc_row(cc: &Cc, i: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let t = theme::get();
    let style = HighlightStyle { color: Some(t.accent.into()), font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() };
    let row = div()
        .id(("cc", i))
        .w_full()
        .h(z(ROW))
        .flex()
        .items_center()
        .gap(z(space::SM))
        .px(z(space::MD))
        .whitespace_nowrap()
        .overflow_hidden()
        .cursor_pointer()
        .when(i == cc.sel, |d| d.bg(t.bg.selected))
        .hover(|s| s.bg(t.bg.hover))
        .on_click(cx.listener(move |w, e: &ClickEvent, window, cx| w.cc_run(Some(i), e.modifiers().control, window, cx)));
    match &cc.rows[i] {
        Row::File(hit) => {
            let Some(e) = cc.files.get(hit.ix) else { return row.into_any_element() };
            let (dir, name) = (&e.rel[..e.name], &e.rel[e.name..]);
            row.child(ui::icon(icon::FILE, 13., t.text.dim))
                .child(div().flex_none().child(StyledText::new(SharedString::from(name.to_string())).with_highlights(ranges(name, &hit.hl, e.name, style))))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(z(text_size::XS))
                        .text_color(t.text.dim)
                        .child(StyledText::new(SharedString::from(dir.trim_end_matches('/').to_string())).with_highlights(ranges(dir.trim_end_matches('/'), &hit.hl, 0, style))),
                )
                .into_any_element()
        }
        Row::Command(c, hl) => {
            let (keys, label, _) = &cc.commands[*c];
            // `fuzzy` gives char indices: the labels are ASCII.
            row.child(div().flex_1().min_w_0().truncate().child(StyledText::new(SharedString::from(*label)).with_highlights(ranges(label, hl, 0, style))))
                .child(div().flex_none().font_family(MONO_FONT).text_size(z(text_size::XS)).text_color(t.text.dim).child(crate::keys::pretty(keys)))
                .into_any_element()
        }
    }
}
