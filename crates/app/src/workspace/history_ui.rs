//! History: a project's past Claude Code sessions (the model is `history.rs`). Enter resumes one in a new tab in
//! the directory it was started in, or switches to the tab that already has it.

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, FontWeight, HighlightStyle, MouseMoveEvent, SharedString, StyledText,
    UniformListScrollHandle, div, uniform_list,
};

use super::view::short_path;
use super::*;
use crate::history::Entry;
use crate::projects;
use crate::scrollbar;
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};
use crate::ui::{self, icon};

const PAGE: usize = 10;

/// A transcript written this recently with no tab of ours holding it may belong to a claude running elsewhere (another
/// terminal): resuming asks first, so two processes never write one transcript by accident.
const RECENT_SECS: i64 = 120;

pub(super) struct History {
    root: PathBuf,
    pub edit: LineEdit,
    /// Where its worktrees go (sessions there count as the project's).
    worktrees: PathBuf,
    /// None while the transcripts are read.
    all: Option<Arc<Vec<Entry>>>,
    /// Claude processes outside this app, read with the list (Linux; empty elsewhere). Not refreshed while open:
    /// nothing polls.
    running: crate::history::Running,
    rows: Vec<Row>,
    sel: usize,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
}

struct Row {
    /// Index into `all`.
    ix: usize,
    /// Matched chars of the title line, or of the prompt line when only that matched.
    hl: Vec<usize>,
    in_prompt: bool,
}

/// What the row's first line shows: the title, else the first prompt.
fn label(e: &Entry) -> &str {
    e.title.as_deref().or(e.prompt.as_deref()).unwrap_or_default()
}

/// The second line's prompt excerpt: only when the title is something else.
fn excerpt(e: &Entry) -> Option<&str> {
    e.title.as_ref().and(e.prompt.as_deref())
}

impl Workspace {
    /// The project menu's History… / the Command Center's "Claude: Session history…": lists at once with a note, the
    /// transcripts are read off the UI thread (cached per file version for the app's lifetime).
    pub(super) fn open_history(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let wt_dir = self.projects.get(&root).and_then(|p| p.worktrees_dir.clone());
        let worktrees = projects::worktrees_base(&root, wt_dir.as_deref());
        self.modal = Some(Modal::History(Box::new(History {
            root: root.clone(),
            worktrees: worktrees.clone(),
            edit: LineEdit::new(""),
            all: None,
            running: Default::default(),
            rows: Vec::new(),
            sel: 0,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
        })));
        let cache = self.history.clone();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let (all, running) = cx
                .background_executor()
                .spawn(async move {
                    let projects = home().join(".claude/projects");
                    let all = crate::history::list(&projects, &r, &worktrees, &cache);
                    (all, crate::history::running(&agent::runtime_dir()))
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                if let Some(Modal::History(h)) = &mut w.modal
                    && h.root == root
                {
                    h.all = Some(Arc::new(all));
                    h.running = running;
                    w.history_filter();
                    cx.notify();
                }
            });
        })
        .detach();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The Command Center's "Claude: Session history…": the active session's project (its sidebar group); with no
    /// session open, the project picker first.
    pub(super) fn history_of_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(self.active) else { return self.open_picker(true, window, cx) };
        let root = self.tab_group(self.active).unwrap_or_else(|| t.cwd.clone());
        self.open_history(root, window, cx);
    }

    /// Rows for the query: newest first when empty, else by match score (title over prompt), ties newest first.
    fn history_filter(&mut self) {
        let Some(Modal::History(h)) = &mut self.modal else { return };
        let Some(all) = &h.all else { return };
        let q = h.edit.text.trim();
        let mut scored: Vec<(i32, Row)> = all
            .iter()
            .enumerate()
            .filter_map(|(ix, e)| {
                if let Some(m) = projects::fuzzy(q, label(e)) {
                    return Some((m.score + 10, Row { ix, hl: m.chars, in_prompt: false }));
                }
                let m = projects::fuzzy(q, excerpt(e)?)?;
                Some((m.score, Row { ix, hl: m.chars, in_prompt: true }))
            })
            .collect();
        if !q.is_empty() {
            scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s)); // stable: newest first breaks ties
        }
        h.rows = scored.into_iter().map(|(_, r)| r).collect();
        h.sel = 0;
        h.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
    }

    pub(super) fn history_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::History(h)) = &mut self.modal else { return };
        let n = h.rows.len();
        let moved = |h: &mut History, to: usize| {
            h.sel = to.min(n.saturating_sub(1));
            h.scroll.scroll_to_item(h.sel, gpui::ScrollStrategy::Center);
        };
        match ev.keystroke.key.as_str() {
            "escape" => return self.close_modal(window, cx),
            "up" => moved(h, h.sel.saturating_sub(1)),
            "down" => moved(h, h.sel + 1),
            "pageup" => moved(h, h.sel.saturating_sub(PAGE)),
            "pagedown" => moved(h, h.sel + PAGE),
            "enter" => return self.history_open(None, window, cx),
            _ => {
                if h.edit.key(&ev.keystroke, cx) == Edit::Changed {
                    self.history_filter();
                }
            }
        }
        cx.notify();
    }

    /// Enter / a click: the tab that has this session (resumed there if its claude isn't running), else a new tab
    /// typing `claude --resume <id> <project flags>` in the directory it was started in (the project if that's gone;
    /// claude finds the transcript from anywhere, verified on 2.1.319).
    pub(super) fn history_open(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::History(h)) = &self.modal else { return };
        let Some(e) = h.rows.get(ix.unwrap_or(h.sel)).and_then(|r| h.all.as_ref().map(|a| a[r.ix].clone())) else {
            return;
        };
        let root = h.root.clone();
        let (exact, recent) = (h.running.sessions.contains(&e.id), now_secs() - e.modified < RECENT_SECS);
        let in_project = h.running.in_project(&h.root, &h.worktrees);
        self.modal = None;
        let dir = if e.cwd_exists { e.cwd.clone() } else { root.clone() };
        if let Some(i) = self.held_by(&e.id) {
            if self.agent_running(i, cx) {
                self.activate(i, window, cx);
            } else {
                self.resume_one(i, window, cx);
            }
        } else if exact || recent {
            let why = if exact {
                "A claude outside Dirigent runs this session (its command line names it)."
            } else {
                "Its transcript changed in the last 2 minutes and no session here has it: claude may still run it in \
                 another terminal."
            };
            // The project-level hint never asks on its own; here it adds a line.
            let also = if in_project && !exact { " A claude outside Dirigent runs in this project." } else { "" };
            self.modal = Some(Modal::Confirm {
                title: "Resume a session that may be running?".into(),
                body: format!("{why}{also} Two claude processes on one session both write its transcript."),
                files: Vec::new(),
                ok: "Resume anyway",
                danger: true,
                then: Then::Resume { dir, id: e.id, project: root },
            });
            window.focus(&self.focus, cx);
        } else {
            self.resume_new(dir, e.id, root, window, cx);
        }
        cx.notify();
    }

    /// A new tab typing `claude --resume <id>` in `dir` with `project`'s flags.
    pub(super) fn resume_new(
        &mut self,
        dir: PathBuf,
        id: String,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opts = SessionOpts { resume: Some(id), project: Some(project), ..Default::default() };
        self.new_session_with(dir, opts, window, cx);
    }

    /// The tab whose claude session this is.
    fn held_by(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.claude_session.as_deref() == Some(id))
    }

    pub(super) fn history_view(&self, h: &History, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let height = (window.viewport_size().height - z(160.)).min(z(620.)).max(z(240.));
        let search = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .px(z(space::MD))
            .py(z(space::SM))
            .rounded(z(radius::MD))
            .bg(t.bg.input)
            .border_1()
            .border_color(t.accent)
            .text_size(z(text_size::MD))
            .child(ui::icon(icon::SEARCH, 14., t.text.dim))
            .child(ui::line_text(&h.edit, true, "Search titles and first prompts"));
        let n = h.rows.len();
        let list = uniform_list(
            "history-rows",
            n,
            cx.processor(|w: &mut Workspace, range: std::ops::Range<usize>, _window, cx| {
                let Some(Modal::History(h)) = &w.modal else { return Vec::new() };
                let now = now_secs();
                range.map(|i| w.history_row(h, i, now, cx)).collect()
            }),
        )
        .track_scroll(&h.scroll)
        .size_full();
        let note = match &h.all {
            None => Some("Reading Claude's transcripts…"),
            Some(a) if a.is_empty() => Some("No Claude Code sessions were started in this project yet."),
            Some(_) if n == 0 => Some("No session matches."),
            _ => None,
        };
        let count = h.all.as_ref().map(|a| {
            let of = if n == a.len() { String::new() } else { format!(" of {}", a.len()) };
            format!("{n}{of} session{}", if a.len() == 1 { "" } else { "s" })
        });
        div()
            .w(z(760.))
            .h(height)
            .p(z(space::LG))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(z(space::SM))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("History of {}", short_path(&h.root))))
                    .children(count.map(|c| div().text_size(z(text_size::XS)).text_color(t.text.dim).child(c)))
                    .when(h.running.in_project(&h.root, &h.worktrees), |d| {
                        d.child(
                            div()
                                .id("history-busy")
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap(z(space::XS))
                                .text_size(z(text_size::XS))
                                .text_color(t.text.muted)
                                .child(ui::icon(icon::TERMINAL, 12., t.state.needs_you))
                                .child("claude running in this project")
                                .tooltip(ui::tip(
                                    "A claude started outside Dirigent works here (it names no session, so none is marked).",
                                )),
                        )
                    }),
            )
            .child(search)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(scrollbar::scrollbar(h.scroll.clone(), h.drag.clone(), cx.entity_id()))
                    .children(note.map(|s| {
                        div()
                            .absolute()
                            .top_0()
                            .p(z(space::LG))
                            .text_size(z(text_size::SM))
                            .text_color(t.text.dim)
                            .child(s)
                    })),
            )
            .child(
                div()
                    .text_size(z(text_size::XS))
                    .text_color(t.text.dim)
                    .child("↑↓ select · Enter resume in a new session (or switch to it) · Esc"),
            )
            .into_any_element()
    }

    fn history_row(&self, h: &History, i: usize, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let row = &h.rows[i];
        let Some(e) = h.all.as_ref().map(|a| &a[row.ix]) else { return div().into_any_element() };
        let open = self.held_by(&e.id).is_some();
        let elsewhere = !open && (h.running.sessions.contains(&e.id) || now - e.modified < RECENT_SECS);
        let styled = |text: &str, hl: &[usize]| {
            let ranges: Vec<_> = hl
                .iter()
                .filter_map(|&c| text.char_indices().nth(c).map(|(b, ch)| b..b + ch.len_utf8()))
                .map(|r| {
                    let style = HighlightStyle {
                        color: Some(t.accent.into()),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    };
                    (r, style)
                })
                .collect();
            StyledText::new(SharedString::from(text.to_string())).with_highlights(ranges)
        };
        let none: &[usize] = &[];
        let (hl_title, hl_prompt) = if row.in_prompt { (none, &row.hl[..]) } else { (&row.hl[..], none) };
        let meta = div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .text_size(z(text_size::XS))
            .text_color(t.text.dim)
            .children(excerpt(e).map(|p| div().flex_1().min_w_0().truncate().child(styled(p, hl_prompt))))
            .when(excerpt(e).is_none(), |d| d.child(div().flex_1()))
            .children(e.branch.clone().map(|b| {
                div()
                    .flex_none()
                    .max_w(z(200.))
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .text_color(t.text.muted)
                    .child(ui::icon(icon::BRANCH, 11., t.text.dim))
                    .child(div().truncate().font_family(MONO_FONT).child(b))
            }))
            .child(div().flex_none().child(format!("{} messages", e.messages)))
            .child(div().flex_none().w(z(40.)).text_right().child(ui::fmt_ago(now - e.modified)));
        div()
            .id(("history", i))
            .w_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap(z(2.))
            .h(z(48.))
            .px(z(space::MD))
            .rounded(z(radius::MD))
            .cursor_pointer()
            .when(i == h.sel, |d| d.bg(t.bg.selected_focus))
            .on_mouse_move(cx.listener(move |w, _: &MouseMoveEvent, _, cx| {
                if let Some(Modal::History(h)) = &mut w.modal
                    && h.sel != i
                {
                    h.sel = i;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| w.history_open(Some(i), window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(z(text_size::MD))
                            .text_color(t.text.primary)
                            .child(styled(label(e), hl_title)),
                    )
                    .when(open, |d| d.child(ui::pill("open")))
                    .when(elsewhere, |d| {
                        d.child(div().id(("elsewhere", i)).child(ui::pill("maybe running elsewhere")).tooltip(ui::tip(
                            "A claude outside Dirigent names it, or it was written in the last 2 minutes; \
                                     no session here has it",
                        )))
                    }),
            )
            .child(meta)
            .into_any_element()
    }
}
