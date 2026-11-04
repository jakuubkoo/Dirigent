//! Root view: session list, sidebar, the active terminal (cached view), modal, resize policy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    ClickEvent, Context, DragMoveEvent, Entity, EntityId, FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Render,
    SharedString, StyleRefinement, Task, Window, actions, div, prelude::*, px, rgb,
};
use session::{Session, Size};

use crate::agent::{self, Hub};
use crate::git_panel::{self, DiffView, GitPanel, PanelView, Repos, line_counts};
use crate::line_edit::{Edit, LineEdit};
use crate::persist::{self, State as Saved, TabState, WindowMode, WindowState};
use crate::window_chrome::{self, Chrome};
use crate::terminal_element::{Metrics, palette};
use crate::terminal_view::{Banner, PADDING, TerminalView};

actions!(
    agent_ide,
    [
        NewSession, CloseSession, NextSession, PrevSession, Rename, ToggleSidebar, NextNeedsYou, ResumeAll, ToggleGitPanel,
        Activate1, Activate2, Activate3, Activate4, Activate5, Activate6, Activate7, Activate8, Activate9
    ]
);

const DIVIDER: f32 = 4.;
const COLLAPSED: f32 = 10.;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub(crate) mod colors {
    pub const SIDEBAR: u32 = 0x17171c;
    pub const ROW_ACTIVE: u32 = 0x2b2b35;
    pub const ROW_HOVER: u32 = 0x212129;
    pub const DIM: u32 = 0x8a8a96;
    pub const DOT: u32 = 0x5fa2e6;
    pub const EXITED: u32 = 0xe0626c;
    pub const DIVIDER: u32 = 0x26262e;
    pub const DIVIDER_HOVER: u32 = 0x4a4a58;
    pub const MODAL: u32 = 0x24242c;
    pub const WORKING: u32 = 0x5fa2e6;
    pub const NEEDS_YOU: u32 = 0xf0a030;
    pub const DONE: u32 = 0x8cc265;
    pub const BAR_BG: u32 = 0x2b2b35;
    pub const INPUT: u32 = 0x1a1a20;
}

struct Tab {
    id: usize,
    name: String,
    /// Directory the tab belongs to (kept even if it no longer exists; the shell then starts in $HOME).
    cwd: PathBuf,
    dir_missing: bool,
    /// None until the tab is first viewed (restored tabs spawn lazily).
    view: Option<Entity<TerminalView>>,
    claude_session: Option<String>,
    claude_args: Vec<String>,
    /// The git repo `cwd` is in (resolved off-thread when the tab spawns and on every cwd change).
    repo: TabRepo,
    /// Bumped per resolve: only the newest one applies (`cd a; cd b` in quick succession).
    repo_gen: u64,
    /// Claude Code's cwd from its last hook, as last applied (so each change is applied once).
    hook_cwd: Option<PathBuf>,
}

#[derive(Clone, PartialEq)]
enum TabRepo {
    /// Not spawned yet: no discovery, no watcher.
    Unknown,
    Pending,
    NotRepo,
    GitMissing,
    /// Holds one reference on that root's watcher in `Workspace::repos`.
    Repo(PathBuf),
}

enum Modal {
    NewSession { edit: LineEdit, error: Option<String> },
    Rename { tab: usize, edit: LineEdit },
}

#[derive(Clone)]
struct SidebarDrag;

#[derive(Clone)]
struct GitPanelDrag;

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
    /// Left button went down on the title bar; the first move with it held starts a compositor move.
    title_pressed: bool,
    window_title: String,
    /// Claude Code hooks/statusLine receiver; None if the runtime dir/socket couldn't be set up.
    agents: Option<Hub>,
    _agent_task: Option<Task<()>>,
    /// Re-renders the limits countdown at the next minute boundary; only while limits are known and the window is
    /// visible.
    _minute: Option<Task<()>>,
    window_visible: bool,
    _subscriptions: Vec<gpui::Subscription>,
    state_path: PathBuf,
    _save_task: Option<Task<()>>,
    save_pending: bool,
    window_state: Option<WindowState>,
    repos: Repos,
    _repos_task: Task<()>,
    /// Status Arc pointer per repo as of the last re-render: a refresh with an unchanged status re-renders nothing.
    repos_seen: HashMap<PathBuf, usize>,
    git_panel: Entity<GitPanel>,
    git_open: bool,
    git_width: Pixels,
    /// Open diff (shown over the terminal); the active session is hidden meanwhile.
    diff: Option<Entity<DiffView>>,
    _diff_task: Option<Task<()>>,
    /// Bumped per diff read: a stale read sees it changed, kills its `git diff` and is dropped.
    diff_gen: Arc<AtomicU64>,
    /// The open diff's repo refresh count when it was last read.
    diff_seen: u64,
}

impl Workspace {
    pub fn new(saved: Saved, state_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let metrics = Metrics::new(window);
        let last_dir = saved
            .last_dir
            .clone()
            .filter(|d| d.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(home);
        let (repos, repo_pings) = Repos::new();
        let repos_task = cx.spawn(async move |this, cx| {
            while repo_pings.recv().await.is_ok() {
                if this.update(cx, |w, cx| w.repos_changed(cx)).is_err() {
                    return;
                }
            }
        });
        let git_panel = {
            let ws = cx.entity().downgrade();
            cx.new(|_| GitPanel::new(ws))
        };
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            focus: cx.focus_handle(),
            sidebar_width: px(saved.sidebar.width.clamp(140., 2000.)),
            sidebar_open: saved.sidebar.open,
            dragging: false,
            flush_resize: false,
            applied: Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
            pending: None,
            _resize_task: None,
            last_dir,
            modal: None,
            _cycle: None,
            title_pressed: false,
            window_title: String::new(),
            agents: None,
            _agent_task: None,
            _minute: None,
            window_visible: true,
            _subscriptions: Vec::new(),
            state_path,
            _save_task: None,
            save_pending: false,
            window_state: window_state(window),
            metrics,
            repos,
            _repos_task: repos_task,
            repos_seen: HashMap::new(),
            git_panel,
            git_open: saved.git_panel.open,
            git_width: px(saved.git_panel.width.clamp(git_panel::MIN_WIDTH, 2000.)),
            diff: None,
            _diff_task: None,
            diff_gen: Arc::new(AtomicU64::new(0)),
            diff_seen: 0,
        };
        match Hub::start() {
            Ok(hub) => {
                let rx = hub.changes.clone();
                this._agent_task = Some(cx.spawn(async move |this, cx| {
                    while rx.recv().await.is_ok() {
                        crate::perf::event("agent state change");
                        if this.update(cx, |w, cx| w.agents_changed(cx)).is_err() {
                            return;
                        }
                    }
                }));
                let dir = hub.dir.clone();
                this._subscriptions.push(cx.on_app_quit(move |_, _| {
                    let _ = std::fs::remove_dir_all(&dir);
                    async {}
                }));
                this.agents = Some(hub);
            }
            Err(e) => eprintln!("agent-ide: Claude Code integration disabled: {e}"),
        }
        this._subscriptions.push(cx.observe_window_visibility(window, |w, visibility, _, cx| {
            w.window_visible = visibility == gpui::WindowVisibility::Visible;
            if !w.window_visible {
                w._minute = None; // no countdown ticks while nobody can see them
            }
            cx.notify();
        }));
        this._subscriptions.push(cx.observe_window_bounds(window, |w, window, cx| {
            let ws = window_state(window);
            if ws != w.window_state {
                w.window_state = ws;
                w.mark_dirty(cx);
            }
        }));
        this._subscriptions.push(cx.on_app_quit(|w, _| {
            // Final save, synchronous: the process is about to exit.
            w._save_task = None;
            if let Err(e) = persist::save(&w.state_path, &w.snapshot()) {
                eprintln!("agent-ide: saving state failed: {e}");
            }
            async {}
        }));
        this.applied = this.grid_size(window);

        if let Ok(script) = std::env::var("AGENT_IDE_RUN") {
            // Perf harness: one (eagerly spawned) session per line, the line typed into it; saved tabs are ignored.
            for line in script.split('\n') {
                let dir = this.last_dir.clone();
                let i = this.add_tab(None, dir, None, Vec::new());
                let view = this.ensure_spawned(i, window, cx);
                if !line.is_empty() {
                    view.read(cx).session.write(format!("{line}\r").as_bytes());
                }
            }
            this.activate(0, window, cx);
        } else if saved.tabs.is_empty() {
            let dir = this.last_dir.clone();
            this.add_tab(None, dir, None, Vec::new());
            this.activate(0, window, cx);
        } else {
            // Restore: rows for every tab, a shell only for the active one (the rest spawn on first view).
            for t in saved.tabs {
                this.add_tab(Some(t.name), t.cwd, t.claude_session, t.claude_args);
            }
            let active = saved.active.min(this.tabs.len() - 1);
            this.activate(active, window, cx);
            this.check_resumable(cx);
        }

        if let Ok(path) = std::env::var("AGENT_IDE_PERF_DIFF") {
            // Perf harness: open that file's diff in the active tab's repo, then jump 40 rows every 50 ms
            // (AGENT_IDE_PERF_DIFF_STEPS times, default 100).
            cx.spawn_in(window, async move |this, cx| {
                let bg = cx.background_executor().clone();
                let timer = |ms| bg.timer(Duration::from_millis(ms));
                for _ in 0..100 {
                    timer(100).await;
                    let opened = this.update_in(cx, |w, window, cx| {
                        let TabRepo::Repo(root) = w.tabs.get(w.active)?.repo.clone() else { return None };
                        let file = w.repos.status(&root)?.files.iter().find(|f| f.path == path)?.clone();
                        w.toggle_diff(root, file, window, cx);
                        Some(())
                    });
                    if matches!(opened, Ok(Some(()))) {
                        break;
                    }
                }
                let started = Instant::now();
                loop {
                    timer(20).await;
                    let rows = this.update(cx, |w, cx| w.diff.as_ref().map_or(0, |d| d.read(cx).loaded_rows())).unwrap_or(0);
                    if rows > 0 || started.elapsed() > Duration::from_secs(10) {
                        eprintln!("[perf] diff open → rows ready {:.0} ms ({} rows)", started.elapsed().as_secs_f64() * 1e3, rows);
                        break;
                    }
                }
                eprintln!("[perf] diff scroll start");
                let steps = std::env::var("AGENT_IDE_PERF_DIFF_STEPS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
                for step in 1..=steps {
                    timer(50).await;
                    let _ = this.update(cx, |w, cx| {
                        if let Some(d) = &w.diff {
                            d.update(cx, |d, cx| d.scroll_to(step * 40, cx));
                        }
                    });
                }
                eprintln!("[perf] diff scroll end");
            })
            .detach();
        }

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
        let right = if self.git_open { self.git_width + px(DIVIDER) } else { px(0.) };
        let m = &self.metrics;
        let chrome = Chrome::of(window);
        let w = vp.width - side - right - px(PADDING) - chrome.horizontal();
        let h = vp.height - px(PADDING) - chrome.vertical();
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
        for view in self.tabs.iter().filter_map(|t| t.view.clone()) {
            n += view.update(cx, |v, _| v.resize_to(size)) as u64;
        }
        crate::perf::pty_resizes(n);
        if let Some(view) = self.tabs.get(self.active).and_then(|t| t.view.as_ref()) {
            view.update(cx, |_, cx| cx.notify());
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

    /// Adds a tab row without starting its shell; returns its index.
    fn add_tab(&mut self, name: Option<String>, cwd: PathBuf, claude_session: Option<String>, claude_args: Vec<String>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let dir_missing = !cwd.is_dir();
        let name = name.unwrap_or_else(|| format!("Session {id}"));
        self.tabs.push(Tab {
            id,
            name,
            cwd,
            dir_missing,
            view: None,
            claude_session,
            claude_args,
            repo: TabRepo::Unknown,
            repo_gen: 0,
            hook_cwd: None,
        });
        self.tabs.len() - 1
    }

    /// Starts the tab's shell if it hasn't been yet; a tab with a saved Claude session gets the resume banner.
    fn ensure_spawned(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) -> Entity<TerminalView> {
        if let Some(view) = &self.tabs[i].view {
            return view.clone();
        }
        let tab = &self.tabs[i];
        let dir = if tab.dir_missing { home() } else { tab.cwd.clone() };
        let (id, resume) = (tab.id, resume_command(tab));
        let view = self.new_view(id, &dir, window, cx);
        if let Some(cmd) = resume {
            view.update(cx, |v, cx| v.set_banner(Some(Banner::Resume(cmd)), cx));
        }
        self.tabs[i].view = Some(view.clone());
        self.resolve_repo(i, cx);
        view
    }

    fn new_view(&mut self, id: usize, dir: &Path, _: &mut Window, cx: &mut Context<Self>) -> Entity<TerminalView> {
        let env = match &self.agents {
            Some(hub) => hub.session_env(id as u32).unwrap_or_else(|e| {
                eprintln!("agent-ide: no Claude Code hooks for session {id}: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        let session = Session::spawn(dir, self.applied, &env).expect("spawn shell");
        let (metrics, ws) = (self.metrics.clone(), cx.entity().downgrade());
        cx.new(|cx| TerminalView::new(session, metrics, ws, cx))
    }

    fn activate(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        crate::perf::switch_started();
        self.drop_diff();
        if let Some(old) = self.tabs.get(self.active).and_then(|t| t.view.as_ref()) {
            old.read(cx).session.set_visible(false);
        }
        let view = self.ensure_spawned(idx, window, cx);
        let new = &self.tabs[idx];
        view.read(cx).session.set_visible(true);
        if let Some(hub) = &self.agents {
            hub.board.lock().unwrap().mark_seen(new.id as u32);
        }
        // It may have output since its cached frame was recorded.
        view.update(cx, |_, cx| cx.notify());
        if self.modal.is_none() {
            window.focus(&view.read(cx).focus_handle(cx), cx);
        }
        if self.active != idx {
            self.active = idx;
            self.mark_dirty(cx);
        }
        self.sync_panel(cx);
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
        let tab = self.tabs.remove(self.active);
        if let Some(hub) = &self.agents {
            hub.remove_tab(tab.id as u32);
        }
        if let TabRepo::Repo(root) = &tab.repo {
            self.repos.release(root);
        }
        self.drop_diff();
        self.mark_dirty(cx);
        self.modal = None;
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus, cx);
            self.sync_panel(cx);
            cx.notify();
        } else {
            let idx = self.active.min(self.tabs.len() - 1);
            self.active = usize::MAX; // old one is gone; nothing to hide
            self.activate(idx, window, cx);
        }
    }

    pub fn restart(&mut self, view_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.tab_of_view(view_id) else { return };
        if let Some(hub) = &self.agents {
            hub.board.lock().unwrap().reset_live(self.tabs[i].id as u32);
        }
        self.tabs[i].view = None;
        self.ensure_spawned(i, window, cx);
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
        if let Some(view) = self.tabs.get(self.active).and_then(|t| t.view.as_ref()) {
            window.focus(&view.read(cx).focus_handle(cx), cx);
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
                        self.mark_dirty(cx);
                    }
                    self.close_modal(window, cx);
                }
                Some(Modal::NewSession { edit, .. }) => {
                    let dir = untilde(edit.text.trim());
                    if dir.is_dir() {
                        self.last_dir = dir.clone();
                        self.modal = None;
                        let i = self.add_tab(None, dir, None, Vec::new());
                        self.activate(i, window, cx);
                        self.mark_dirty(cx);
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
        let board = self.agents.as_ref().map(|h| h.board.lock().unwrap());
        let rows = self.tabs.iter().enumerate().map(|(i, tab)| {
            let session = tab.view.as_ref().map(|v| v.read(cx).session.clone());
            let (unread, exited) = session.as_ref().map_or((false, false), |s| (s.is_unread(), s.is_exited()));
            let cwd = if tab.dir_missing { format!("⚠ missing · {}", tilde(&tab.cwd)) } else { tilde(&tab.cwd) };
            let active = i == self.active;
            let agent = match &board {
                Some(b) if !exited => b.get(tab.id as u32),
                _ => agent::Agent::default(),
            };
            let name: SharedString = match &self.modal {
                Some(Modal::Rename { tab: r, edit }) if *r == i => {
                    return div().id(("tab", tab.id)).px_2().py_1().child(text_field(edit)).into_any_element();
                }
                _ => tab.name.clone().into(),
            };
            let needs_you = agent.state == agent::State::NeedsYou;
            let badge = match agent.state {
                agent::State::NoAgent if tab.claude_session.is_some() => Some(("resumable", colors::DIM)),
                agent::State::NoAgent => None,
                agent::State::Ready => Some(("claude", colors::DIM)),
                agent::State::Working => Some(("working", colors::WORKING)),
                agent::State::NeedsYou => Some(("needs you", colors::NEEDS_YOU)),
                agent::State::Done => Some(("done", colors::DONE)),
            };
            let changes = match &tab.repo {
                TabRepo::Repo(root) => self.repos.status(root).map(|s| s.totals()).filter(|t| t.0 > 0),
                _ => None,
            };
            let model_line = (agent.state != agent::State::NoAgent).then(|| {
                let mut parts = vec![agent.model.clone().unwrap_or_else(|| "claude".into())];
                if let Some(p) = agent.context_pct {
                    parts.push(format!("ctx {p}%"));
                }
                parts.join(" · ")
            });
            div()
                .id(("tab", tab.id))
                .flex()
                .items_center()
                .gap_2()
                .pl(px(9.))
                .pr_3()
                .py_1()
                .cursor_pointer()
                // Needs you stands out: amber accent bar on the left edge (transparent otherwise, no layout shift).
                .border_l(px(3.))
                .border_color(if needs_you { rgb(colors::NEEDS_YOU).into() } else { gpui::transparent_black() })
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
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().text_sm().truncate().child(name))
                                .children(badge.map(|(label, color)| {
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(rgb(color))
                                        .when(needs_you || agent.unseen, |d| d.font_weight(gpui::FontWeight::BOLD))
                                        .child(label)
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(rgb(if tab.dir_missing { colors::NEEDS_YOU } else { colors::DIM }))
                                .child(cwd),
                        )
                        .children(
                            model_line.map(|m| div().text_xs().truncate().text_color(rgb(colors::DIM)).child(m)),
                        )
                        .children(changes.map(|(n, add, del)| {
                            let clamp = |v: u64| v.min(u32::MAX as u64) as u32;
                            div()
                                .flex()
                                .gap_1()
                                .text_xs()
                                .text_color(rgb(colors::DIM))
                                .child(format!("{n} file{}", if n == 1 { "" } else { "s" }))
                                .child(line_counts(Some((clamp(add), clamp(del)))))
                        })),
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
                                if let Some(id) = w.tabs[i].view.as_ref().map(|v| v.entity_id()) {
                                    w.restart(id, window, cx);
                                }
                                cx.stop_propagation();
                            })),
                    )
                })
                .when(unread && !active && !exited && badge.is_none(), |d| {
                    d.child(div().size(px(7.)).rounded_full().bg(rgb(colors::DOT)))
                })
                .into_any_element()
        });
        let rows: Vec<_> = rows.collect();
        let limits = board.as_ref().map(|b| (b.five_hour, b.seven_day));
        drop(board);

        let header = div()
            .flex()
            .items_center()
            .px_3()
            .py_2()
            .child(div().flex_1().text_xs().text_color(rgb(colors::DIM)).child("Sessions"))
            .child(icon_button("new", "+").on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_new_session(window, cx))))
            .child(icon_button("collapse", "‹").on_click(cx.listener(|w, _: &ClickEvent, _, cx| w.toggle_sidebar(cx))));

        let now = now_secs();
        let footer = match limits {
            Some((five, seven)) if five.is_some() || seven.is_some() => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(colors::DIVIDER))
                    .child(limit_row("5h limit", five, now))
                    .child(limit_row("Weekly limit", seven, now)),
            ),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(self.sidebar_width)
            .h_full()
            .bg(rgb(colors::SIDEBAR))
            .child(header)
            .child(div().id("tabs").flex_1().overflow_y_scroll().children(rows))
            .children(footer)
    }

    /// Agent states/limits changed (listener thread). The tab being looked at is never "unseen".
    fn agents_changed(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = &self.agents else { return };
        let mut dirty = false;
        let mut moves = Vec::new();
        {
            let mut board = hub.board.lock().unwrap();
            if let Some(tab) = self.tabs.get(self.active) {
                board.mark_seen(tab.id as u32);
            }
            for (i, tab) in self.tabs.iter_mut().enumerate() {
                let a = board.get(tab.id as u32);
                if a.cwd != tab.hook_cwd {
                    tab.hook_cwd = a.cwd.clone();
                    moves.extend(a.cwd.clone().map(|d| (i, d)));
                }
                if a.resume_failed.is_some() && a.resume_failed == tab.claude_session {
                    // `claude --resume` said the session doesn't exist: drop it and the badge.
                    board.forget_session(tab.id as u32);
                    tab.claude_session = None;
                    dirty = true;
                } else if a.session_id.is_some() && a.session_id != tab.claude_session {
                    tab.claude_session = a.session_id;
                    dirty = true;
                }
                if let Some(args) = a.args
                    && args != tab.claude_args
                {
                    tab.claude_args = args;
                    dirty = true;
                }
            }
        }
        if dirty {
            self.mark_dirty(cx);
        }
        for (i, dir) in moves {
            self.set_cwd(i, dir, cx);
        }
        cx.notify();
    }

    fn tab_of_view(&self, view_id: EntityId) -> Option<usize> {
        self.tabs.iter().position(|t| t.view.as_ref().is_some_and(|v| v.entity_id() == view_id))
    }

    /// Saves at most 500 ms after the first unsaved change (not re-armed by later ones, so a steady stream of
    /// changes can't postpone it forever); the snapshot is taken when it fires and written off the UI thread.
    fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Ok((path, snapshot)) = this.update(cx, |w, _| {
                w.save_pending = false;
                (w.state_path.clone(), w.snapshot())
            }) else {
                return;
            };
            let result = cx.background_executor().spawn(async move { persist::save(&path, &snapshot) }).await;
            match result {
                Ok(()) => crate::perf::event("state saved"),
                Err(e) => eprintln!("agent-ide: saving state failed: {e}"),
            }
        }));
    }

    fn snapshot(&self) -> Saved {
        Saved {
            tabs: self
                .tabs
                .iter()
                .map(|t| TabState {
                    name: t.name.clone(),
                    cwd: t.cwd.clone(),
                    claude_session: t.claude_session.clone(),
                    claude_args: t.claude_args.clone(),
                })
                .collect(),
            active: self.active.min(self.tabs.len().saturating_sub(1)),
            sidebar: persist::SidebarState { width: f32::from(self.sidebar_width), open: self.sidebar_open },
            window: self.window_state,
            last_dir: Some(self.last_dir.clone()),
            git_panel: persist::PanelState { width: f32::from(self.git_width), open: self.git_open },
            ..Saved::default()
        }
    }

    /// Startup: drop saved Claude sessions whose transcript is gone (checked off the UI thread, once).
    fn check_resumable(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<(usize, String)> =
            self.tabs.iter().filter_map(|t| Some((t.id, t.claude_session.clone()?))).collect();
        if ids.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let gone: Vec<usize> = cx
                .background_executor()
                .spawn(async move { ids.into_iter().filter(|(_, s)| !persist::transcript_exists(s)).map(|(id, _)| id).collect() })
                .await;
            if gone.is_empty() {
                return;
            }
            let _ = this.update(cx, |w, cx| {
                for t in w.tabs.iter_mut().filter(|t| gone.contains(&t.id)) {
                    t.claude_session = None;
                    if let Some(view) = &t.view {
                        view.update(cx, |v, cx| {
                            if matches!(v.banner(), Some(Banner::Resume(_))) {
                                v.set_banner(None, cx);
                                cx.notify();
                            }
                        });
                    }
                }
                w.mark_dirty(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Enter on the resume banner: re-check the transcript off-thread, then type `claude --resume <id> <flags>`.
    pub fn resume(&mut self, view_id: EntityId, cx: &mut Context<Self>) {
        let Some(i) = self.tab_of_view(view_id) else { return };
        self.resume_tabs(vec![i], cx);
    }

    /// Ctrl+Shift+R: resume every tab with a saved session whose claude isn't running (spawning hidden ones).
    fn resume_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let board = self.agents.as_ref().map(|h| h.board.lock().unwrap());
        let idle = |t: &Tab| board.as_ref().is_none_or(|b| b.get(t.id as u32).state == agent::State::NoAgent);
        let targets: Vec<usize> =
            (0..self.tabs.len()).filter(|&i| self.tabs[i].claude_session.is_some() && idle(&self.tabs[i])).collect();
        drop(board);
        for &i in &targets {
            let view = self.ensure_spawned(i, window, cx);
            view.update(cx, |v, cx| {
                v.set_banner(None, cx);
                cx.notify();
            });
        }
        self.resume_tabs(targets, cx);
    }

    fn resume_tabs(&mut self, tabs: Vec<usize>, cx: &mut Context<Self>) {
        let jobs: Vec<(usize, String, String)> = tabs
            .into_iter()
            .filter_map(|i| {
                let t = &self.tabs[i];
                Some((t.id, t.claude_session.clone()?, resume_command(t)?))
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let checked: Vec<(usize, String, String, bool)> = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter().map(|(id, s, a)| {
                        let ok = persist::transcript_exists(&s);
                        (id, s, a, ok)
                    }).collect()
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                for (id, _session, cmd, exists) in checked {
                    let Some(t) = w.tabs.iter_mut().find(|t| t.id == id) else { continue };
                    let Some(view) = t.view.clone() else { continue };
                    if exists {
                        view.read(cx).session.write(format!("{cmd}\r").as_bytes());
                    } else {
                        t.claude_session = None;
                        view.update(cx, |v, cx| {
                            v.set_banner(Some(Banner::Gone), cx);
                            cx.notify();
                        });
                        w.mark_dirty(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn needs_you_count(&self) -> usize {
        let Some(hub) = &self.agents else { return 0 };
        let board = hub.board.lock().unwrap();
        self.tabs.iter().filter(|t| board.get(t.id as u32).state == agent::State::NeedsYou).count()
    }

    /// Jumps to the next session that needs you; if none, the next finished one you haven't seen.
    fn next_needs_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hub) = &self.agents else { return };
        let n = self.tabs.len();
        let target = {
            let board = hub.board.lock().unwrap();
            let order = (1..=n).map(|k| (self.active + k) % n);
            let find = |pred: &dyn Fn(&agent::Agent) -> bool| {
                order.clone().find(|&i| pred(&board.get(self.tabs[i].id as u32)))
            };
            find(&|a| a.state == agent::State::NeedsYou).or_else(|| find(&|a| a.unseen))
        };
        if let Some(i) = target {
            self.activate(i, window, cx);
        }
    }

    /// Keeps the limits countdown fresh: one wakeup per minute boundary, only while it's shown and visible.
    fn arm_minute_timer(&mut self, cx: &mut Context<Self>) {
        let known = self.agents.as_ref().is_some_and(|h| {
            let b = h.board.lock().unwrap();
            b.five_hour.is_some() || b.seven_day.is_some()
        });
        if !known || !self.window_visible || !self.sidebar_open || self._minute.is_some() {
            return;
        }
        let wait = 60 - now_secs().rem_euclid(60) as u64;
        self._minute = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(wait)).await;
            let _ = this.update(cx, |w, cx| {
                w._minute = None;
                cx.notify();
            });
        }));
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        self.flush_resize = true;
        self.mark_dirty(cx);
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
        self.arm_minute_timer(cx);

        let terminal = match (&self.diff, self.tabs.get(self.active)) {
            (Some(diff), _) => diff.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (None, Some(Tab { view: Some(view), .. })) => {
                view.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            }
            _ => div()
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

        let git_panel = self.git_open.then(|| {
            div()
                .flex()
                .flex_none()
                .h_full()
                .child(
                    div()
                        .id("git-divider")
                        .w(px(DIVIDER))
                        .h_full()
                        .cursor_col_resize()
                        .bg(rgb(colors::DIVIDER))
                        .hover(|s| s.bg(rgb(colors::DIVIDER_HOVER)))
                        .on_drag(GitPanelDrag, |_, _, _, cx| cx.new(|_| DragGhost)),
                )
                .child(
                    div()
                        .w(self.git_width)
                        .h_full()
                        .child(self.git_panel.clone().cached(StyleRefinement::default().size_full())),
                )
        });

        let chrome = Chrome::of(window);
        let mut title = match self.tabs.get(self.active) {
            Some(tab) => format!("{} — agent-ide", tab.name),
            None => "agent-ide".to_string(),
        };
        match self.needs_you_count() {
            0 => {}
            1 => title.push_str(" · 1 needs you"),
            n => title.push_str(&format!(" · {n} need you")),
        }
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title.clone();
        }
        let title_bar = chrome.csd.then(|| {
            window_chrome::title_bar(
                title,
                window,
                rgb(colors::SIDEBAR).into(),
                rgb(colors::DIM).into(),
                rgb(colors::ROW_HOVER).into(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|w, e: &gpui::MouseDownEvent, window, _| {
                    if e.click_count == 2 {
                        w.title_pressed = false;
                        window.zoom_window();
                    } else {
                        w.title_pressed = true;
                    }
                }),
            )
            .on_mouse_move(cx.listener(|w, e: &gpui::MouseMoveEvent, window, _| {
                if w.title_pressed && e.pressed_button == Some(MouseButton::Left) {
                    w.title_pressed = false;
                    window.start_window_move();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|w, _, _, _| w.title_pressed = false))
            .on_click(|e, window, _| {
                if e.is_right_click() {
                    window.show_window_menu(e.position());
                }
            })
        });

        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette::BG)
            .text_color(palette::FG)
            .when(chrome.border > px(0.), |d| d.border(chrome.border).border_color(rgb(colors::DIVIDER_HOVER)))
            .on_key_down(cx.listener(Self::modal_key))
            .on_action(cx.listener(|w, _: &NewSession, window, cx| w.open_new_session(window, cx)))
            .on_action(cx.listener(|w, _: &CloseSession, window, cx| w.close_active(window, cx)))
            .on_action(cx.listener(|w, _: &NextSession, window, cx| w.cycle(1, window, cx)))
            .on_action(cx.listener(|w, _: &PrevSession, window, cx| w.cycle(-1, window, cx)))
            .on_action(cx.listener(|w, _: &Rename, window, cx| w.start_rename(w.active, window, cx)))
            .on_action(cx.listener(|w, _: &ToggleSidebar, _, cx| w.toggle_sidebar(cx)))
            .on_action(cx.listener(|w, _: &NextNeedsYou, window, cx| w.next_needs_you(window, cx)))
            .on_action(cx.listener(|w, _: &ResumeAll, window, cx| w.resume_all(window, cx)))
            .on_action(cx.listener(|w, _: &ToggleGitPanel, _, cx| w.toggle_git_panel(cx)))
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
            .on_drag_move::<GitPanelDrag>(cx.listener(|w, e: &DragMoveEvent<GitPanelDrag>, window, cx| {
                w.dragging = true;
                let min = px(git_panel::MIN_WIDTH);
                let max = (window.viewport_size().width * 0.6).max(min);
                w.git_width = (e.bounds.right() - e.event.position.x).clamp(min, max);
                w.sync_panel(cx);
                cx.notify();
            }))
            .on_drop::<GitPanelDrag>(cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .children(title_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(side)
                    .child(div().flex_1().min_w_0().h_full().overflow_hidden().child(terminal))
                    .children(git_panel),
            )
            .children(self.modal_view())
            .when(chrome.csd, |d| d.children(window_chrome::resize_handles(chrome.tiling)))
    }
}

/// Git: repo resolution per tab, cwd following, the panel and the diff view.
impl Workspace {
    /// Finds the repo the tab's cwd is in (off the UI thread) and moves its watcher reference there.
    fn resolve_repo(&mut self, i: usize, cx: &mut Context<Self>) {
        let t = &mut self.tabs[i];
        t.repo_gen += 1;
        if t.repo == TabRepo::Unknown {
            t.repo = TabRepo::Pending;
        }
        let (id, generation) = (t.id, t.repo_gen);
        let dir = if t.dir_missing { home() } else { t.cwd.clone() };
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { git::discover(&dir) }).await;
            let _ = this.update(cx, |w, cx| w.repo_resolved(id, generation, found, cx));
        })
        .detach();
    }

    fn repo_resolved(&mut self, id: usize, generation: u64, found: git::Discovery, cx: &mut Context<Self>) {
        // Tab closed meanwhile, or a newer resolve is running: nothing to do (nothing was acquired yet).
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id && t.repo_gen == generation) else { return };
        let new = match &found {
            git::Discovery::Repo(r) => TabRepo::Repo(r.root.clone()),
            git::Discovery::NotRepo => TabRepo::NotRepo,
            git::Discovery::GitMissing => TabRepo::GitMissing,
        };
        if new == t.repo {
            return;
        }
        crate::perf::event_with(|| format!("tab {id} repo → {}", match &new { TabRepo::Repo(r) => r.display().to_string(), _ => "none".into() }));
        if let TabRepo::Repo(old) = std::mem::replace(&mut t.repo, new) {
            self.repos.release(&old);
        }
        if let git::Discovery::Repo(r) = found {
            self.repos.acquire(r);
        }
        self.sync_panel(cx);
        cx.notify();
    }

    /// The shell (OSC 7) or Claude Code (hook payload) is in a new directory: it becomes the tab's directory
    /// (saved), and its repo is resolved again.
    fn set_cwd(&mut self, i: usize, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        if t.cwd == dir && !t.dir_missing {
            return;
        }
        crate::perf::event_with(|| format!("tab {} cwd → {}", t.id, dir.display()));
        t.cwd = dir;
        t.dir_missing = false;
        self.mark_dirty(cx);
        self.resolve_repo(i, cx);
        cx.notify();
    }

    pub fn shell_cwd_changed(&mut self, view_id: EntityId, dir: PathBuf, cx: &mut Context<Self>) {
        if let Some(i) = self.tab_of_view(view_id) {
            self.set_cwd(i, dir, cx);
        }
    }

    /// A watcher ran `git status`. Re-renders the sidebar only if a status changed; re-reads an open diff
    /// whenever its repo refreshed (same +/- counts can still be different content).
    fn repos_changed(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let roots: Vec<PathBuf> = self.repos.roots().cloned().collect();
        for root in &roots {
            let ptr = self.repos.status(root).map_or(0, |s| Arc::as_ptr(&s) as usize);
            changed |= self.repos_seen.insert(root.clone(), ptr) != Some(ptr);
        }
        self.repos_seen.retain(|r, _| roots.contains(r));
        crate::perf::event(if changed { "git refresh (changed)" } else { "git refresh (same)" });
        if changed {
            cx.notify();
        }
        self.sync_panel(cx);
        let diff_root = self.diff.as_ref().map(|d| d.read(cx).root.clone());
        if let Some(root) = diff_root {
            let n = self.repos.refreshes(&root);
            if n != self.diff_seen {
                self.diff_seen = n;
                self.load_diff(cx);
            }
        }
    }

    /// Pushes the active tab's repo state into the panel (it re-renders only if that differs).
    fn sync_panel(&mut self, cx: &mut Context<Self>) {
        let view = match self.tabs.get(self.active) {
            None => PanelView::NoSession,
            Some(t) => match &t.repo {
                TabRepo::Unknown | TabRepo::Pending => PanelView::Pending,
                TabRepo::NotRepo => PanelView::NotRepo,
                TabRepo::GitMissing => PanelView::GitMissing,
                TabRepo::Repo(root) => PanelView::Repo { root: root.clone(), status: self.repos.status(root) },
            },
        };
        let selected = self.diff.as_ref().map(|d| d.read(cx).file.path.clone());
        let narrow = self.git_width < px(git_panel::LEGEND_WIDTH);
        self.git_panel.update(cx, |p, cx| p.set(view, selected, narrow, cx));
    }

    pub fn refresh_repo(&self, root: &Path) {
        self.repos.refresh(root);
    }

    fn toggle_git_panel(&mut self, cx: &mut Context<Self>) {
        self.git_open = !self.git_open;
        self.flush_resize = true;
        self.mark_dirty(cx);
        cx.notify();
    }

    /// Clicking a file: opens its diff over the terminal; clicking the open one again closes it.
    pub fn toggle_diff(&mut self, root: PathBuf, file: git::FileChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = &self.diff {
            let d = d.read(cx);
            if d.root == root && d.file.path == file.path {
                self.close_diff(window, cx);
                return;
            }
        }
        let ws = cx.entity().downgrade();
        let lh = self.metrics.line_height;
        let view = cx.new(|cx| DiffView::new(root.clone(), file, lh, ws, cx));
        if self.diff.is_none()
            && let Some(term) = self.tabs.get(self.active).and_then(|t| t.view.as_ref())
        {
            term.read(cx).session.set_visible(false); // not on screen: wakes only for the unread flip
        }
        self.diff = Some(view.clone());
        self.diff_seen = self.repos.refreshes(&root);
        self.load_diff(cx);
        window.focus(&view.focus_handle(cx), cx);
        self.sync_panel(cx);
        cx.notify();
    }

    /// Esc in the diff: back to the terminal.
    pub fn close_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.drop_diff() {
            return;
        }
        if let Some(view) = self.tabs.get(self.active).and_then(|t| t.view.clone()) {
            view.read(cx).session.set_visible(true);
            view.update(cx, |_, cx| cx.notify());
            if self.modal.is_none() {
                window.focus(&view.read(cx).focus_handle(cx), cx);
            }
        }
        self.sync_panel(cx);
        cx.notify();
    }

    /// Forgets the diff view and cancels its read; returns whether one was open.
    fn drop_diff(&mut self) -> bool {
        self.diff_gen.fetch_add(1, Ordering::Relaxed);
        self._diff_task = None;
        self.diff.take().is_some()
    }

    /// (Re-)reads the open diff off the UI thread, with the file's current status entry.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.diff.clone() else { return };
        let (root, old) = {
            let v = view.read(cx);
            (v.root.clone(), v.file.clone())
        };
        let Some(repo) = self.repos.repo(&root).cloned() else { return };
        let file = self
            .repos
            .status(&root)
            .and_then(|s| s.files.iter().find(|f| f.path == old.path).cloned())
            .unwrap_or(old);
        let generation = self.diff_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let (current, current2) = (self.diff_gen.clone(), self.diff_gen.clone());
        let read_file = file.clone();
        self._diff_task = Some(cx.spawn(async move |_, cx| {
            let started = Instant::now();
            let lines = cx
                .background_executor()
                .spawn(async move {
                    git::diff(&repo, &read_file, || current.load(Ordering::Relaxed) != generation).map(DiffView::prepare)
                })
                .await;
            if current2.load(Ordering::Relaxed) != generation {
                return; // a newer read or a close happened meanwhile
            }
            crate::perf::event_with(|| {
                let n = lines.as_ref().map_or(0, |(l, _)| l.len());
                format!("diff read: {n} lines in {:.1} ms", started.elapsed().as_secs_f64() * 1e3)
            });
            view.update(cx, |v, cx| v.set_lines(lines, file, cx));
        }));
    }
}

impl Workspace {
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.dragging {
            self.dragging = false;
            self.flush_resize = true;
            self.mark_dirty(cx);
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

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// "2 h 14 m", "3 d 4 h", "14 m"; minute granularity (the countdown re-renders once a minute).
fn fmt_remaining(secs: i64) -> String {
    let m = (secs.max(0) + 59) / 60;
    let (d, h, m) = (m / 1440, m / 60 % 24, m % 60);
    match (d, h) {
        (0, 0) => format!("{m} m"),
        (0, _) => format!("{h} h {m} m"),
        _ => format!("{d} d {h} h"),
    }
}

fn limit_row(label: &'static str, limit: Option<hook::Limit>, now: i64) -> impl IntoElement {
    let (pct, detail) = match limit {
        Some(l) if l.resets_at > now => (l.used_pct, format!("{:.0}% · resets in {}", l.used_pct, fmt_remaining(l.resets_at - now))),
        Some(_) => (0., "reset".to_string()),
        None => (0., "—".to_string()),
    };
    let color = if pct >= 90. { colors::EXITED } else if pct >= 70. { colors::NEEDS_YOU } else { colors::WORKING };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .text_xs()
                .child(div().flex_1().child(label))
                .child(div().text_color(rgb(colors::DIM)).child(detail)),
        )
        .child(
            div()
                .h(px(3.))
                .w_full()
                .rounded_full()
                .bg(rgb(colors::BAR_BG))
                .child(div().h_full().rounded_full().bg(rgb(color)).w(gpui::relative(pct.clamp(0., 100.) / 100.))),
        )
}

fn window_state(window: &Window) -> Option<WindowState> {
    let (mode, b) = match window.window_bounds() {
        gpui::WindowBounds::Windowed(b) => (WindowMode::Windowed, b),
        gpui::WindowBounds::Maximized(b) => (WindowMode::Maximized, b),
        gpui::WindowBounds::Fullscreen(b) => (WindowMode::Fullscreen, b),
    };
    let f = f32::from;
    Some(WindowState { mode, x: f(b.origin.x), y: f(b.origin.y), w: f(b.size.width), h: f(b.size.height) })
}

/// What resuming a tab types (also shown in its banner): saved flags minus prompt, session-starting and
/// permission-bypassing ones (`hook::resume_args`).
fn resume_command(t: &Tab) -> Option<String> {
    let mut cmd = format!("claude --resume {}", sh_word(t.claude_session.as_ref()?));
    for a in hook::resume_args(&t.claude_args) {
        cmd.push(' ');
        cmd.push_str(&sh_word(&a));
    }
    Some(cmd)
}

/// Quotes one shell word for typing into zsh/sh.
fn sh_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_remaining, sh_word};

    #[test]
    fn shell_words() {
        assert_eq!(sh_word("--model"), "--model");
        assert_eq!(sh_word("my session"), "'my session'");
        assert_eq!(sh_word("it's"), r"'it'\''s'");
        assert_eq!(sh_word(""), "''");
    }

    #[test]
    fn remaining() {
        assert_eq!(fmt_remaining(0), "0 m");
        assert_eq!(fmt_remaining(61), "2 m");
        assert_eq!(fmt_remaining(2 * 3600 + 14 * 60), "2 h 14 m");
        assert_eq!(fmt_remaining(3 * 86400 + 4 * 3600), "3 d 4 h");
    }
}
