//! Root view: session list, sidebar, the active terminal (cached view), modal, resize policy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    ClickEvent, Context, DragMoveEvent, Entity, EntityId, FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Render,
    SharedString, StyleRefinement, Task, Window, actions, div, prelude::*, px, rgb,
};
use session::{Session, Size};

use crate::line_edit::{Edit, LineEdit};
use crate::terminal_element::{Metrics, palette};
use crate::terminal_view::{PADDING, TerminalView};

actions!(
    agent_ide,
    [
        NewSession, CloseSession, NextSession, PrevSession, Rename, ToggleSidebar,
        Activate1, Activate2, Activate3, Activate4, Activate5, Activate6, Activate7, Activate8, Activate9
    ]
);

const DIVIDER: f32 = 4.;
const COLLAPSED: f32 = 10.;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);

mod colors {
    pub const SIDEBAR: u32 = 0x17171c;
    pub const ROW_ACTIVE: u32 = 0x2b2b35;
    pub const ROW_HOVER: u32 = 0x212129;
    pub const DIM: u32 = 0x8a8a96;
    pub const DOT: u32 = 0x5fa2e6;
    pub const EXITED: u32 = 0xe0626c;
    pub const DIVIDER: u32 = 0x26262e;
    pub const DIVIDER_HOVER: u32 = 0x4a4a58;
    pub const MODAL: u32 = 0x24242c;
    pub const INPUT: u32 = 0x1a1a20;
}

struct Tab {
    id: usize,
    name: String,
    view: Entity<TerminalView>,
}

enum Modal {
    NewSession { edit: LineEdit, error: Option<String> },
    Rename { tab: usize, edit: LineEdit },
}

#[derive(Clone)]
struct SidebarDrag;

struct DragGhost;
impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    metrics: Metrics,
    focus: FocusHandle,
    sidebar_width: Pixels,
    sidebar_open: bool,
    dragging: bool,
    flush_resize: bool,
    applied: Size,
    pending: Option<Size>,
    _resize_task: Option<Task<()>>,
    last_dir: PathBuf,
    modal: Option<Modal>,
    _cycle: Option<Task<()>>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let metrics = Metrics::new(window);
        let last_dir = std::env::current_dir().unwrap_or_else(|_| home());
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            focus: cx.focus_handle(),
            sidebar_width: px(220.),
            sidebar_open: true,
            dragging: false,
            flush_resize: false,
            applied: Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
            pending: None,
            _resize_task: None,
            last_dir,
            modal: None,
            _cycle: None,
            metrics,
        };
        this.applied = this.grid_size(window);

        // Perf harness: one session per line of AGENT_IDE_RUN, the line typed into it.
        let script = std::env::var("AGENT_IDE_RUN").ok();
        for line in script.as_deref().unwrap_or("").split('\n') {
            let dir = this.last_dir.clone();
            this.spawn_tab(&dir, window, cx);
            if !line.is_empty() {
                this.tabs.last().unwrap().view.read(cx).session.write(format!("{line}\r").as_bytes());
            }
        }
        this.activate(0, window, cx);

        if let Some(n) = std::env::var("AGENT_IDE_PERF_CYCLE").ok().and_then(|n| n.parse::<usize>().ok()) {
            this._cycle = Some(cx.spawn_in(window, async move |this, cx| {
                for _ in 0..n {
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                    if this.update_in(cx, |w, window, cx| w.cycle(1, window, cx)).is_err() {
                        return;
                    }
                }
            }));
        }
        this
    }

    /// Terminal grid size for the current window and sidebar width.
    fn grid_size(&self, window: &Window) -> Size {
        let vp = window.viewport_size();
        let side = if self.sidebar_open { self.sidebar_width + px(DIVIDER) } else { px(COLLAPSED) };
        let m = &self.metrics;
        let w = vp.width - side - px(PADDING);
        let h = vp.height - px(PADDING);
        Size {
            cols: (w / m.cell_width).floor().max(2.) as u16,
            rows: (h / m.line_height).floor().max(1.) as u16,
            cell_width: f32::from(m.cell_width) as u16,
            cell_height: f32::from(m.line_height) as u16,
        }
    }

    /// Resizes every session (visible or not), so switching never triggers a resize + TUI redraw.
    fn apply_size(&mut self, size: Size, cx: &mut Context<Self>) {
        self.applied = size;
        self.pending = None;
        self._resize_task = None;
        let mut n = 0;
        for tab in &self.tabs {
            n += tab.view.read(cx).session.resize(size) as u64;
        }
        crate::perf::pty_resizes(n);
        if let Some(tab) = self.tabs.get(self.active) {
            tab.view.update(cx, |_, cx| cx.notify());
        }
    }

    /// Live layout, deferred PTY resize: immediately after a divider drag ends, 100 ms after the last
    /// window size change otherwise, never during a drag.
    fn schedule_resize(&mut self, window: &Window, cx: &mut Context<Self>) {
        let size = self.grid_size(window);
        if self.dragging || (size == self.applied && self.pending.is_none()) {
            return;
        }
        if self.flush_resize || size == self.applied {
            self.flush_resize = false;
            self.apply_size(size, cx);
        } else if self.pending != Some(size) {
            self.pending = Some(size);
            self._resize_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_DEBOUNCE).await;
                let _ = this.update(cx, |w, cx| w.apply_size(size, cx));
            }));
        }
    }

    fn spawn_tab(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id += 1;
        let view = self.new_view(dir, window, cx);
        self.tabs.push(Tab { id, name: format!("Session {id}"), view });
    }

    fn new_view(&mut self, dir: &Path, _: &mut Window, cx: &mut Context<Self>) -> Entity<TerminalView> {
        let session = Session::spawn(dir, self.applied).expect("spawn shell");
        let (metrics, ws) = (self.metrics.clone(), cx.entity().downgrade());
        cx.new(|cx| TerminalView::new(session, metrics, ws, cx))
    }

    fn activate(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(new) = self.tabs.get(idx) else { return };
        crate::perf::switch_started();
        if let Some(old) = self.tabs.get(self.active) {
            old.view.read(cx).session.set_visible(false);
        }
        new.view.read(cx).session.set_visible(true);
        // It may have output since its cached frame was recorded.
        new.view.update(cx, |_, cx| cx.notify());
        if self.modal.is_none() {
            window.focus(&new.view.read(cx).focus_handle(cx), cx);
        }
        self.active = idx;
        cx.notify();
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        if n > 0 {
            self.activate((self.active as isize + step).rem_euclid(n) as usize, window, cx);
        }
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        // Dropping the view drops the Session, which kills + reaps the shell on its own thread.
        self.tabs.remove(self.active);
        self.modal = None;
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            let idx = self.active.min(self.tabs.len() - 1);
            self.active = usize::MAX; // old one is gone; nothing to hide
            self.activate(idx, window, cx);
        }
    }

    pub fn restart(&mut self, view_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.tabs.iter().position(|t| t.view.entity_id() == view_id) else { return };
        let dir = self.tabs[i].view.read(cx).session.cwd.clone();
        self.tabs[i].view = self.new_view(&dir, window, cx);
        if i == self.active {
            self.active = usize::MAX;
            self.activate(i, window, cx);
        }
        cx.notify();
    }

    fn open_new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = Some(Modal::NewSession { edit: LineEdit::new(tilde(&self.last_dir)), error: None });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn start_rename(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(idx) else { return };
        self.modal = Some(Modal::Rename { tab: idx, edit: LineEdit::new(tab.name.clone()) });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        if let Some(tab) = self.tabs.get(self.active) {
            window.focus(&tab.view.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn modal_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.as_mut() else { return };
        let ks = &ev.keystroke;
        let edit = match modal {
            Modal::NewSession { edit, .. } | Modal::Rename { edit, .. } => edit,
        };
        let result = if ks.modifiers.control && ks.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                edit.insert(text.trim());
            }
            Edit::Changed
        } else {
            edit.key(&ks.key, ks.key_char.as_deref(), ks.modifiers.control || ks.modifiers.platform)
        };
        match result {
            Edit::Ignored => return,
            Edit::Changed => {}
            Edit::Cancel => self.close_modal(window, cx),
            Edit::Submit => match self.modal.take() {
                Some(Modal::Rename { tab, edit }) => {
                    let name = edit.text.trim();
                    if let Some(t) = self.tabs.get_mut(tab)
                        && !name.is_empty()
                    {
                        t.name = name.to_string();
                    }
                    self.close_modal(window, cx);
                }
                Some(Modal::NewSession { edit, .. }) => {
                    let dir = untilde(edit.text.trim());
                    if dir.is_dir() {
                        self.last_dir = dir.clone();
                        self.modal = None;
                        self.spawn_tab(&dir, window, cx);
                        self.activate(self.tabs.len() - 1, window, cx);
                    } else {
                        let error = Some(format!("Not a directory: {}", dir.display()));
                        self.modal = Some(Modal::NewSession { edit, error });
                    }
                }
                None => {}
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.tabs.iter().enumerate().map(|(i, tab)| {
            let session = &tab.view.read(cx).session;
            let (unread, exited) = (session.is_unread(), session.is_exited());
            let cwd = tilde(&session.cwd);
            let active = i == self.active;
            let name: SharedString = match &self.modal {
                Some(Modal::Rename { tab: r, edit }) if *r == i => {
                    return div().id(("tab", tab.id)).px_2().py_1().child(text_field(edit)).into_any_element();
                }
                _ => tab.name.clone().into(),
            };
            div()
                .id(("tab", tab.id))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .when(active, |d| d.bg(rgb(colors::ROW_ACTIVE)))
                .when(!active, |d| d.hover(|s| s.bg(rgb(colors::ROW_HOVER))))
                .on_click(cx.listener(move |w, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 {
                        w.start_rename(i, window, cx);
                    } else {
                        w.activate(i, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_hidden()
                        .child(div().text_sm().truncate().child(name))
                        .child(div().text_xs().truncate().text_color(rgb(colors::DIM)).child(cwd)),
                )
                .when(exited, |d| {
                    d.child(div().text_xs().text_color(rgb(colors::EXITED)).child("exited")).child(
                        div()
                            .id(("restart", tab.id))
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(colors::DIVIDER_HOVER)))
                            .child("⟳")
                            .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| {
                                let id = w.tabs[i].view.entity_id();
                                w.restart(id, window, cx);
                                cx.stop_propagation();
                            })),
                    )
                })
                .when(unread && !active && !exited, |d| d.child(div().size(px(7.)).rounded_full().bg(rgb(colors::DOT))))
                .into_any_element()
        });
        let rows: Vec<_> = rows.collect();

        let header = div()
            .flex()
            .items_center()
            .px_3()
            .py_2()
            .child(div().flex_1().text_xs().text_color(rgb(colors::DIM)).child("Sessions"))
            .child(icon_button("new", "+").on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_new_session(window, cx))))
            .child(icon_button("collapse", "‹").on_click(cx.listener(|w, _: &ClickEvent, _, cx| w.toggle_sidebar(cx))));

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(self.sidebar_width)
            .h_full()
            .bg(rgb(colors::SIDEBAR))
            .child(header)
            .child(div().id("tabs").flex_1().overflow_y_scroll().children(rows))
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        self.flush_resize = true;
        cx.notify();
    }

    fn modal_view(&self) -> Option<impl IntoElement> {
        let Some(Modal::NewSession { edit, error }) = &self.modal else { return None };
        Some(
            div().absolute().size_full().flex().justify_center().pt(px(120.)).bg(gpui::rgba(0x00000066)).child(
                div()
                    .w(px(520.))
                    .h_auto()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_md()
                    .bg(rgb(colors::MODAL))
                    .child(div().text_sm().child("New session"))
                    .child(div().text_xs().text_color(rgb(colors::DIM)).child("Working directory the shell starts in."))
                    .child(text_field(edit))
                    .children(error.clone().map(|e| div().text_xs().text_color(rgb(colors::EXITED)).child(e)))
                    .child(div().text_xs().text_color(rgb(colors::DIM)).child("Enter to create · Esc to cancel")),
            ),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::root_render();
        self.schedule_resize(window, cx);

        let terminal = match self.tabs.get(self.active) {
            Some(tab) => tab.view.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(colors::DIM))
                .child("No sessions — Ctrl+Shift+T to start one")
                .into_any_element(),
        };
        let side = if self.sidebar_open {
            div()
                .flex()
                .h_full()
                .child(self.sidebar(cx))
                .child(
                    div()
                        .id("divider")
                        .w(px(DIVIDER))
                        .h_full()
                        .cursor_col_resize()
                        .bg(rgb(colors::DIVIDER))
                        .hover(|s| s.bg(rgb(colors::DIVIDER_HOVER)))
                        .on_drag(SidebarDrag, |_, _, _, cx| cx.new(|_| DragGhost)),
                )
                .into_any_element()
        } else {
            div()
                .id("expand")
                .w(px(COLLAPSED))
                .h_full()
                .cursor_pointer()
                .bg(rgb(colors::SIDEBAR))
                .hover(|s| s.bg(rgb(colors::DIVIDER_HOVER)))
                .on_click(cx.listener(|w, _: &ClickEvent, _, cx| w.toggle_sidebar(cx)))
                .into_any_element()
        };

        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .flex()
            .size_full()
            .bg(palette::BG)
            .text_color(palette::FG)
            .on_key_down(cx.listener(Self::modal_key))
            .on_action(cx.listener(|w, _: &NewSession, window, cx| w.open_new_session(window, cx)))
            .on_action(cx.listener(|w, _: &CloseSession, window, cx| w.close_active(window, cx)))
            .on_action(cx.listener(|w, _: &NextSession, window, cx| w.cycle(1, window, cx)))
            .on_action(cx.listener(|w, _: &PrevSession, window, cx| w.cycle(-1, window, cx)))
            .on_action(cx.listener(|w, _: &Rename, window, cx| w.start_rename(w.active, window, cx)))
            .on_action(cx.listener(|w, _: &ToggleSidebar, _, cx| w.toggle_sidebar(cx)))
            .on_action(cx.listener(|w, _: &Activate1, window, cx| w.activate(0, window, cx)))
            .on_action(cx.listener(|w, _: &Activate2, window, cx| w.activate(1, window, cx)))
            .on_action(cx.listener(|w, _: &Activate3, window, cx| w.activate(2, window, cx)))
            .on_action(cx.listener(|w, _: &Activate4, window, cx| w.activate(3, window, cx)))
            .on_action(cx.listener(|w, _: &Activate5, window, cx| w.activate(4, window, cx)))
            .on_action(cx.listener(|w, _: &Activate6, window, cx| w.activate(5, window, cx)))
            .on_action(cx.listener(|w, _: &Activate7, window, cx| w.activate(6, window, cx)))
            .on_action(cx.listener(|w, _: &Activate8, window, cx| w.activate(7, window, cx)))
            .on_action(cx.listener(|w, _: &Activate9, window, cx| w.activate(8, window, cx)))
            .on_drag_move::<SidebarDrag>(cx.listener(|w, e: &DragMoveEvent<SidebarDrag>, window, cx| {
                w.dragging = true;
                let max = (window.viewport_size().width * 0.5).max(px(140.));
                w.sidebar_width = (e.event.position.x - e.bounds.left()).clamp(px(140.), max);
                cx.notify();
            }))
            .on_drop::<SidebarDrag>(cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .child(side)
            .child(div().flex_1().h_full().overflow_hidden().child(terminal))
            .children(self.modal_view())
    }
}

impl Workspace {
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.dragging {
            self.dragging = false;
            self.flush_resize = true;
            cx.notify();
        }
    }
}

fn icon_button(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .text_color(rgb(colors::DIM))
        .hover(|s| s.bg(rgb(colors::ROW_HOVER)).text_color(palette::FG))
        .child(label)
}

fn text_field(edit: &LineEdit) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(rgb(colors::INPUT))
        .text_sm()
        .child(edit.before_cursor().to_string())
        .child(div().w(px(1.5)).h(px(16.)).bg(palette::CURSOR))
        .child(edit.after_cursor().to_string())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn untilde(s: &str) -> PathBuf {
    match s.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(s),
    }
}
