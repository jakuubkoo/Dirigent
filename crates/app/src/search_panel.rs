//! The "Search" tab of the right panel: text search in the active project's files (plain / regex, case, whole
//! word). Results stream in per file from worker threads (`git::search`); a new query cancels the running one.
//! Handlers that call into the workspace are plain closures with a weak handle (the workspace updates this view).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use git::search::{FileHits, Query};
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, Pixels, ScrollStrategy, SharedString,
    StyledText, Task, UniformListScrollHandle, WeakEntity, Window, div, prelude::*, uniform_list,
};

use crate::line_edit::{Edit, LineEdit};
use crate::scrollbar::{self, scrollbar};
use crate::theme::{self, MONO_FONT, height, space, text_size, z};
use crate::ui::{self, icon};
use crate::workspace::Workspace;

/// Typing pause before a search starts.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct SearchPanel {
    pub query: Query,
    edit: LineEdit,
    root: Option<PathBuf>,
    results: Vec<FileHits>,
    /// Flattened list: (file, None) = its header, (file, Some(hit)).
    rows: Vec<(usize, Option<usize>)>,
    hits: usize,
    running: bool,
    capped: bool,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
    sel: usize,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: Pixels,
    font_size: Pixels,
    _task: Option<Task<()>>,
}

impl SearchPanel {
    pub fn new(workspace: WeakEntity<Workspace>, m: &crate::terminal_element::Metrics, cx: &mut Context<Self>) -> Self {
        Self {
            query: Query::default(),
            edit: LineEdit::new(""),
            root: None,
            results: Vec::new(),
            rows: Vec::new(),
            hits: 0,
            running: false,
            capped: false,
            error: None,
            cancel: Default::default(),
            sel: 0,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height: m.line_height,
            font_size: m.font_size - z(1.),
            _task: None,
        }
    }

    pub fn set_metrics(&mut self, m: &crate::terminal_element::Metrics, cx: &mut Context<Self>) {
        (self.line_height, self.font_size) = (m.line_height, m.font_size - z(1.));
        cx.notify();
    }

    /// The Command Center's `#` text: replaces the query and searches again.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.edit.text != text {
            self.edit = LineEdit::new(text);
            self.restart(cx);
        }
    }

    /// Cancels the running search and starts the current query after a short pause (typing restarts the pause).
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.query.text = self.edit.text.clone();
        (self.results, self.rows, self.hits, self.sel) = (Vec::new(), Vec::new(), 0, 0);
        (self.capped, self.error) = (false, None);
        self.running = !self.query.text.is_empty();
        cx.notify();
        if !self.running {
            self._task = None;
            return;
        }
        let re = match self.query.compile() {
            Ok(re) => re,
            Err(e) => {
                (self.error, self.running) = (Some(e), false);
                return;
            }
        };
        let (ws, cancel) = (self.workspace.clone(), self.cancel.clone());
        self._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let Ok(Some((root, files))) = ws.update(cx, |w, cx| w.project_files(false, cx)) else {
                let _ = this.update(cx, |p, cx| {
                    (p.running, p.error) = (false, Some("No project: open a session first".into()));
                    cx.notify();
                });
                return;
            };
            let (files, _) = files.await;
            let _ = this.update(cx, |p, _| p.root = Some(root.clone()));
            let (tx, rx) = async_channel::unbounded::<FileHits>();
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(1)).clamp(1, 8);
            let c = cancel.clone();
            let done = std::thread::Builder::new().name("search".into()).spawn(move || {
                git::search::search(&root, &files, &re, threads, &c, &|f| {
                    let _ = tx.send_blocking(f);
                })
            });
            // Everything that arrived since the last frame goes in at once: one render per batch.
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |p, cx| p.add(batch, cx)).is_err() {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
            }
            let capped = done.ok().and_then(|h| h.join().ok()).unwrap_or(false);
            let _ = this.update(cx, |p, cx| {
                (p.running, p.capped) = (false, capped);
                cx.notify();
            });
        }));
    }

    fn add(&mut self, batch: Vec<FileHits>, cx: &mut Context<Self>) {
        for f in batch {
            let i = self.results.len();
            self.hits += f.hits.len();
            self.rows.push((i, None));
            self.rows.extend((0..f.hits.len()).map(|h| (i, Some(h))));
            self.results.push(f);
        }
        cx.notify();
    }

    fn toggle(&mut self, which: char, cx: &mut Context<Self>) {
        match which {
            'r' => self.query.regex = !self.query.regex,
            'c' => self.query.case = !self.query.case,
            _ => self.query.word = !self.query.word,
        }
        self.restart(cx);
    }

    /// Opens row `ix` (a header opens the file, a hit the file at its line with the match marked).
    fn open(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(&(f, hit))) = (&self.root, self.rows.get(ix)) else { return };
        let file = &self.results[f];
        let path = root.join(&file.rel);
        let target = hit.map(|h| {
            let h = &file.hits[h];
            (h.line, h.text[h.range.clone()].to_string())
        });
        let ws = self.workspace.clone();
        // Deferred: the workspace updates this panel while opening.
        window.defer(cx, move |window, cx| {
            let _ = ws.update(cx, |w, cx| w.open_search_hit(path, target, window, cx));
        });
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "up" | "down" if !self.rows.is_empty() => {
                let last = self.rows.len() - 1;
                self.sel = if ks.key == "up" { self.sel.saturating_sub(1) } else { (self.sel + 1).min(last) };
                self.scroll.scroll_to_item(self.sel, ScrollStrategy::Nearest);
                cx.notify();
            }
            "enter" => self.open(self.sel, window, cx),
            "escape" => {
                let ws = self.workspace.clone();
                window.defer(cx, move |window, cx| {
                    let _ = ws.update(cx, |w, cx| w.focus_main(window, cx));
                });
            }
            "r" | "c" | "w" if m.alt && !m.control => self.toggle(ks.key.chars().next().unwrap_or('w'), cx),
            _ => match self.edit.key(ks, cx) {
                Edit::Changed => self.restart(cx),
                Edit::Moved => cx.notify(),
                Edit::Submit => self.open(self.sel, window, cx),
                Edit::Ignored | Edit::Cancel => return,
            },
        }
        cx.stop_propagation();
    }

    pub fn focus_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let focused = self.focus.is_focused(window);
        let toggle = |id, label, on, hint, which: char, cx: &mut Context<Self>| {
            ui::toggle(id, label, on, hint).on_click(cx.listener(move |p, _: &ClickEvent, _, cx| p.toggle(which, cx)))
        };
        let status = match (&self.error, self.running, self.hits) {
            (Some(e), ..) => e.clone(),
            (None, _, _) if self.query.text.is_empty() => "Type to search the project's files (Ctrl+Shift+F)".into(),
            (None, true, 0) => "Searching…".into(),
            (None, running, n) => format!(
                "{n} result{} in {} file{}{}{}",
                if n == 1 { "" } else { "s" },
                self.results.len(),
                if self.results.len() == 1 { "" } else { "s" },
                if self.capped { " (more not shown)" } else { "" },
                if running { " …" } else { "" }
            ),
        };
        let header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(z(space::XS))
            .p(z(space::MD))
            .border_b_1()
            .border_color(t.border.subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .child(div().flex_1().min_w_0().child(ui::text_input(&self.edit, focused, "Search in files")))
                    .child(toggle("s-regex", ".*", self.query.regex, "Regular expression  Alt+R", 'r', cx))
                    .child(toggle("s-case", "Aa", self.query.case, "Match case  Alt+C", 'c', cx))
                    .child(toggle("s-word", "ab", self.query.word, "Whole word  Alt+W", 'w', cx)),
            )
            .child(
                div()
                    .text_size(z(text_size::XS))
                    .text_color(if self.error.is_some() { t.state.error } else { t.text.dim })
                    .child(status),
            );
        let (lh, sel) = (self.line_height.max(z(height::LIST_ROW)), self.sel);
        let list = uniform_list(
            "search-rows",
            self.rows.len(),
            cx.processor(move |p: &mut SearchPanel, range: std::ops::Range<usize>, _, cx| {
                let t = theme::get();
                range
                    .map(|ix| {
                        let (f, hit) = p.rows[ix];
                        let file = &p.results[f];
                        let row = div()
                            .id(ix)
                            .w_full()
                            .h(lh)
                            .flex()
                            .items_center()
                            .gap(z(space::SM))
                            .px(z(space::MD))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .cursor_pointer()
                            .when(ix == sel, |d| d.bg(t.bg.selected))
                            .hover(|s| s.bg(t.bg.hover))
                            .on_click(cx.listener(move |p, _: &ClickEvent, window, cx| {
                                p.sel = ix;
                                p.open(ix, window, cx);
                            }));
                        match hit {
                            None => {
                                let (dir, name) =
                                    file.rel.rsplit_once('/').map_or(("", file.rel.as_str()), |(d, n)| (d, n));
                                row.child(ui::icon(icon::FILE, 12., t.text.dim))
                                    .child(div().flex_none().text_color(t.text.primary).child(name.to_string()))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(z(text_size::XS))
                                            .text_color(t.text.dim)
                                            .child(dir.to_string()),
                                    )
                                    .child(
                                        div()
                                            .ml_auto()
                                            .flex_none()
                                            .text_size(z(text_size::XS))
                                            .text_color(t.text.dim)
                                            .child(file.hits.len().to_string()),
                                    )
                                    .into_any_element()
                            }
                            Some(h) => {
                                let h = &file.hits[h];
                                let hl = HighlightStyle {
                                    background_color: Some(t.search_match.into()),
                                    color: Some(t.text.primary.into()),
                                    ..Default::default()
                                };
                                row.pl(z(space::XL))
                                    .font_family(MONO_FONT)
                                    .child(div().flex_none().text_color(t.text.faint).child(h.line.to_string()))
                                    .child(
                                        div().min_w_0().text_color(t.text.muted).child(
                                            StyledText::new(SharedString::from(h.text.trim_start().to_string()))
                                                .with_highlights(trimmed(&h.text, h.range.clone()).map(|r| (r, hl))),
                                        ),
                                    )
                                    .into_any_element()
                            }
                        }
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .text_size(self.font_size);
        div()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .text_size(z(text_size::SM))
            .child(header)
            .child(div().relative().flex_1().min_h_0().child(list.size_full()).child(scrollbar(
                self.scroll.clone(),
                self.drag.clone(),
                cx.entity_id(),
            )))
    }
}

/// The match range after the line's leading whitespace is trimmed for display.
fn trimmed(text: &str, r: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    let cut = text.len() - text.trim_start().len();
    (r.start >= cut).then(|| r.start - cut..r.end - cut)
}
