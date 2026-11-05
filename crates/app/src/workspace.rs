//! Root view: session list, sidebar, the active terminal (cached view), modal, resize policy.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Context, Entity, EntityId, FocusHandle, Focusable, KeyDownEvent, Pixels, Render, Task, Window, actions, div,
    prelude::*, px,
};
use session::{Session, Size};

use crate::agent::{self, Hub};
use crate::file_tree::{self, Marks, Row};
use crate::files_panel::{Content, FileView, FilesPanel, TreeData};
use crate::git_panel::{self, DiffView, GitPanel, PanelView, Repos};
use crate::line_edit::{Edit, LineEdit};
use crate::persist::{self, PanelTab, State as Saved, TabState, WindowMode, WindowState};
use crate::window_chrome::{self, Chrome};
use crate::terminal_element::Metrics;
use crate::terminal_view::{Banner, PADDING, TerminalView};

actions!(
    agent_ide,
    [
        NewSession, CloseSession, NextSession, PrevSession, Rename, ToggleSidebar, NextNeedsYou, ResumeAll, ShowChanges, ShowFiles, ToggleDiffMode,
        Activate1, Activate2, Activate3, Activate4, Activate5, Activate6, Activate7, Activate8, Activate9
    ]
);

const DIVIDER: f32 = 4.;
const COLLAPSED: f32 = 10.;
const MIN_SIDEBAR: f32 = 180.;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

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
    /// File tree: expanded directories (persisted) and the selection.
    tree: file_tree::TreeState,
    /// Sidebar group of a tab that isn't spawned yet (nearest ancestor with `.git`; no git call, no watcher).
    group_hint: Option<PathBuf>,
}

/// What covers the terminal area: a diff or a file preview. The active session is hidden meanwhile.
#[derive(Clone)]
enum Overlay {
    Diff(Entity<DiffView>),
    File(Entity<FileView>),
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
    overlay: Option<Overlay>,
    _overlay_task: Option<Task<()>>,
    /// Bumped per overlay read: a stale read sees it changed (a `git diff` is killed) and is dropped.
    overlay_gen: Arc<AtomicU64>,
    /// The overlay's repo refresh count when it was last read.
    overlay_seen: u64,
    /// Set (non-zero) to abort the in-flight syntax parse of the overlay; replaced per load.
    overlay_cancel: Arc<AtomicUsize>,
    /// Highlighted files by content hash (with their parse trees), shared by diff and preview loads.
    syntax_cache: Arc<syntax::Cache>,
    files_panel: Entity<FilesPanel>,
    panel_tab: PanelTab,
    show_ignored: bool,
    /// Unified or split diffs (saved); split falls back to unified while the main area is narrow.
    diff_mode: persist::DiffMode,
    split_style: persist::SplitStyle,
    /// Directory listings shown by any spawned tab's tree.
    tree_cache: file_tree::Cache,
    /// Listing requests in flight: directory → request id (only the newest applies).
    tree_pending: HashMap<PathBuf, u64>,
    tree_req: u64,
    /// The active tab's flattened rows for that root; None = rebuild on the next sync.
    tree_rows: Option<(PathBuf, Arc<Vec<Row>>)>,
    /// Status marks for (root, status Arc pointer).
    tree_marks: Option<((PathBuf, usize), Arc<Marks>)>,
    /// Watches the shown directories of trees outside any repo (repos use their own watcher).
    dir_watcher: Option<git::DirWatcher>,
    dir_watched: HashSet<PathBuf>,
    /// Collapsed sidebar groups (repo roots; `""` = "Other"), persisted.
    collapsed: HashSet<PathBuf>,
    /// The compositor blurs behind the window: the sidebar is painted translucent.
    translucent: bool,
    /// The last render drew a "working" spinner (set during render, read by the spin tick).
    spinner_drawn: std::cell::Cell<bool>,
    _spin: Option<Task<()>>,
}

impl Workspace {
    pub fn new(saved: Saved, state_path: PathBuf, translucent: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let files_panel = {
            let ws = cx.entity().downgrade();
            cx.new(|cx| FilesPanel::new(ws, cx))
        };
        let dir_watcher = git::DirWatcher::start(repos.reporter())
            .map_err(|e| eprintln!("agent-ide: no directory watcher for file trees: {e}"))
            .ok();
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            focus: cx.focus_handle(),
            sidebar_width: px(saved.sidebar.width.clamp(MIN_SIDEBAR, 2000.)),
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
            overlay: None,
            _overlay_task: None,
            overlay_gen: Arc::new(AtomicU64::new(0)),
            overlay_seen: 0,
            overlay_cancel: Default::default(),
            syntax_cache: Default::default(),
            files_panel,
            panel_tab: saved.git_panel.tab,
            show_ignored: saved.git_panel.show_ignored,
            diff_mode: saved.diff_mode,
            split_style: saved.split_style,
            tree_cache: Default::default(),
            tree_pending: HashMap::new(),
            tree_req: 0,
            tree_rows: None,
            tree_marks: None,
            dir_watcher,
            dir_watched: HashSet::new(),
            collapsed: saved.collapsed_groups.iter().cloned().collect(),
            translucent,
            spinner_drawn: Default::default(),
            _spin: None,
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
                let i = this.add_tab(None, dir, None, Vec::new(), Vec::new());
                let view = this.ensure_spawned(i, window, cx);
                if !line.is_empty() {
                    view.read(cx).session.write(format!("{line}\r").as_bytes());
                }
            }
            this.activate(0, window, cx);
        } else if saved.tabs.is_empty() {
            let dir = this.last_dir.clone();
            this.add_tab(None, dir, None, Vec::new(), Vec::new());
            this.activate(0, window, cx);
        } else {
            // Restore: rows for every tab, a shell only for the active one (the rest spawn on first view).
            for t in saved.tabs {
                this.add_tab(Some(t.name), t.cwd, t.claude_session, t.claude_args, t.expanded);
            }
            let active = saved.active.min(this.tabs.len() - 1);
            this.activate(active, window, cx);
            this.check_resumable(cx);
            this.hint_groups(cx);
        }

        if let Ok(path) = std::env::var("AGENT_IDE_PERF_DIFF") {
            // Perf harness: open that file's diff in the active tab's repo, then jump 40 rows every 50 ms
            // (AGENT_IDE_PERF_STEPS times, default 100).
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
                    let rows = this
                        .update(cx, |w, cx| match &w.overlay {
                            Some(Overlay::Diff(d)) => d.read(cx).loaded_rows(),
                            _ => 0,
                        })
                        .unwrap_or(0);
                    if rows > 0 || started.elapsed() > Duration::from_secs(10) {
                        eprintln!("[perf] diff open → rows ready {:.0} ms ({} rows)", started.elapsed().as_secs_f64() * 1e3, rows);
                        break;
                    }
                }
                if !syntax_off() {
                    while started.elapsed() < Duration::from_secs(5) {
                        let colored = this
                            .update(cx, |w, cx| matches!(&w.overlay, Some(Overlay::Diff(d)) if d.read(cx).colored()))
                            .unwrap_or(false);
                        if colored {
                            eprintln!("[perf] diff colored {:.0} ms", started.elapsed().as_secs_f64() * 1e3);
                            break;
                        }
                        timer(5).await;
                    }
                }
                eprintln!("[perf] diff scroll start");
                let steps = std::env::var("AGENT_IDE_PERF_STEPS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
                for step in 1..=steps {
                    timer(50).await;
                    let _ = this.update(cx, |w, cx| {
                        if let Some(Overlay::Diff(d)) = &w.overlay {
                            d.update(cx, |d, cx| d.scroll_to(step * 40, cx));
                        }
                    });
                }
                eprintln!("[perf] diff scroll end");
            })
            .detach();
        }

        if let Some(ms) = std::env::var("AGENT_IDE_PERF_MODAL").ok().and_then(|v| v.parse().ok()) {
            // Screenshot harness: open the new-session modal after `ms` milliseconds.
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| w.open_new_session(window, cx));
            })
            .detach();
        }

        if let Ok(path) = std::env::var("AGENT_IDE_PERF_OPEN") {
            // Perf/screenshot harness: open that file's preview once the active tab's repo is resolved.
            cx.spawn_in(window, async move |this, cx| {
                for _ in 0..100 {
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    let done = this.update_in(cx, |w, window, cx| {
                        let ready = w.tabs.get(w.active).and_then(Self::tree_root).is_some();
                        if ready {
                            w.open_file(PathBuf::from(&path), false, window, cx);
                        }
                        ready
                    });
                    if !matches!(done, Ok(false)) {
                        break;
                    }
                }
                if let Some(steps) = std::env::var("AGENT_IDE_PERF_STEPS").ok().and_then(|n| n.parse::<usize>().ok()) {
                    // Scroll harness: wait for the rows and the colors, then jump 40 rows every 50 ms.
                    let started = Instant::now();
                    let (mut rows_at, mut colored_at) = (None, None);
                    while started.elapsed() < Duration::from_secs(5) && colored_at.is_none() {
                        cx.background_executor().timer(Duration::from_millis(5)).await;
                        let (rows, colored) = this
                            .update(cx, |w, cx| match &w.overlay {
                                Some(Overlay::File(f)) => (f.read(cx).loaded_rows(), f.read(cx).colored()),
                                _ => (0, false),
                            })
                            .unwrap_or((0, false));
                        if rows > 0 && rows_at.is_none() {
                            rows_at = Some(started.elapsed());
                        }
                        if colored {
                            colored_at = Some(started.elapsed());
                        }
                    }
                    let ms = |d: Option<Duration>| d.map_or("-".into(), |d| format!("{:.0} ms", d.as_secs_f64() * 1e3));
                    eprintln!("[perf] preview rows ready {}, colored {}", ms(rows_at), ms(colored_at));
                    eprintln!("[perf] preview scroll start");
                    for step in 1..=steps {
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        let _ = this.update(cx, |w, cx| {
                            if let Some(Overlay::File(f)) = &w.overlay {
                                f.update(cx, |f, cx| f.scroll_to(step * 40, cx));
                            }
                        });
                    }
                    eprintln!("[perf] preview scroll end");
                }
                if std::env::var_os("AGENT_IDE_PERF_OPEN_END").is_some() {
                    for _ in 0..100 {
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        let done = this.update(cx, |w, cx| match &w.overlay {
                            Some(Overlay::File(f)) => f.update(cx, |f, cx| f.scroll_to_end(cx)),
                            _ => false,
                        });
                        if !matches!(done, Ok(false)) {
                            break;
                        }
                    }
                }
                eprintln!("[perf] opened");
            })
            .detach();
        }

        if let Ok(dir) = std::env::var("AGENT_IDE_PERF_TREE") {
            // Perf harness: show Files, expand that directory (and its parents) in the active tab, then jump 40 rows
            // every 50 ms (AGENT_IDE_PERF_STEPS times, default 100).
            let dir = PathBuf::from(dir);
            cx.spawn_in(window, async move |this, cx| {
                let bg = cx.background_executor().clone();
                let timer = |ms| bg.timer(Duration::from_millis(ms));
                let mut started = None;
                for _ in 0..200 {
                    timer(20).await;
                    let state = this.update_in(cx, |w, window, cx| {
                        let root = w.tabs.get(w.active).and_then(Self::tree_root)?;
                        if !w.tree_cache.contains_key(&root) {
                            return None;
                        }
                        if started.is_none() {
                            w.panel_tab = PanelTab::Files;
                            w.git_open = true;
                            let dirs: Vec<PathBuf> =
                                dir.ancestors().take_while(|a| a.starts_with(&root) && *a != root).map(Path::to_path_buf).collect();
                            started = Some(Instant::now());
                            for d in dirs.into_iter().rev() {
                                w.tree_action(file_tree::Action::Expand(d), window, cx);
                            }
                        }
                        let rows = w.tree_rows.as_ref().map_or(0, |(_, r)| r.len());
                        w.tree_cache.contains_key(&dir).then_some(rows)
                    });
                    if let Ok(Some(rows)) = state
                        && let Some(t) = started
                    {
                        eprintln!("[perf] tree expand → rows ready {:.1} ms ({rows} rows)", t.elapsed().as_secs_f64() * 1e3);
                        break;
                    }
                }
                let steps = std::env::var("AGENT_IDE_PERF_STEPS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
                eprintln!("[perf] tree scroll start");
                for step in 1..=steps {
                    timer(50).await;
                    let _ = this.update(cx, |w, cx| w.files_panel.update(cx, |p, cx| p.scroll_to(step * 40, cx)));
                }
                eprintln!("[perf] tree scroll end");
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
        // Slim session header above the terminal and the status bar below everything.
        let bars = px(crate::theme::height::HEADER + crate::theme::height::STATUS_BAR);
        let h = vp.height - px(PADDING) - chrome.vertical() - bars;
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
        self.sync_diff_mode(cx);
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
    fn add_tab(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        claude_session: Option<String>,
        claude_args: Vec<String>,
        expanded: Vec<PathBuf>,
    ) -> usize {
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
            tree: file_tree::TreeState { expanded: expanded.into_iter().collect(), selected: None },
            group_hint: None,
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
        self.drop_overlay();
        self.tree_rows = None;
        if let Some(old) = self.tabs.get(self.active).and_then(|t| t.view.as_ref()) {
            old.read(cx).session.set_visible(false);
        }
        let view = self.ensure_spawned(idx, window, cx);
        // Showing a session reveals its group.
        let group = match &self.tabs[idx].repo {
            TabRepo::Repo(root) => root.clone(),
            _ => PathBuf::new(),
        };
        if self.collapsed.remove(&group) {
            self.mark_dirty(cx);
        }
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

    /// Ctrl+Tab / Ctrl+Shift+Tab, in the sidebar's (grouped) order.
    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.visual_order();
        let Some(pos) = order.iter().position(|&i| i == self.active) else { return };
        let next = order[(pos as isize + step).rem_euclid(order.len() as isize) as usize];
        self.activate(next, window, cx);
    }

    /// Ctrl+1..9: the n-th session as the sidebar lists it.
    fn activate_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&i) = self.visual_order().get(n) {
            self.activate(i, window, cx);
        }
    }

    /// Restored tabs that aren't spawned get their sidebar group from the nearest ancestor holding `.git` (stat
    /// calls off the UI thread), so groups are right before each tab's shell (and real repo lookup) starts.
    fn hint_groups(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<(usize, PathBuf)> = self.tabs.iter().map(|t| (t.id, t.cwd.clone())).collect();
        cx.spawn(async move |this, cx| {
            let hints: Vec<(usize, Option<PathBuf>)> = cx
                .background_executor()
                .spawn(async move {
                    dirs.into_iter()
                        .map(|(id, d)| (id, d.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)))
                        .collect()
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                for (id, hint) in hints {
                    if let Some(t) = w.tabs.iter_mut().find(|t| t.id == id) {
                        t.group_hint = hint;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sidebar groups: (repo root or None for "Other", tab indices), see `group_order`.
    fn groups(&self) -> Vec<(Option<PathBuf>, Vec<usize>)> {
        let keys: Vec<Option<PathBuf>> = self
            .tabs
            .iter()
            .map(|t| match &t.repo {
                TabRepo::Repo(root) => Some(root.clone()),
                TabRepo::Unknown | TabRepo::Pending => t.group_hint.clone(),
                TabRepo::NotRepo | TabRepo::GitMissing => None,
            })
            .collect();
        group_order(&keys)
    }

    fn visual_order(&self) -> Vec<usize> {
        self.groups().into_iter().flat_map(|(_, members)| members).collect()
    }

    fn toggle_group(&mut self, id: PathBuf, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.mark_dirty(cx);
        cx.notify();
    }

    /// The agent state shown for tab `i` (default when the integration is off).
    fn agent(&self, i: usize) -> agent::Agent {
        match (&self.agents, self.tabs.get(i)) {
            (Some(h), Some(t)) => h.board.lock().unwrap().get(t.id as u32),
            _ => agent::Agent::default(),
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
        self.drop_overlay();
        self.tree_rows = None;
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
                        let i = self.add_tab(None, dir, None, Vec::new(), Vec::new());
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
                    expanded: t.tree.expanded.iter().cloned().collect(),
                })
                .collect(),
            active: self.active.min(self.tabs.len().saturating_sub(1)),
            sidebar: persist::SidebarState { width: f32::from(self.sidebar_width), open: self.sidebar_open },
            window: self.window_state,
            last_dir: Some(self.last_dir.clone()),
            collapsed_groups: self.collapsed.iter().cloned().collect(),
            git_panel: persist::PanelState {
                width: f32::from(self.git_width),
                open: self.git_open,
                tab: self.panel_tab,
                show_ignored: self.show_ignored,
            },
            diff_mode: self.diff_mode,
            split_style: self.split_style,
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
        // Limits countdowns, "2m ago" and "Working · 3m" all have minute granularity.
        let shown = self.agents.as_ref().is_some_and(|h| {
            let b = h.board.lock().unwrap();
            b.five_hour.is_some() || b.seven_day.is_some() || b.tabs.values().any(|a| a.last_activity.is_some() || a.state_since.is_some())
        });
        if !shown || !self.window_visible || self._minute.is_some() {
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

}

mod view;

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
        if let Some(root) = self.overlay_repo(cx) {
            let n = self.repos.refreshes(&root);
            if n != self.overlay_seen {
                self.overlay_seen = n;
                self.load_overlay(cx);
            }
        }
        self.tree_dirs_changed(cx);
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
        let selected = match &self.overlay {
            Some(Overlay::Diff(d)) => Some(d.read(cx).file.path.clone()),
            _ => None,
        };
        let narrow = self.git_width < px(git_panel::LEGEND_WIDTH);
        self.git_panel.update(cx, |p, cx| p.set(view, selected, narrow, cx));
        self.sync_files(false, cx);
    }

    pub fn refresh_repo(&self, root: &Path) {
        self.repos.refresh(root);
    }

    /// Clicking a file in Changes (or "Show diff"): opens its diff over the terminal; the open one again closes it.
    pub fn toggle_diff(&mut self, root: PathBuf, file: git::FileChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Diff(d)) = &self.overlay {
            let d = d.read(cx);
            if d.root == root && d.file.path == file.path {
                self.close_overlay(window, cx);
                return;
            }
        }
        let ws = cx.entity().downgrade();
        let lh = self.metrics.line_height;
        let mode = (self.diff_mode, self.split_style, self.diff_narrow(window));
        let view = cx.new(|cx| DiffView::new(root, file, lh, mode, ws, cx));
        self.show_overlay(Overlay::Diff(view), window, cx);
    }

    fn diff_narrow(&self, window: &Window) -> bool {
        self.grid_size(window).cols < git_panel::MIN_SPLIT_COLS
    }

    /// Ctrl+Shift+D (only while a diff is open) and the diff header's switch.
    pub fn toggle_diff_mode(&mut self, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Diff(_))) {
            let mode = match self.diff_mode {
                persist::DiffMode::Unified => persist::DiffMode::Split,
                persist::DiffMode::Split => persist::DiffMode::Unified,
            };
            self.set_diff_mode(mode, cx);
        }
    }

    pub fn set_diff_mode(&mut self, mode: persist::DiffMode, cx: &mut Context<Self>) {
        if mode != self.diff_mode {
            self.diff_mode = mode;
            self.mark_dirty(cx);
            self.sync_diff_mode(cx);
        }
    }

    pub fn set_split_style(&mut self, style: persist::SplitStyle, cx: &mut Context<Self>) {
        if style != self.split_style {
            self.split_style = style;
            self.mark_dirty(cx);
            self.sync_diff_mode(cx);
        }
    }

    /// Hands the mode, the split style and the (settled) main-area width to the open diff; it re-renders only if either changed.
    fn sync_diff_mode(&self, cx: &mut Context<Self>) {
        if let Some(Overlay::Diff(v)) = &self.overlay {
            let narrow = self.applied.cols < git_panel::MIN_SPLIT_COLS;
            let (mode, style) = (self.diff_mode, self.split_style);
            v.update(cx, |v, cx| v.set_mode(mode, style, narrow, cx));
        }
    }

    /// Opens a file's preview over the terminal; with `toggle`, the open one closes instead.
    fn open_file(&mut self, path: PathBuf, toggle: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::File(f)) = &self.overlay
            && f.read(cx).path == path
        {
            if toggle {
                self.close_overlay(window, cx);
            }
            return;
        }
        let tab = self.tabs.get(self.active);
        let repo = match tab.map(|t| &t.repo) {
            Some(TabRepo::Repo(root)) if path.starts_with(root) => Some(root.clone()),
            _ => None,
        };
        let base = tab.and_then(Self::tree_root).filter(|r| path.starts_with(r));
        let ws = cx.entity().downgrade();
        let m = (self.metrics.line_height, self.metrics.cell_width);
        let view = cx.new(|cx| FileView::new(path, repo, base, m, ws, cx));
        self.show_overlay(Overlay::File(view), window, cx);
    }

    fn show_overlay(&mut self, overlay: Overlay, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_none()
            && let Some(term) = self.tabs.get(self.active).and_then(|t| t.view.as_ref())
        {
            term.read(cx).session.set_visible(false); // not on screen: wakes only for the unread flip
        }
        self.drop_overlay();
        let focus = match &overlay {
            Overlay::Diff(v) => v.focus_handle(cx),
            Overlay::File(v) => v.focus_handle(cx),
        };
        self.overlay = Some(overlay);
        self.overlay_seen = self.overlay_repo(cx).map_or(0, |r| self.repos.refreshes(&r));
        self.load_overlay(cx);
        window.focus(&focus, cx);
        self.sync_panel(cx);
        cx.notify();
    }

    /// The repo whose refreshes re-read the open overlay.
    fn overlay_repo(&self, cx: &gpui::App) -> Option<PathBuf> {
        match self.overlay.as_ref()? {
            Overlay::Diff(d) => Some(d.read(cx).root.clone()),
            Overlay::File(f) => f.read(cx).repo.clone(),
        }
    }

    /// Esc in a diff or preview: back to the terminal.
    pub fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.drop_overlay() {
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

    /// Forgets the overlay and cancels its read; returns whether one was open.
    fn drop_overlay(&mut self) -> bool {
        self.overlay_gen.fetch_add(1, Ordering::Relaxed);
        self.overlay_cancel.store(1, Ordering::Relaxed);
        self._overlay_task = None;
        self.overlay.take().is_some()
    }

    /// (Re-)reads the open diff or file off the UI thread.
    fn load_overlay(&mut self, cx: &mut Context<Self>) {
        let generation = self.overlay_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let (current, current2) = (self.overlay_gen.clone(), self.overlay_gen.clone());
        self.overlay_cancel.store(1, Ordering::Relaxed);
        self.overlay_cancel = Default::default();
        let (cancel, cache) = (self.overlay_cancel.clone(), self.syntax_cache.clone());
        match self.overlay.clone() {
            None => {}
            Some(Overlay::Diff(view)) => {
                let (root, old, prev) = {
                    let v = view.read(cx);
                    (v.root.clone(), v.file.clone(), v.layout())
                };
                let Some(repo) = self.repos.repo(&root).cloned() else { return };
                let file = self
                    .repos
                    .status(&root)
                    .and_then(|s| s.files.iter().find(|f| f.path == old.path).cloned())
                    .unwrap_or(old);
                let read_file = file.clone();
                let syn_file = file.clone();
                let syn_repo = repo.clone();
                self._overlay_task = Some(cx.spawn(async move |_, cx| {
                    let started = Instant::now();
                    let lines = cx
                        .background_executor()
                        .spawn(async move {
                            let diff = git::diff(&repo, &read_file, || current.load(Ordering::Relaxed) != generation)?;
                            // Same diff as shown: keep its layout (no work, and the view sees no change).
                            let hash = git::layout::hash(&diff);
                            if let Some(prev) = prev.filter(|p| p.hash == hash) {
                                return Some((prev, None));
                            }
                            let t = Instant::now();
                            let layout = Arc::new(git::layout::layout(&diff));
                            Some((layout, Some(t.elapsed())))
                        })
                        .await;
                    let plain = lines.as_ref().filter(|(l, _)| !l.colored).map(|(l, _)| l.clone());
                    if current2.load(Ordering::Relaxed) != generation {
                        return; // a newer read or a close happened meanwhile
                    }
                    crate::perf::event_with(|| match &lines {
                        Some((l, Some(took))) => format!(
                            "diff read: {} lines in {:.1} ms (layout {:.1} ms)",
                            l.lines.len(),
                            started.elapsed().as_secs_f64() * 1e3,
                            took.as_secs_f64() * 1e3
                        ),
                        Some((_, None)) => "diff read: unchanged, layout reused".into(),
                        None => "diff read failed".into(),
                    });
                    view.update(cx, |v, cx| v.set_layout(lines.map(|(l, _)| l), file.clone(), cx));
                    // Second pass: syntax colors (both sides parsed as whole files), then one more publish.
                    let Some(plain) = plain.filter(|_| !syntax_off()) else { return };
                    let started = Instant::now();
                    let colored = cx
                        .background_executor()
                        .spawn(async move { color_diff(&plain, &syn_repo, &syn_file, &cache, &cancel).map(Arc::new) })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    crate::perf::event_with(|| {
                        format!("diff syntax: {} in {:.1} ms", if colored.is_some() { "colored" } else { "plain" }, started.elapsed().as_secs_f64() * 1e3)
                    });
                    if let Some(colored) = colored {
                        view.update(cx, |v, cx| v.set_layout(Some(colored), file, cx));
                    }
                }));
            }
            Some(Overlay::File(view)) => {
                let (path, repo) = {
                    let v = view.read(cx);
                    (v.path.clone(), v.repo.clone())
                };
                let change = repo.as_ref().and_then(|root| {
                    let rel = path.strip_prefix(root).ok()?.to_str()?.to_string();
                    self.repos.status(root)?.files.iter().find(|f| f.path == rel).cloned()
                });
                self._overlay_task = Some(cx.spawn(async move |_, cx| {
                    let started = Instant::now();
                    let lang = syntax::detect(&path);
                    let read = cx
                        .background_executor()
                        .spawn(async move { git::text::read_file(&path).ok().map(Content::prepare) })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    crate::perf::event_with(|| format!("file read in {:.1} ms", started.elapsed().as_secs_f64() * 1e3));
                    let (content, raw) = match read {
                        Some((c, raw)) => (Some(c), raw),
                        None => (None, None),
                    };
                    view.update(cx, |v, cx| v.set_content(content, change, cx));
                    let (Some(lang), Some(raw), false) = (lang, raw, syntax_off()) else { return };
                    let started = Instant::now();
                    let colors = cx
                        .background_executor()
                        .spawn(async move { cache.get(lang, &raw, &cancel).ok().map(|h| Content::colors(&raw, &h)) })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    crate::perf::event_with(|| {
                        format!("file syntax: {} in {:.1} ms", if colors.is_some() { "colored" } else { "plain" }, started.elapsed().as_secs_f64() * 1e3)
                    });
                    if let Some(colors) = colors {
                        view.update(cx, |v, cx| v.set_colors(colors, cx));
                    }
                }));
            }
        }
    }
}

/// File tree: listings (shared cache), per-tab expanded state, watching, the Files tab.
impl Workspace {
    /// The tree's root for a tab: its repo root, else its cwd (None while the repo is being resolved).
    fn tree_root(t: &Tab) -> Option<PathBuf> {
        match &t.repo {
            TabRepo::Repo(root) => Some(root.clone()),
            TabRepo::NotRepo | TabRepo::GitMissing => Some(if t.dir_missing { home() } else { t.cwd.clone() }),
            TabRepo::Unknown | TabRepo::Pending => None,
        }
    }

    /// Brings listings and watches in line with what the spawned tabs' trees show: lists missing directories,
    /// evicts unused ones, and (outside repos) watches exactly the shown directories that aren't ignored.
    fn sync_tree_wants(&mut self, cx: &mut Context<Self>) {
        let mut wanted: HashSet<PathBuf> = HashSet::new();
        let mut watch: HashSet<PathBuf> = HashSet::new();
        for t in self.tabs.iter().filter(|t| t.view.is_some()) {
            let Some(root) = Self::tree_root(t) else { continue };
            let dirs = file_tree::wanted(&root, &t.tree);
            if !matches!(t.repo, TabRepo::Repo(_)) {
                watch.extend(dirs.iter().cloned());
            }
            wanted.extend(dirs);
        }
        let before = self.tree_cache.len();
        self.tree_cache.retain(|d, _| wanted.contains(d));
        if self.tree_cache.len() != before {
            self.tree_rows = None;
        }
        self.tree_pending.retain(|d, _| wanted.contains(d));
        let missing: Vec<PathBuf> =
            wanted.into_iter().filter(|d| !self.tree_cache.contains_key(d) && !self.tree_pending.contains_key(d)).collect();
        for d in missing {
            self.request_listing(d, cx);
        }
        if let Some(dw) = &self.dir_watcher {
            for d in self.dir_watched.difference(&watch) {
                dw.unwatch(d);
            }
            for d in watch.difference(&self.dir_watched) {
                dw.watch(d);
            }
        }
        self.dir_watched = watch;
    }

    /// Lists one directory off the UI thread; only the newest request per directory applies.
    fn request_listing(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.tree_req += 1;
        let id = self.tree_req;
        self.tree_pending.insert(dir.clone(), id);
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let d = dir.clone();
            let result = cx.background_executor().spawn(async move { git::list_dir(&d) }).await;
            let _ = this.update(cx, |w, cx| {
                if w.tree_pending.get(&dir) != Some(&id) {
                    return; // superseded, or no longer shown
                }
                w.tree_pending.remove(&dir);
                match result {
                    Ok(listing) => {
                        crate::perf::event_with(|| {
                            format!("listed {} ({} entries) in {:.1} ms", dir.display(), listing.entries.len(), started.elapsed().as_secs_f64() * 1e3)
                        });
                        if w.tree_cache.get(&dir).is_some_and(|l| **l == listing) {
                            return;
                        }
                        w.tree_cache.insert(dir, Arc::new(listing));
                    }
                    Err(_) => {
                        // Gone (deleted, or a saved expanded dir that no longer exists): forget it everywhere.
                        w.tree_cache.remove(&dir);
                        let mut forgot = false;
                        for t in &mut w.tabs {
                            forgot |= t.tree.expanded.remove(&dir);
                        }
                        if forgot {
                            w.mark_dirty(cx);
                        }
                    }
                }
                w.tree_rows = None;
                w.sync_files(false, cx);
            });
        })
        .detach();
    }

    /// Watchers reported changed directories: re-list the ones some tree shows.
    fn tree_dirs_changed(&mut self, cx: &mut Context<Self>) {
        for d in self.repos.take_dirty_dirs() {
            if self.tree_cache.contains_key(&d) {
                self.request_listing(d, cx);
            }
        }
    }

    /// Pushes the active tab's tree into the Files panel; rows are rebuilt only after something invalidated them.
    fn sync_files(&mut self, reveal: bool, cx: &mut Context<Self>) {
        self.sync_tree_wants(cx);
        let Some(t) = self.tabs.get(self.active) else {
            self.files_panel.update(cx, |p, cx| p.set(TreeData::default(), false, cx));
            return;
        };
        let root = Self::tree_root(t);
        let rows = match (&root, &self.tree_rows) {
            (Some(r), Some((rr, rows))) if rr == r => rows.clone(),
            (Some(r), _) => {
                let started = Instant::now();
                let rows = Arc::new(file_tree::flatten(r, &t.tree, &self.tree_cache, self.show_ignored));
                crate::perf::event_with(|| format!("tree flatten: {} rows in {:.2} ms", rows.len(), started.elapsed().as_secs_f64() * 1e3));
                self.tree_rows = Some((r.clone(), rows.clone()));
                rows
            }
            (None, _) => Arc::default(),
        };
        let marks = match &t.repo {
            TabRepo::Repo(r) => match self.repos.status(r) {
                Some(st) => {
                    let key = (r.clone(), Arc::as_ptr(&st) as usize);
                    match &self.tree_marks {
                        Some((k, m)) if *k == key => m.clone(),
                        _ => {
                            let m = Arc::new(file_tree::marks(r, &st));
                            self.tree_marks = Some((key, m.clone()));
                            m
                        }
                    }
                }
                None => Arc::default(),
            },
            _ => Arc::default(),
        };
        let open = match &self.overlay {
            Some(Overlay::File(f)) => Some(f.read(cx).path.clone()),
            _ => None,
        };
        let data = TreeData {
            loading: root.as_ref().is_some_and(|r| !self.tree_cache.contains_key(r)),
            root,
            rows,
            marks,
            selected: t.tree.selected.clone(),
            open,
            show_ignored: self.show_ignored,
        };
        self.files_panel.update(cx, |p, cx| p.set(data, reveal, cx));
    }

    fn change_tree(&mut self, f: impl FnOnce(&mut file_tree::TreeState), cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let before = t.tree.expanded.len();
        f(&mut t.tree);
        if t.tree.expanded.len() != before {
            self.tree_rows = None;
            self.mark_dirty(cx);
        }
    }

    /// Keyboard in the Files panel.
    pub fn tree_key(&mut self, key: file_tree::Key, window: &mut Window, cx: &mut Context<Self>) {
        let action = {
            let Some(t) = self.tabs.get(self.active) else { return };
            let rows = self.tree_rows.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
            file_tree::key(&rows, t.tree.selected.as_deref(), key)
        };
        self.tree_action(action, window, cx);
    }

    fn tree_action(&mut self, action: file_tree::Action, window: &mut Window, cx: &mut Context<Self>) {
        use file_tree::Action;
        match action {
            Action::None => return,
            Action::Select(p) => self.change_tree(|t| t.selected = Some(p), cx),
            Action::Expand(p) => {
                self.change_tree(|t| {
                    t.expanded.insert(p.clone());
                }, cx);
                self.request_listing(p, cx); // fresh on every expand (ignored dirs are never watched)
            }
            Action::Collapse(p) => self.change_tree(|t| {
                t.expanded.remove(&p);
            }, cx),
            Action::Open(p) => {
                let focus = self.files_panel.focus_handle(cx);
                self.open_file(p, false, window, cx);
                window.focus(&focus, cx); // keep navigating the tree; Esc there goes to the preview's terminal
            }
        }
        self.sync_files(true, cx);
    }

    /// Mouse in the Files panel: select; a directory toggles, a file opens (the open one closes).
    pub fn tree_click(&mut self, path: PathBuf, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.change_tree(|t| t.selected = Some(path.clone()), cx);
        let expanded = self.tabs.get(self.active).is_some_and(|t| t.tree.expanded.contains(&path));
        match (is_dir, expanded) {
            (true, true) => self.tree_action(file_tree::Action::Collapse(path), window, cx),
            (true, false) => self.tree_action(file_tree::Action::Expand(path), window, cx),
            (false, _) => {
                self.open_file(path, true, window, cx);
                self.sync_files(false, cx);
            }
        }
    }

    pub fn toggle_show_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        self.tree_rows = None;
        self.mark_dirty(cx);
        self.sync_files(false, cx);
    }

    /// Esc in the tree: back to whatever the main area shows.
    pub fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = match &self.overlay {
            Some(Overlay::Diff(v)) => Some(v.focus_handle(cx)),
            Some(Overlay::File(v)) => Some(v.focus_handle(cx)),
            None => self.tabs.get(self.active).and_then(|t| t.view.as_ref()).map(|v| v.focus_handle(cx)),
        };
        if let Some(f) = focus {
            window.focus(&f, cx);
        }
    }

    /// Ctrl+Shift+G / Ctrl+Shift+E: show that tab (Files also takes focus); again closes the panel.
    fn show_panel_tab(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        let files_focused = self.files_panel.focus_handle(cx).is_focused(window);
        let shown = self.git_open && self.panel_tab == tab;
        if shown && (tab == PanelTab::Changes || files_focused) {
            self.git_open = false;
            self.flush_resize = true;
            self.focus_main(window, cx);
        } else {
            if !self.git_open {
                self.git_open = true;
                self.flush_resize = true;
            }
            self.panel_tab = tab;
            if tab == PanelTab::Files {
                let f = self.files_panel.focus_handle(cx);
                window.focus(&f, cx);
            }
        }
        self.mark_dirty(cx);
        cx.notify();
    }

    fn set_panel_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.panel_tab != tab {
            self.panel_tab = tab;
            self.mark_dirty(cx);
            cx.notify();
        }
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

/// Groups in order of their first session; "Other" (None) last. Sessions keep their order within a group.
fn group_order(keys: &[Option<PathBuf>]) -> Vec<(Option<PathBuf>, Vec<usize>)> {
    let mut groups: Vec<(Option<PathBuf>, Vec<usize>)> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        match groups.iter_mut().find(|(g, _)| g == k) {
            Some((_, members)) => members.push(i),
            None => groups.push((k.clone(), vec![i])),
        }
    }
    groups.sort_by_key(|(k, _)| k.is_none()); // stable: keeps first-appearance order otherwise
    groups
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

/// `AGENT_IDE_SYNTAX=off`: no syntax highlighting (escape hatch, and the plain baseline for perf runs).
fn syntax_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("AGENT_IDE_SYNTAX").is_ok_and(|v| v == "off"))
}

/// The diff's syntax colors: the old side from HEAD, the new side from the working tree, each highlighted as a
/// whole file (cached by content). None when the language is unknown or neither side could be highlighted.
fn color_diff(
    plain: &git::layout::Layout,
    repo: &git::Repo,
    file: &git::FileChange,
    cache: &syntax::Cache,
    cancel: &AtomicUsize,
) -> Option<git::layout::Layout> {
    let lang = syntax::detect(Path::new(&file.path))?;
    let old_text = (file.kind != '?')
        .then(|| git::head_text(repo, file.orig.as_deref().unwrap_or(&file.path), syntax::MAX_BYTES))
        .flatten();
    let new_text = match git::text::read_file(&repo.root.join(&file.path)) {
        Ok(git::text::FileText::Lines { raw, .. }) => Some(raw),
        _ => None,
    };
    // Both sides in parallel: a big file's two parses take about as long as one.
    let (old, new) = std::thread::scope(|s| {
        let old = s.spawn(|| old_text.as_deref().and_then(|t| cache.get(lang, t, cancel).ok()));
        let new = new_text.as_deref().and_then(|t| cache.get(lang, t, cancel).ok());
        (old.join().ok().flatten(), new)
    });
    if old.is_none() && new.is_none() {
        return None;
    }
    let old_side = old.as_ref().zip(old_text.as_deref()).map(|(h, t)| git::layout::Side::new(t, &h.lines));
    let new_side = new.as_ref().zip(new_text.as_deref()).map(|(h, t)| git::layout::Side::new(t, &h.lines));
    Some(plain.with_syntax(old_side.as_ref(), new_side.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::{fmt_remaining, group_order, sh_word};
    use std::path::PathBuf;

    /// Staged + unstaged edits: every colored row comes from the right side (HEAD for removed lines, the working
    /// tree for added/context lines), so its text is exactly that file line.
    #[test]
    fn diff_colors_come_from_head_and_worktree() {
        use std::process::Command;
        let d = std::env::temp_dir().join(format!("agent-ide-color-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        let head = "fn one() {\n    let a = \"x\";\n}\n\nfn two() {\n\tlet b = 2;\n}\n";
        std::fs::write(d.join("m.rs"), head).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(d.join("m.rs"), head.replace("\"x\"", "\"staged\"")).unwrap();
        git(&["add", "."]);
        let work = head.replace("\"x\"", "\"staged\"").replace("let b = 2;", "let b = 3; // unstaged");
        std::fs::write(d.join("m.rs"), &work).unwrap();

        let git::Discovery::Repo(repo) = git::discover(&d) else { panic!("repo") };
        let file = git::status(&repo).unwrap().files.into_iter().find(|f| f.path == "m.rs").unwrap();
        let diff = git::diff(&repo, &file, || false).unwrap();
        let plain = git::layout::layout(&diff);
        let colored = super::color_diff(&plain, &repo, &file, &syntax::Cache::default(), &Default::default()).unwrap();
        let shown = |t: &str, n: Option<u32>| git::text::display_line(t.lines().nth(n.unwrap() as usize - 1).unwrap(), 0);
        let mut checked = 0;
        for l in colored.lines.iter().filter(|l| l.runs.iter().any(|r| r.class.is_some())) {
            let expect = match l.kind {
                git::LineKind::Del => shown(head, l.old),
                _ => shown(&work, l.new),
            };
            assert_eq!(*l.text, expect, "{:?} row matches its side", l.kind);
            checked += 1;
        }
        assert!(checked >= 4, "colored rows: {checked}");
        let add = colored.lines.iter().find(|l| l.kind == git::LineKind::Add && l.text.contains("unstaged")).unwrap();
        let comment = syntax::Class::Comment as u8;
        assert!(add.runs.iter().any(|r| r.class == Some(comment)), "{:?}", add.runs);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn grouping_and_order() {
        let a = Some(PathBuf::from("/a"));
        let b = Some(PathBuf::from("/b"));
        let groups = group_order(&[None, b.clone(), a.clone(), b.clone(), None]);
        assert_eq!(groups, vec![(b, vec![1, 3]), (a, vec![2]), (None, vec![0, 4])]);
        let order: Vec<usize> = groups.into_iter().flat_map(|(_, m)| m).collect();
        assert_eq!(order, vec![1, 3, 2, 0, 4], "Ctrl+Tab / Ctrl+N follow this order");
    }

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
