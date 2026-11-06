//! Root view: session list, sidebar, the active terminal (cached view), modal, resize policy.

use crate::theme::{self as theme, radius, space, text_size, z};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Context, Entity, EntityId, FocusHandle, Focusable, KeyDownEvent, Render, SharedString, Task, Window, actions, div,
    prelude::*,
};
use session::{Session, Size};

use crate::agent::{self, Hub};
use crate::diff_syntax::{self, syntax_off};
use crate::file_tree::{self, Marks, Row};
use crate::files_panel::{Content, FileView, FilesPanel, TreeData};
use crate::git_panel::{self, DiffView, GitPanel, PanelView, Repos};
use crate::layout::{self, Dock, Panel};
use crate::line_edit::{Edit, LineEdit};
use crate::notify;
use crate::persist::{self, State as Saved, TabState, WindowMode, WindowState};
use crate::settings::{self, Settings};
use crate::terminal_element::Metrics;
use crate::terminal_view::{Banner, PADDING, TerminalView};
use crate::window_chrome::{self, Chrome};

actions!(
    dirigent,
    [
        NewSession,
        NewInProject,
        CloseSession,
        NextSession,
        PrevSession,
        Rename,
        ToggleSidebar,
        NextNeedsYou,
        ResumeAll,
        ShowChanges,
        ShowFiles,
        ToggleDiffMode,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        SessionMenu,
        QuickOpen,
        CommandPalette,
        FindInFiles,
        FindInTerminal,
        GitSync,
        GitFetch,
        GitPull,
        GitPush,
        SwitchBranch,
        Activate1,
        Activate2,
        Activate3,
        Activate4,
        Activate5,
        Activate6,
        Activate7,
        Activate8,
        Activate9
    ]
);

const DIVIDER: f32 = 4.;
const COLLAPSED: f32 = 10.;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// The second option of claude's folder trust dialog (2.1.318), answered when we start claude in a new session.
const TRUST_PROMPT: &str = "Yes, I trust this folder";

struct Tab {
    id: usize,
    /// A name the user set (rename, setup dialog): always shown, never replaced.
    custom: Option<String>,
    /// The automatic name (Claude's title or the first prompt); `Workspace::tab_title` resolves what is shown.
    auto: Option<agent::Auto>,
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
    /// Bumped per Esc/Ctrl-C typed while its agent works: only the newest interrupt check runs.
    interrupt_gen: u64,
    /// Claude Code's cwd from its last hook, as last applied (so each change is applied once).
    hook_cwd: Option<PathBuf>,
    /// File tree: expanded directories (persisted) and the selection.
    tree: file_tree::TreeState,
    /// Sidebar group of a tab that isn't spawned yet (nearest ancestor with `.git`; no git call, no watcher).
    group_hint: Option<PathBuf>,
    /// The agent's `notice_seq` last considered for a desktop notification.
    notified_seq: u64,
    /// Changes panel: This agent (Some(true)) / All; None = the default (This agent when an agent ran).
    agent_only: Option<bool>,
    /// Opened as a new session (picker, `+`, CLI): its project is remembered once its repo is resolved.
    remember: bool,
    /// A linked worktree's main checkout (its sidebar group, its project); also the hint for unspawned tabs.
    main_root: Option<PathBuf>,
}

/// Project defaults dialog: start claude, its flags, the setup command for new worktrees, where worktrees go.
pub(super) struct Defaults {
    pub root: PathBuf,
    pub auto: bool,
    pub flags: LineEdit,
    pub setup: LineEdit,
    pub wt_dir: LineEdit,
    /// 0 the checkbox, 1 flags, 2 setup, 3 worktree dir.
    pub focus: u8,
}

/// How `new_session_with` opens a session.
#[derive(Default)]
pub(super) struct SessionOpts {
    pub name: Option<String>,
    /// None: the project's default.
    pub claude: Option<bool>,
    pub prompt: Option<String>,
    /// Run the project's setup command first (a fresh worktree).
    pub setup: bool,
    /// The project whose defaults apply (None: the one containing the directory).
    pub project: Option<PathBuf>,
}

/// What covers the terminal area: a diff, a file preview or Settings. The active session is hidden meanwhile.
#[derive(Clone)]
enum Overlay {
    Diff(Entity<DiffView>),
    File(Entity<FileView>),
    Settings(Entity<crate::settings_view::SettingsView>),
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
    /// New session: the project picker (Ctrl+Shift+T, the title-bar `+`).
    Picker(Box<projects_ui::Picker>),
    Rename {
        tab: usize,
        edit: LineEdit,
        /// The name the field opened with: an automatic name may change meanwhile.
        was: String,
    },
    ProjectRename {
        root: PathBuf,
        edit: LineEdit,
    },
    /// Per-project defaults: start claude in new sessions, its flags (typed; filtered on save).
    ProjectDefaults(Box<Defaults>),
    /// New session setup: name, shared directory or a new worktree, claude + first prompt.
    Setup(Box<setup::Setup>),
    /// Closing the last session in a linked worktree / Remove… from the list: keep or remove it.
    CloseWorktree(Box<setup::CloseWt>),
    Worktrees(Box<setup::WtList>),
    /// The title bar's search dropdown (quick open, commands, search in files, go to line).
    CommandCenter(Box<command_center::Cc>),
    /// Switch / create a branch in the active session's repo.
    Branches(Box<branches_ui::Branches>),
    /// "Discard …?" / "Commit … files of this agent?" before a git write; `ok` is the action button's label.
    Confirm {
        title: String,
        body: String,
        files: Vec<(String, crate::changes::Mark)>,
        ok: &'static str,
        danger: bool,
        then: Then,
    },
}

/// Closing the window (title-bar x, the compositor) drops the workspace before `on_app_quit` runs, and with it the
/// throttled save task: a change from the last 500 ms would be lost. Save it here.
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.save_pending
            && let Err(e) = persist::save(&self.state_path, &self.snapshot())
        {
            eprintln!("dirigent: saving state failed: {e}");
        }
    }
}

/// A project's file list and whether it hit the index cap.
pub type ProjectFiles = (Arc<Vec<git::index::Entry>>, bool);

/// The Changes panel's dropdowns (`MenuKind::Git`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GitMenu {
    /// The repo bar's ⋯.
    More,
    /// The commit button's ▾.
    Commit,
    /// The diff header's view menu: unified/split, split style, structural/word.
    DiffView,
}

/// A repo's PR strip: for which branch, the last answer, a lookup running.
#[derive(Default)]
struct PrSlot {
    branch: String,
    view: Option<crate::pr::View>,
    loading: bool,
}

/// What a confirm dialog's action button does.
enum Then {
    Git(PathBuf, crate::git_ops::Job),
    /// Close / restart the tab with this id (claude runs in it).
    Close(usize),
    Restart(usize),
}

/// A session row being dragged: its tab id, sidebar group and vector index when the drag began.
#[derive(Clone)]
pub(super) struct SessionDrag {
    id: usize,
    group: Option<PathBuf>,
    pos: usize,
    name: SharedString,
}

/// The card under the pointer while a session row is dragged.
struct RowGhost(SharedString);
impl Render for RowGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        div()
            .px(z(space::MD))
            .py(z(space::XS))
            .rounded(z(radius::MD))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .text_size(z(text_size::SM))
            .text_color(t.text.primary)
            .child(self.0.clone())
    }
}

/// The new tab order after moving group member `from` to position `to` (both positions in `slots`, the vector
/// indices the group holds, ascending). Only those slots change, so the group's first slot (and with it the order of
/// the groups) stays where it was.
fn reorder(n: usize, slots: &[usize], from: usize, to: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut members = slots.to_vec();
    let moved = members.remove(from);
    members.insert(to, moved);
    for (&slot, &m) in slots.iter().zip(&members) {
        order[slot] = m;
    }
    order
}

/// A dock's divider being dragged.
#[derive(Clone)]
struct DockDrag(Dock);

/// A dock's panel tab being dragged to the other dock.
#[derive(Clone)]
struct PanelDrag(Panel);

/// One dock's live state: open, the panel it shows, its width at 100 % (settings.json's value; follows the
/// pointer during a divider drag, written back on release).
#[derive(Clone, Copy)]
struct DockUi {
    open: bool,
    active: Panel,
    width: f32,
}

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
    /// Left and right dock (`Dock::ix`). Widths at 100 % zoom (drawn ×zoom, so docks scale with everything else).
    docks: [DockUi; 2],
    /// A panel tab is being dragged: empty or collapsed docks show a drop strip.
    panel_drag: bool,
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
    /// The window has keyboard focus (desktop notifications skip what you're looking at).
    window_active: bool,
    notifier: Box<dyn notify::Notifier>,
    /// Files each tab's agent changed (sidebar summary, Changes filter, tree marks).
    tracker: Arc<crate::changes::Tracker>,
    _changes_task: Option<Task<()>>,
    /// Git writes per repo (lock, running/failed) and the commit boxes (drafts per repo).
    git_ops: HashMap<PathBuf, crate::git_ops::RepoOps>,
    commit_boxes: HashMap<PathBuf, Entity<crate::commit_box::CommitBox>>,
    /// The PR/MR strip per repo: looked up when its branch changes or after a push, never polled.
    prs: HashMap<PathBuf, PrSlot>,
    /// The one-time "install gh/glab" note was shown (saved).
    pr_hint_shown: bool,
    /// Background fetch (Settings → Git), only while `fetch_minutes` > 0.
    fetch_task: Option<Task<()>>,
    /// Refreshes the active repo's PR every 30 s while its checks run and the Changes panel shows (pr_poll_check).
    pr_poll: Option<Task<()>>,
    /// Preferences (settings.json), and what's wrong with the file (shown on the Settings page and the status bar).
    pub(crate) settings: Settings,
    pub(crate) settings_notes: Vec<String>,
    /// The file isn't valid JSON: the next UI change keeps it as `.bad-<time>` before writing.
    settings_broken: bool,
    pub(crate) settings_path: PathBuf,
    /// The file's text as we last wrote or read it: a watcher ping with the same text is our own write.
    settings_text: Option<String>,
    settings_save_pending: bool,
    _settings_save: Option<Task<()>>,
    _settings_watch: Option<::notify::RecommendedWatcher>,
    _settings_task: Option<Task<()>>,
    /// The font size or UI zoom changed: metrics are rebuilt at the next render (needs the window).
    metrics_stale: bool,
    /// The UI zoom just changed: the status bar shows it until this timer ends.
    /// A short status-bar message ("Zoom 120%", "Not a file: …") and the timer that clears it.
    pub(crate) flash: Option<(String, Task<()>)>,
    _click_task: Option<Task<()>>,
    /// `dirigent <path>` requests from other processes.
    _cli_task: Option<Task<()>>,
    /// Remembered projects (picker, sidebar groups, per-project defaults).
    pub(crate) projects: crate::projects::Projects,
    /// The state file had no project list yet: fill it from the restored tabs' groups (`hint_groups`).
    seed_projects: bool,
    /// An open dropdown (sidebar group header).
    menu: Option<Menu>,
    /// A file `dirigent <file>` asked for in a new tab: shown once that tab's repo is resolved (tab id, file).
    pending_open: Option<(usize, PathBuf)>,
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
    /// Structural diffs by both versions' content hashes.
    structural_cache: Arc<syntax::StructuralCache>,
    files_panel: Entity<FilesPanel>,
    search_panel: Entity<crate::search_panel::SearchPanel>,
    /// Where the Command Center box was last drawn (its dropdown hangs under it).
    cc_anchor: command_center::Anchor,
    show_ignored: bool,
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
    /// The active session row's bottom-left as last painted (the anchor of Shift+F10's menu); None when not drawn.
    active_row: std::rc::Rc<std::cell::Cell<Option<gpui::Point<gpui::Pixels>>>>,
    _spin: Option<Task<()>>,
}

impl Workspace {
    pub fn new(
        saved: Saved,
        state_path: PathBuf,
        (settings_path, loaded): (PathBuf, settings::Loaded),
        translucent: bool,
        launch: crate::cli::Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::theme::set_zoom(loaded.parsed.settings.ui_zoom as f32 / 100.);
        window.set_rem_size(gpui::px(16. * crate::theme::zoom())); // unscaled: this is the scaling
        let metrics = Metrics::new(window, loaded.parsed.settings.font_size);
        let last_dir =
            saved.last_dir.clone().filter(|d| d.is_dir()).or_else(|| std::env::current_dir().ok()).unwrap_or_else(home);
        let (tracker, tracker_wake) = crate::changes::Tracker::new();
        let (repos, repo_pings) = Repos::new(tracker.clone());
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
        let search_panel = {
            let (ws, m) = (cx.entity().downgrade(), metrics.clone());
            cx.new(|cx| crate::search_panel::SearchPanel::new(ws, &m, cx))
        };
        let dir_watcher = git::DirWatcher::start(repos.reporter())
            .map_err(|e| eprintln!("dirigent: no directory watcher for file trees: {e}"))
            .ok();
        let (notifier, clicks) = notify::start(agent::runtime_dir().join("icon.svg"));
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            focus: cx.focus_handle(),
            docks: {
                let l = loaded.parsed.settings.layout;
                [
                    DockUi { open: saved.sidebar.open, active: saved.sidebar.tab, width: l.left_width as f32 },
                    DockUi { open: saved.git_panel.open, active: saved.git_panel.tab, width: l.right_width as f32 },
                ]
            },
            panel_drag: false,
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
            window_active: window.is_window_active(),
            notifier,
            tracker,
            _changes_task: None,
            git_ops: HashMap::new(),
            commit_boxes: HashMap::new(),
            prs: HashMap::new(),
            pr_hint_shown: saved.pr_hint_shown,
            fetch_task: None,
            pr_poll: None,
            settings: loaded.parsed.settings,
            settings_notes: loaded.parsed.notes,
            settings_broken: loaded.parsed.broken,
            settings_path,
            settings_text: loaded.text,
            settings_save_pending: false,
            _settings_save: None,
            _settings_watch: None,
            _settings_task: None,
            metrics_stale: false,
            flash: None,
            _click_task: None,
            _cli_task: None,
            projects: crate::projects::Projects(saved.projects.clone().unwrap_or_default()),
            seed_projects: saved.projects.is_none(),
            menu: None,
            pending_open: None,
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
            overlay: None,
            _overlay_task: None,
            overlay_gen: Arc::new(AtomicU64::new(0)),
            overlay_seen: 0,
            overlay_cancel: Default::default(),
            syntax_cache: Default::default(),
            structural_cache: Default::default(),
            files_panel,
            search_panel,
            cc_anchor: Default::default(),
            show_ignored: saved.git_panel.show_ignored,
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
            active_row: Default::default(),
            _spin: None,
        };
        this._changes_task = Some(cx.spawn(async move |this, cx| {
            while tracker_wake.recv().await.is_ok() {
                crate::perf::event("agent changes");
                if this.update(cx, |w, cx| w.changes_changed(cx)).is_err() {
                    return;
                }
            }
        }));
        match Hub::start(this.tracker.clone()) {
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
            Err(e) => eprintln!("dirigent: Claude Code integration disabled: {e}"),
        }
        this._subscriptions.push(cx.observe_window_activation(window, |w, window, _| {
            w.window_active = window.is_window_active();
            crate::perf::event_with(|| format!("window active: {}", w.window_active));
            // Looking at the session now: its notification is done.
            if let (true, Some(tab)) = (w.window_active, w.tabs.get(w.active)) {
                w.notifier.close(tab.id as u32);
            }
        }));
        this._click_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(click) = clicks.recv().await {
                if this.update_in(cx, |w, window, cx| w.open_from_notification(click, window, cx)).is_err() {
                    return;
                }
            }
        }));
        let crate::cli::Launch { req: launch_req, listener, sock } = launch;
        if let Some(listener) = listener {
            let requests = crate::cli::serve(listener);
            this._cli_task = Some(cx.spawn_in(window, async move |this, cx| {
                while let Ok(inc) = requests.recv().await {
                    let req = inc.req.clone();
                    match this.update_in(cx, |w, window, cx| w.open_request(req, window, cx)) {
                        Ok(r) => inc.reply(r),
                        Err(_) => return,
                    }
                }
            }));
            this._subscriptions.push(cx.on_app_quit(move |_, _| {
                let _ = std::fs::remove_file(&sock);
                async {}
            }));
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
            w.save_pending = false;
            if let Err(e) = persist::save(&w.state_path, &w.snapshot()) {
                eprintln!("dirigent: saving state failed: {e}");
            }
            async {}
        }));
        this.applied = this.grid_size(window);
        this.watch_settings(window, cx);
        this.restart_fetch_timer(cx);
        git::remote::set_host_rules(this.settings.hosts.clone());

        if let Ok(script) = std::env::var("DIRIGENT_RUN") {
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
            // `dirigent <dir>` on a fresh start: that session only, not also one in the last directory.
            if launch_req.dir.is_none() {
                let dir = this.last_dir.clone();
                this.add_tab(None, dir, None, Vec::new(), Vec::new());
                this.activate(0, window, cx);
            }
            let _ = this.open_request(launch_req, window, cx);
        } else {
            // Restore: rows for every tab, a shell only for the active one (the rest spawn on first view).
            for t in saved.tabs {
                let custom = match t.custom {
                    Some(c) => c.then_some(t.name),
                    None => (!is_default_name(&t.name)).then_some(t.name), // a file from before automatic names
                };
                let i = this.add_tab(custom, t.cwd, t.claude_session, t.claude_args, t.expanded);
                this.tabs[i].auto = t.auto;
                if let Some(c) = t.changes {
                    this.tracker.restore(this.tabs[i].id as u32, c);
                }
            }
            let active = saved.active.min(this.tabs.len() - 1);
            this.activate(active, window, cx);
            this.check_resumable(cx);
            this.hint_groups(cx);
            let _ = this.open_request(launch_req, window, cx);
        }

        if let Ok(path) = std::env::var("DIRIGENT_PERF_DIFF") {
            // Perf harness: open that file's diff in the active tab's repo, then jump 40 rows every 50 ms
            // (DIRIGENT_PERF_STEPS times, default 100).
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
                        eprintln!(
                            "[perf] diff open → rows ready {:.0} ms ({} rows)",
                            started.elapsed().as_secs_f64() * 1e3,
                            rows
                        );
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
                    while started.elapsed() < Duration::from_secs(5) {
                        let done = this
                            .update(
                                cx,
                                |w, cx| matches!(&w.overlay, Some(Overlay::Diff(d)) if d.read(cx).structure_settled()),
                            )
                            .unwrap_or(false);
                        if done {
                            eprintln!("[perf] diff structural settled {:.0} ms", started.elapsed().as_secs_f64() * 1e3);
                            break;
                        }
                        timer(5).await;
                    }
                }
                eprintln!("[perf] diff scroll start");
                let steps = std::env::var("DIRIGENT_PERF_STEPS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
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

        if let Some(ms) = std::env::var("DIRIGENT_PERF_MODAL").ok().and_then(|v| v.parse().ok()) {
            // Screenshot harness: open the new-session modal after `ms` milliseconds.
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| w.open_new_session(window, cx));
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_SETUP") {
            // Screenshot harness: `<ms>|<project>|<first prompt, \n = new line>` opens the New session dialog in
            // worktree mode with that prompt (naming runs as when typed); the prefix list opens 6 s later.
            let mut parts = spec.splitn(3, '|');
            let ms = parts.next().and_then(|v| v.parse().ok()).unwrap_or(500);
            let project = PathBuf::from(parts.next().unwrap_or("."));
            let prompt = parts.next().unwrap_or("").replace("\\n", "\n");
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| w.perf_setup(project, &prompt, window, cx));
                cx.background_executor().timer(Duration::from_secs(6)).await;
                let _ = this.update(cx, |w, cx| {
                    if let Some(Modal::Setup(s)) = &mut w.modal {
                        s.prefix_open = true;
                        cx.notify();
                    }
                });
            })
            .detach();
        }

        if let Some(ms) = std::env::var("DIRIGENT_PERF_REORDER").ok().and_then(|v| v.parse().ok()) {
            // Test harness: after 1 s the active group's last session moves to its top (as a drop would), then the
            // window closes `ms` later (before the save throttle fires when short).
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = this.update_in(cx, |w, _, cx| {
                    let Some((group, slots)) = w.groups().into_iter().find(|(_, m)| m.contains(&w.active)) else {
                        return;
                    };
                    let (Some(&first), Some(&last)) = (slots.first(), slots.last()) else { return };
                    let drag = SessionDrag { id: w.tabs[last].id, group, pos: last, name: SharedString::default() };
                    w.drop_session(&drag, first, cx);
                });
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |_, window, _| window.remove_window());
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_CC") {
            // Screenshot / perf harness: `<ms>:<input>` opens the Command Center with that input typed.
            let (ms, input) = spec.split_once(':').unwrap_or((&spec, ""));
            let (ms, input) = (ms.parse().unwrap_or(1000), input.to_string());
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| {
                    crate::perf::event("cc open");
                    w.open_cc(&input, window, cx);
                });
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_FIND") {
            // Perf / screenshot harness: `<ms>:<query>` opens the active session's find bar with that query, a
            // `re:` prefix with the regex toggle on (`find start` / `find done N matches X ms` with DIRIGENT_PERF=2).
            let (ms, query) = spec.split_once(':').unwrap_or((&spec, ""));
            let (ms, query) = (ms.parse().unwrap_or(1000), query.to_string());
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| {
                    let (regex, query) = match query.strip_prefix("re:") {
                        Some(q) => (true, q.to_string()),
                        None => (false, query),
                    };
                    w.find_in_terminal(Some(query), window, cx);
                    if let (true, Some(view)) = (regex, w.tabs.get(w.active).and_then(|t| t.view.clone())) {
                        view.update(cx, |v, cx| v.toggle_find('r', cx));
                    }
                });
            })
            .detach();
        }

        if let Some(ms) = std::env::var("DIRIGENT_PERF_MENU").ok().and_then(|v| v.parse().ok()) {
            // Screenshot harness: the active session's context menu (as Shift+F10) after `ms` milliseconds.
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| w.open_session_menu(w.active, None, window, cx));
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_GIT") {
            // Screenshot harness: `<ms>:<branches|more|commit|view|switch:<branch>>` opens the branch switcher, a Changes
            // panel menu, or the switch (confirm when dirty)
            // after `ms` milliseconds.
            let (ms, what) = spec.split_once(':').unwrap_or(("1000", &spec));
            let (ms, what) = (ms.parse().unwrap_or(1000), what.to_string());
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| {
                    let Some(root) = w.active_root() else { return };
                    let at = gpui::point(window.viewport_size().width - z(280.), z(110.));
                    match what.as_str() {
                        "branches" => w.open_branches(window, cx),
                        "more" => w.open_git_menu(root, GitMenu::More, at, cx),
                        "commit" => w.open_git_menu(
                            root,
                            GitMenu::Commit,
                            gpui::point(at.x, window.viewport_size().height - z(140.)),
                            cx,
                        ),
                        s if s.starts_with("switch:") => {
                            let b = s["switch:".len()..].to_string();
                            w.switch_branch(root, crate::git_ops::Job::Switch(b.clone()), b, window, cx)
                        }
                        "view" => w.open_git_menu(
                            root,
                            GitMenu::DiffView,
                            gpui::point(window.viewport_size().width - z(620.), z(90.)),
                            cx,
                        ),
                        _ => {}
                    }
                });
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_LAYOUT") {
            // Perf/screenshot harness: `<ms>:<panel>:<dock>[,<panel>:<dock>…]` moves panels (as a tab drop), the
            // first after `ms` milliseconds, then one per second.
            let (ms, moves) = spec.split_once(':').unwrap_or(("500", &spec));
            let ms = ms.parse().unwrap_or(500);
            fn word<T: serde::de::DeserializeOwned>(w: &str) -> Option<T> {
                serde_json::from_value(serde_json::Value::String(w.trim().to_string())).ok()
            }
            let moves: Vec<(Panel, Dock)> = moves
                .split(',')
                .filter_map(|m| m.split_once(':').and_then(|(p, d)| Some((word(p)?, word(d)?))))
                .collect();
            cx.spawn_in(window, async move |this, cx| {
                let mut wait = ms;
                for (p, d) in moves {
                    cx.background_executor().timer(Duration::from_millis(wait)).await;
                    wait = 1000;
                    eprintln!("[perf] layout move {p:?} → {d:?}");
                    if this.update(cx, |w, cx| w.move_panel(p, d, cx)).is_err() {
                        return;
                    }
                }
            })
            .detach();
        }

        if let Ok(spec) = std::env::var("DIRIGENT_PERF_SETTINGS") {
            // Screenshot harness: `<ms>[:<category or search>]` opens Settings there after `ms` milliseconds.
            let (ms, what) = spec.split_once(':').unwrap_or((&spec, ""));
            let (ms, what) = (ms.parse().unwrap_or(500), what.to_string());
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                let _ = this.update_in(cx, |w, window, cx| {
                    w.open_settings(window, cx);
                    if let Some(Overlay::Settings(v)) = &w.overlay {
                        v.update(cx, |v, cx| v.harness(&what, cx));
                    }
                });
            })
            .detach();
        }

        if let Ok(path) = std::env::var("DIRIGENT_PERF_OPEN") {
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
                if let Some(steps) = std::env::var("DIRIGENT_PERF_STEPS").ok().and_then(|n| n.parse::<usize>().ok()) {
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
                if std::env::var_os("DIRIGENT_PERF_OPEN_END").is_some() {
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

        if let Ok(dir) = std::env::var("DIRIGENT_PERF_TREE") {
            // Perf harness: show Files, expand that directory (and its parents) in the active tab, then jump 40 rows
            // every 50 ms (DIRIGENT_PERF_STEPS times, default 100).
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
                            w.show_panel(Panel::Files, cx);
                            let dirs: Vec<PathBuf> = dir
                                .ancestors()
                                .take_while(|a| a.starts_with(&root) && *a != root)
                                .map(Path::to_path_buf)
                                .collect();
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
                        eprintln!(
                            "[perf] tree expand → rows ready {:.1} ms ({rows} rows)",
                            t.elapsed().as_secs_f64() * 1e3
                        );
                        break;
                    }
                }
                let steps = std::env::var("DIRIGENT_PERF_STEPS").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
                eprintln!("[perf] tree scroll start");
                for step in 1..=steps {
                    timer(50).await;
                    let _ = this.update(cx, |w, cx| w.files_panel.update(cx, |p, cx| p.scroll_to(step * 40, cx)));
                }
                eprintln!("[perf] tree scroll end");
            })
            .detach();
        }

        if let Some(n) = std::env::var("DIRIGENT_PERF_CYCLE").ok().and_then(|n| n.parse::<usize>().ok()) {
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
        let side = z(self.dock_taken(Dock::Left));
        let right = z(self.dock_taken(Dock::Right));
        let m = &self.metrics;
        let chrome = Chrome::of(window);
        let w = vp.width - side - right - z(PADDING) - chrome.horizontal();
        // Slim session header above the terminal and the status bar below everything.
        let bars = z(crate::theme::height::HEADER + crate::theme::height::STATUS_BAR);
        // Server-side decorations: the Command Center's slim bar sits above the main area.
        let bars = if chrome.csd { bars } else { bars + z(crate::theme::height::TITLE_BAR) };
        let h = vp.height - z(PADDING) - chrome.vertical() - bars;
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
        self.tabs.push(Tab {
            id,
            custom: name,
            auto: None,
            cwd,
            dir_missing,
            view: None,
            claude_session,
            claude_args,
            repo: TabRepo::Unknown,
            repo_gen: 0,
            interrupt_gen: 0,
            hook_cwd: None,
            tree: file_tree::TreeState { expanded: expanded.into_iter().collect(), selected: None },
            group_hint: None,
            notified_seq: 0,
            agent_only: None,
            remember: false,
            main_root: None,
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
                eprintln!("dirigent: no Claude Code hooks for session {id}: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        let session = Session::spawn(dir, self.applied, &env).expect("spawn shell");
        if self.settings.scrollback != Settings::default().scrollback {
            session.set_scrollback(self.settings.scrollback);
        }
        let (metrics, ws) = (self.metrics.clone(), cx.entity().downgrade());
        cx.new(|cx| TerminalView::new(session, metrics, ws, cx))
    }

    fn activate(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        crate::perf::switch_started();
        if let Some(g) = self.tab_group(idx)
            && let Some(p) = self.projects.get_mut(&g)
        {
            p.last_used = now_secs(); // saved with the next change
        }
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
        self.notifier.close(new.id as u32);
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
                        .map(|(id, d)| {
                            (
                                id,
                                d.ancestors()
                                    .find(|a| a.join(".git").exists())
                                    .map(|a| main_checkout(a).unwrap_or_else(|| a.to_path_buf())),
                            )
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                let seed = std::mem::take(&mut w.seed_projects);
                let now = now_secs();
                for (n, (id, hint)) in hints.into_iter().enumerate() {
                    if let Some(t) = w.tabs.iter_mut().find(|t| t.id == id) {
                        if seed {
                            // First start with projects: the existing tabs' repos / directories, in tab order.
                            w.projects.touch(hint.as_deref().unwrap_or(&t.cwd), now - n as i64);
                        }
                        t.group_hint = hint;
                    }
                }
                if seed {
                    w.mark_dirty(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sidebar groups: (repo root or None for "Other", tab indices), see `group_order`.
    fn groups(&self) -> Vec<(Option<PathBuf>, Vec<usize>)> {
        let keys: Vec<Option<PathBuf>> = (0..self.tabs.len()).map(|i| self.tab_group(i)).collect();
        let pinned: Vec<PathBuf> = self
            .projects
            .sorted()
            .into_iter()
            .map(|i| &self.projects.0[i])
            .filter(|p| p.pinned)
            .map(|p| p.root.clone())
            .collect();
        with_pinned(group_order(&keys), &pinned)
    }

    /// A tab's sidebar group: its repo root; outside repos the known project its directory is in, else "Other".
    fn tab_group(&self, i: usize) -> Option<PathBuf> {
        let t = &self.tabs[i];
        if let Some(main) = &t.main_root {
            return Some(main.clone()); // worktrees sit in their project's group
        }
        match &t.repo {
            TabRepo::Repo(root) => Some(root.clone()),
            TabRepo::Unknown | TabRepo::Pending if t.group_hint.is_some() => t.group_hint.clone(),
            _ => self.projects.of_dir(&t.cwd).map(|p| p.root.clone()),
        }
    }

    /// Opens a session in `dir` (picker, group `+`, Ctrl+Shift+N, `dirigent <path>`): its project is remembered;
    /// claude starts when asked, else per the project's default, with the project's flags.
    pub(super) fn new_session(
        &mut self,
        dir: PathBuf,
        claude: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        self.new_session_with(dir, SessionOpts { claude, ..Default::default() }, window, cx)
    }

    /// `new_session` with the setup dialog's choices: name, first prompt, the project (a worktree lies outside its
    /// project's directory) and its setup command (fresh worktrees).
    pub(super) fn new_session_with(
        &mut self,
        dir: PathBuf,
        opts: SessionOpts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let project = match &opts.project {
            Some(root) => self.projects.get(root).cloned(),
            None => self.projects.of_dir(&dir).cloned(),
        }
        .unwrap_or_default();
        self.last_dir = dir.clone();
        let i = self.add_tab(opts.name.filter(|n| !n.trim().is_empty()), dir, None, Vec::new(), Vec::new());
        self.tabs[i].remember = true;
        self.activate(i, window, cx);
        self.mark_dirty(cx);
        let claude = opts.claude.unwrap_or(project.auto_claude);
        let setup = project.setup_cmd.as_deref().filter(|_| opts.setup);
        let prompt = opts.prompt.filter(|p| claude && !p.trim().is_empty());
        let prompt_file = prompt.as_ref().map(|_| agent::prompt_file(self.tabs[i].id));
        if let (Some(line), Some(v)) = (
            crate::projects::start_line(setup, claude, &project.claude_args, prompt_file.as_deref()),
            &self.tabs[i].view,
        ) {
            // Our shim is first on the session's PATH: claude starts with our hooks. The PTY holds the line until the
            // shell has read its rc files.
            let session = v.read(cx).session.clone();
            let line = format!("{line}\r");
            match (prompt_file, prompt) {
                // The prompt file first (off the UI thread), then the line that names it.
                (Some(path), Some(prompt)) => {
                    let session = session.clone();
                    cx.spawn(async move |_, cx| {
                        let p = path.clone();
                        let written =
                            cx.background_executor().spawn(async move { agent::write_prompt(&p, &prompt) }).await;
                        if let Err(e) = written {
                            // The shim then starts claude without it.
                            eprintln!("dirigent: couldn't write the first prompt to {}: {e}", path.display());
                        }
                        session.write(line.as_bytes());
                    })
                    .detach();
                }
                _ => session.write(line.as_bytes()),
            }
            // A folder claude hasn't seen asks "Do you trust the files in this folder?" with "No, exit" selected:
            // Down + Enter picks "Yes, I trust this folder". Only for this start, within 30 s; a trusted folder never
            // shows it and the watch expires. Off in Settings → General.
            if claude && self.settings.trust_auto_accept {
                // Setup may run a while first: a longer watch then.
                let within = Duration::from_secs(if setup.is_some() { 600 } else { 30 });
                session.answer_once(TRUST_PROMPT, vec![b"\x1b[B".to_vec(), b"\r".to_vec()], within);
            }
        }
        i
    }

    /// Ctrl+Shift+N: a new session in the active session's project (its group), no dialog.
    fn new_in_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(self.active) else { return self.open_new_session(window, cx) };
        let dir = self.tab_group(self.active).unwrap_or_else(|| t.cwd.clone());
        self.new_session(dir, None, window, cx);
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

    fn tab_idx(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Whether claude runs in tab `i` (closing or restarting it would stop it).
    fn agent_running(&self, i: usize, cx: &gpui::App) -> bool {
        let exited = self.tabs[i].view.as_ref().is_some_and(|v| v.read(cx).session.is_exited());
        !exited && self.agent(i).state != agent::State::NoAgent
    }

    /// Close (Ctrl+Shift+W, the row's x, middle click, the menu) or restart (the menu) tab `i`: asks first when claude
    /// runs in it (Enter confirms, Esc cancels).
    pub(super) fn close_session(&mut self, i: usize, restart: bool, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        if !self.agent_running(i, cx) {
            return if restart { self.restart_tab(i, window, cx) } else { self.close_now(i, window, cx) };
        }
        let (id, name) = (self.tabs[i].id, self.tab_title(i));
        self.menu = None;
        self.modal = Some(Modal::Confirm {
            title: if restart { "Restart shell?" } else { "Close session?" }.to_string(),
            body: format!("claude is running in {name}; it will be stopped."),
            files: Vec::new(),
            ok: if restart { "Restart shell" } else { "Close session" },
            danger: true,
            then: if restart { Then::Restart(id) } else { Then::Close(id) },
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// A new session in tab `i`'s directory and project (its defaults apply), listed right after it.
    pub(super) fn duplicate(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(i) else { return };
        let (dir, id) = (t.cwd.clone(), t.id);
        let project = t.main_root.clone().or_else(|| self.tab_group(i));
        let new = self.new_session_with(dir, SessionOpts { project, ..Default::default() }, window, cx);
        // Pushed last: move it behind the original (everything else keeps its relative order, groups too).
        let tab = self.tabs.remove(new);
        let at = self.tab_idx(id).map_or(self.tabs.len(), |o| o + 1);
        self.tabs.insert(at, tab);
        self.active = at;
        self.mark_dirty(cx);
    }

    /// Resume the saved claude session of tab `i` (a fresh shell first when it exited), and show it.
    pub(super) fn resume_one(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let exited = self.tabs[i].view.as_ref().is_some_and(|v| v.read(cx).session.is_exited());
        if exited {
            self.restart_tab(i, window, cx);
        }
        self.activate(i, window, cx);
        if let Some(v) = self.tabs[i].view.clone() {
            v.update(cx, |v, cx| {
                v.set_banner(None, cx);
                cx.notify();
            });
        }
        self.resume_tabs(vec![i], cx);
    }

    /// Whether tab `i` shows unread output or an unseen "done".
    fn has_unread(&self, i: usize, cx: &gpui::App) -> bool {
        let unread = i != self.active && self.tabs[i].view.as_ref().is_some_and(|v| v.read(cx).session.is_unread());
        unread || self.agent(i).unseen
    }

    pub(super) fn mark_read(&mut self, i: usize, cx: &mut Context<Self>) {
        let t = &self.tabs[i];
        if let Some(v) = &t.view {
            v.read(cx).session.clear_unread();
        }
        if let Some(hub) = &self.agents {
            hub.board.lock().unwrap().mark_seen(t.id as u32);
        }
        self.notifier.close(t.id as u32);
        cx.notify();
    }

    /// Drop of a dragged session row on row `target`: within its group only (another group is refused).
    pub(super) fn drop_session(&mut self, drag: &SessionDrag, target: usize, cx: &mut Context<Self>) {
        let Some(from) = self.tab_idx(drag.id) else { return };
        let group = self.tab_group(from);
        if group != drag.group || target >= self.tabs.len() || self.tab_group(target) != group {
            return;
        }
        let Some((_, slots)) = self.groups().into_iter().find(|(k, _)| *k == group) else { return };
        let (Some(a), Some(b)) = (slots.iter().position(|&s| s == from), slots.iter().position(|&s| s == target))
        else {
            return;
        };
        if a == b {
            return;
        }
        let order = reorder(self.tabs.len(), &slots, a, b);
        let mut old: Vec<Option<Tab>> = std::mem::take(&mut self.tabs).into_iter().map(Some).collect();
        self.tabs = order.iter().map(|&j| old[j].take().expect("a permutation")).collect();
        let new_pos = |i: usize| order.iter().position(|&j| j == i).unwrap_or(i);
        self.active = new_pos(self.active);
        if let Some(Modal::Rename { tab, .. }) = &mut self.modal {
            *tab = new_pos(*tab);
        }
        self.mark_dirty(cx);
        cx.notify();
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_session(self.active, false, window, cx);
    }

    /// Closes tab `i`, unless it is the last session in a linked worktree: then asks whether to keep the worktree.
    fn close_now(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(i) else { return };
        // The last session in a linked worktree: ask whether to keep or remove the worktree.
        if let (Some(main), TabRepo::Repo(root)) = (t.main_root.clone(), t.repo.clone())
            && !self.tabs.iter().any(|o| o.id != t.id && o.repo == t.repo)
        {
            let branch = self.repos.status(&root).map(|s| s.branch.clone()).filter(|b| !b.starts_with('('));
            self.ask_close_worktree(root, main, branch, Some(t.id), window, cx);
            return;
        }
        self.modal = None;
        self.close_tab(i, window, cx);
    }

    /// Closes tab `idx` now (its shell and whatever runs in it are killed off-thread).
    pub(super) fn close_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        let was_active = idx == self.active;
        // Dropping the view drops the Session, which kills + reaps the shell on its own thread.
        let tab = self.tabs.remove(idx);
        if let Some(hub) = &self.agents {
            hub.remove_tab(tab.id as u32);
        }
        self.notifier.close(tab.id as u32);
        self.tracker.remove_tab(tab.id as u32);
        if let TabRepo::Repo(root) = &tab.repo {
            self.repos.release(root);
        }
        self.drop_overlay();
        self.tree_rows = None;
        self.mark_dirty(cx);
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus, cx);
            self.sync_panel(cx);
            cx.notify();
        } else if was_active {
            let idx = idx.min(self.tabs.len() - 1);
            self.active = usize::MAX; // old one is gone; nothing to hide
            self.activate(idx, window, cx);
        } else {
            if idx < self.active {
                self.active -= 1;
            }
            cx.notify();
        }
    }

    pub fn restart(&mut self, view_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.tab_of_view(view_id) {
            self.restart_tab(i, window, cx);
        }
    }

    /// A new shell in tab `i` (the old one, and whatever runs in it, is killed off-thread).
    fn restart_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The name a session shows: the user's own, else the automatic one, else a linked worktree's branch, else
    /// its directory's name.
    pub(crate) fn tab_title(&self, i: usize) -> String {
        let t = &self.tabs[i];
        if let Some(name) = t.custom.as_ref().or(t.auto.as_ref().map(|a| &a.text)) {
            return name.clone();
        }
        if t.main_root.is_some()
            && let TabRepo::Repo(root) = &t.repo
            && let Some(s) = self.repos.status(root)
            && !s.branch.is_empty()
        {
            return s.branch.clone();
        }
        t.cwd.file_name().map_or_else(|| t.cwd.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    /// Drops the user's own name: the automatic one shows again.
    pub(crate) fn reset_name(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(i)
            && t.custom.take().is_some()
        {
            self.mark_dirty(cx);
            cx.notify();
        }
    }

    fn start_rename(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        let was = self.tab_title(idx);
        self.modal = Some(Modal::Rename { tab: idx, edit: LineEdit::new(was.clone()), was });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Session rename (Enter, or a click anywhere outside the field): a typed name that differs from the one shown
    /// becomes the session's own name (automatic names no longer replace it); an empty one is ignored.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Modal::Rename { tab, edit, was }) = self.modal.take() {
            let name = edit.text.trim();
            if tab < self.tabs.len() && !name.is_empty() && name != was.trim() {
                self.tabs[tab].custom = Some(name.to_string());
                self.mark_dirty(cx);
            }
            self.close_modal(window, cx);
        }
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        // Back to what's shown: an open diff/preview hides the terminal (focusing it would leave no key target).
        self.focus_main(window, cx);
        cx.notify();
    }

    fn modal_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if self.menu.is_some() {
            self.menu_key(&ks.key, window, cx);
            cx.stop_propagation(); // a dropdown is open: keys go nowhere else
            return;
        }
        let Some(modal) = self.modal.as_mut() else { return };
        let edit = match modal {
            Modal::Picker(_) => return self.picker_key(ev, window, cx),
            Modal::Setup(_) => return self.setup_key(ev, window, cx),
            Modal::CloseWorktree(_) => return self.close_wt_key(ev, window, cx),
            Modal::Worktrees(_) => return self.worktrees_key(ev, window, cx),
            Modal::CommandCenter(_) => return self.cc_key(ev, window, cx),
            Modal::Branches(_) => return self.branches_key(ev, window, cx),
            Modal::ProjectDefaults(d) if ks.key == "tab" || (d.focus == 0 && ks.key == "space") => {
                if ks.key == "tab" {
                    d.focus = (d.focus + if ks.modifiers.shift { 3 } else { 1 }) % 4;
                } else {
                    d.auto = !d.auto;
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            Modal::ProjectDefaults(d) => match d.focus {
                1 => &mut d.flags,
                2 => &mut d.setup,
                3 => &mut d.wt_dir,
                _ if matches!(ks.key.as_str(), "enter" | "escape") => &mut d.flags,
                _ => {
                    cx.stop_propagation();
                    return;
                }
            },
            Modal::Rename { edit, .. } | Modal::ProjectRename { edit, .. } => edit,
            Modal::Confirm { .. } => {
                match ks.key.as_str() {
                    "enter" => self.confirm_ok(window, cx),
                    "escape" => self.close_modal(window, cx),
                    _ => {}
                }
                cx.stop_propagation();
                return;
            }
        };
        match edit.key(ks, cx) {
            Edit::Ignored => return,
            Edit::Changed | Edit::Moved => {}
            Edit::Cancel => self.close_modal(window, cx),
            Edit::Submit => match self.modal.take() {
                rename @ Some(Modal::Rename { .. }) => {
                    self.modal = rename;
                    self.commit_rename(window, cx);
                }
                Some(Modal::ProjectRename { root, edit }) => {
                    let name = edit.text.trim();
                    let folder = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if self.projects.get(&root).is_none() {
                        self.projects.touch(&root, now_secs());
                    }
                    if let Some(p) = self.projects.get_mut(&root) {
                        // Empty or the folder name: no custom name.
                        p.name = (!name.is_empty() && name != folder).then(|| name.to_string());
                        self.mark_dirty(cx);
                    }
                    self.close_modal(window, cx);
                }
                Some(Modal::ProjectDefaults(d)) => {
                    if self.projects.get(&d.root).is_none() {
                        self.projects.touch(&d.root, now_secs()); // a group that wasn't remembered yet
                    }
                    if let Some(p) = self.projects.get_mut(&d.root) {
                        let typed = crate::projects::Typed {
                            auto: d.auto,
                            flags: &d.flags.text,
                            setup: &d.setup.text,
                            wt_dir: &d.wt_dir.text,
                        };
                        p.apply_defaults(&typed, &home());
                        self.mark_dirty(cx);
                    }
                    self.close_modal(window, cx);
                }
                other => self.modal = other,
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
                // Kept under a custom name too: "Reset to automatic name" shows it at once.
                if let Some(title) = a.title
                    && agent::offer(tab.auto.as_ref(), &title)
                {
                    tab.auto = Some(title);
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
        self.notify_changes();
        cx.notify();
    }

    /// One desktop notification per agent state change into "needs you"/"done", unless it's the session you're
    /// looking at (see `notify::should_notify`).
    fn notify_changes(&mut self) {
        let Some(hub) = &self.agents else { return };
        // Titles only for the tabs with a new notice (this runs on every hook).
        let titles: Vec<Option<String>> = {
            let board = hub.board.lock().unwrap();
            let fresh = |i: usize| board.get(self.tabs[i].id as u32).notice_seq != self.tabs[i].notified_seq;
            (0..self.tabs.len()).map(|i| fresh(i).then(|| self.tab_title(i))).collect()
        };
        let board = hub.board.lock().unwrap();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let a = board.get(tab.id as u32);
            if a.notice_seq == tab.notified_seq {
                continue;
            }
            tab.notified_seq = a.notice_seq;
            if !notify::should_notify(a.state, self.window_active, i == self.active, self.settings.notifications) {
                continue;
            }
            let project = match &tab.repo {
                TabRepo::Repo(root) => root.file_name(),
                _ => tab.cwd.file_name(),
            }
            .map(|n| n.to_string_lossy().into_owned());
            let name: String = titles[i].as_deref().unwrap_or_default().chars().take(60).collect();
            let title = match project {
                Some(p) if p != name => format!("{name} · {p}"),
                _ => name,
            };
            let urgent = a.state == agent::State::NeedsYou;
            let body = a.note.clone().unwrap_or_else(|| if urgent { "Needs your input" } else { "Finished" }.into());
            crate::perf::event_with(|| format!("notify tab {}: {body}", tab.id));
            self.notifier.show(notify::Notice {
                tab: tab.id as u32,
                title,
                body,
                urgent,
                sound: self.settings.notifications.sound,
            });
        }
    }

    /// An agent's changed files (or how they look in git) changed: sidebar, panel, tree.
    fn changes_changed(&mut self, cx: &mut Context<Self>) {
        if self.tracker.take_sets_dirty() {
            self.mark_dirty(cx);
        }
        self.sync_panel(cx);
        cx.notify();
    }

    /// A notification was clicked: show its session and bring the window up.
    /// `dirigent [--new] [--claude] [PATH]` (from another process, or the one that started us): focus the session
    /// in that directory or make one, start claude, show the file, bring the window up.
    fn open_request(
        &mut self,
        req: crate::cli::Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        crate::perf::event_with(|| {
            format!("cli request: {:?} new={} claude={} open={}", req.dir, req.new, req.claude, req.open.is_some())
        });
        if let Some(dir) = req.dir.clone() {
            if self.modal.is_some() {
                self.close_modal(window, cx);
            }
            let rows: Vec<(&Path, bool)> = {
                let board = self.agents.as_ref().map(|h| h.board.lock().unwrap());
                self.tabs
                    .iter()
                    .map(|t| {
                        let agent =
                            board.as_ref().is_some_and(|b| b.get(t.id as u32).state != crate::agent::State::NoAgent);
                        (t.cwd.as_path(), agent && t.view.is_some())
                    })
                    .collect()
            };
            let picked = crate::cli::pick(&req, &rows, self.active, &self.visual_order());
            let i = match picked {
                Some(i) => {
                    self.activate(i, window, cx);
                    i
                }
                None => {
                    if !dir.is_dir() {
                        return Err(format!("no such directory: {}", dir.display()));
                    }
                    // `--claude` forces claude; otherwise the project's default decides.
                    self.new_session(dir, req.claude.then_some(true), window, cx)
                }
            };
            if let Some(file) = req.open {
                if matches!(self.tabs[i].repo, TabRepo::Unknown | TabRepo::Pending) {
                    self.pending_open = Some((self.tabs[i].id, file)); // shown by `render` once resolved
                } else {
                    self.open_file(file, false, window, cx);
                }
            }
        }
        notify::raise(window, req.token.as_deref());
        cx.notify();
        Ok(())
    }

    /// `pending_open`, once its tab's repo is known (called from render; the tab must still be the active one).
    pub(super) fn flush_pending_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = &self.pending_open else { return };
        let Some(t) = self.tabs.get(self.active).filter(|t| t.id == *id) else {
            self.pending_open = None; // closed or switched away meanwhile
            return;
        };
        if matches!(t.repo, TabRepo::Unknown | TabRepo::Pending) {
            return;
        }
        if let Some((_, file)) = self.pending_open.take() {
            cx.defer_in(window, move |w, window, cx| w.open_file(file, false, window, cx));
        }
    }

    fn open_from_notification(&mut self, click: notify::Click, window: &mut Window, cx: &mut Context<Self>) {
        crate::perf::event_with(|| format!("notification click: tab {} (token: {})", click.tab, click.token.is_some()));
        if let Some(i) = self.tabs.iter().position(|t| t.id as u32 == click.tab) {
            self.activate(i, window, cx);
        }
        notify::raise(window, click.token.as_deref());
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
                Err(e) => eprintln!("dirigent: saving state failed: {e}"),
            }
        }));
    }

    fn snapshot(&self) -> Saved {
        Saved {
            tabs: self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| TabState {
                    name: self.tab_title(i),
                    custom: Some(t.custom.is_some()),
                    auto: t.auto.clone(),
                    cwd: t.cwd.clone(),
                    claude_session: t.claude_session.clone(),
                    claude_args: t.claude_args.clone(),
                    expanded: t.tree.expanded.iter().cloned().collect(),
                    changes: self.tracker.saved(t.id as u32),
                })
                .collect(),
            active: self.active.min(self.tabs.len().saturating_sub(1)),
            sidebar: persist::SidebarState {
                width: self.docks[0].width,
                open: self.docks[0].open,
                tab: self.docks[0].active,
            },
            window: self.window_state,
            last_dir: Some(self.last_dir.clone()),
            collapsed_groups: self.collapsed.iter().cloned().collect(),
            git_panel: persist::PanelState {
                width: self.docks[1].width,
                open: self.docks[1].open,
                tab: self.docks[1].active,
                show_ignored: self.show_ignored,
            },
            projects: Some(self.projects.0.clone()),
            pr_hint_shown: self.pr_hint_shown,
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
                .spawn(async move {
                    ids.into_iter().filter(|(_, s)| !persist::transcript_exists(s)).map(|(id, _)| id).collect()
                })
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

    /// Esc / Ctrl-C was typed into a tab whose Claude is working. Claude sends no hook for an interrupt, so for
    /// up to `WATCH` (no timers otherwise) look for either its transcript marker (an interrupt mid-turn) or, when it
    /// stopped before any output (no marker then; the prompt goes back to the input), the TUI going quiet: the
    /// spinner draws every ≤ 250 ms while working (2.1.319). Esc closing a menu or switching vim mode does neither.
    pub fn key_interrupt(&mut self, view_id: EntityId, session: std::rc::Rc<Session>, cx: &mut Context<Self>) {
        const TICK: Duration = Duration::from_millis(250);
        const QUIET: Duration = Duration::from_millis(1250);
        const WATCH: Duration = Duration::from_secs(4);
        let (Some(hub), Some(i)) = (&self.agents, self.tab_of_view(view_id)) else { return };
        let tab = &mut self.tabs[i];
        let a = hub.board.lock().unwrap().get(tab.id as u32);
        let (agent::State::Working, Some(sid)) = (a.state, a.session_id) else { return };
        tab.interrupt_gen += 1;
        let (id, generation, prompts, board) = (tab.id, tab.interrupt_gen, a.prompts, hub.board.clone());
        cx.spawn(async move |this, cx| {
            let path = cx.background_executor().spawn(async move { persist::transcript_path(&sid) }).await;
            let start = Instant::now();
            let (mut seq, mut quiet_since) = (session.output_seq(), start);
            while start.elapsed() < WATCH {
                cx.background_executor().timer(TICK).await;
                let newest = this.update(cx, |w, _| w.tabs.iter().any(|t| t.id == id && t.interrupt_gen == generation));
                if !newest.unwrap_or(false) || board.lock().unwrap().get(id as u32).state != agent::State::Working {
                    return;
                }
                if session.output_seq() != seq {
                    (seq, quiet_since) = (session.output_seq(), Instant::now());
                }
                let marker = match path.clone() {
                    Some(p) => {
                        cx.background_executor()
                            .spawn(async move { agent::interrupted(&agent::transcript_tail(&p)) })
                            .await
                    }
                    None => false,
                };
                if marker || quiet_since.elapsed() >= QUIET {
                    if board.lock().unwrap().interrupt(id as u32, prompts) {
                        let _ = this.update(cx, |w, cx| w.agents_changed(cx));
                    }
                    return;
                }
            }
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
                    jobs.into_iter()
                        .map(|(id, s, a)| {
                            let ok = persist::transcript_exists(&s);
                            (id, s, a, ok)
                        })
                        .collect()
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
            b.five_hour.is_some()
                || b.seven_day.is_some()
                || b.tabs.values().any(|a| a.last_activity.is_some() || a.state_since.is_some())
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

    /// Ctrl+Shift+B / the brand's collapse button: Sessions shown → its dock collapses; else it is shown.
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.panel_visible(Panel::Sessions) {
            self.set_dock_open(self.settings.layout.sessions, false, cx);
        } else {
            self.show_panel(Panel::Sessions, cx);
        }
    }
}

mod branches_ui;
mod command_center;
mod git_write;
mod projects_ui;
mod setup;
mod view;

use projects_ui::Menu;

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
        let main_root = match &found {
            git::Discovery::Repo(r) => r.main_root().map(Path::to_path_buf),
            _ => None,
        };
        if new == t.repo && main_root == t.main_root {
            return;
        }
        t.main_root = main_root;
        crate::perf::event_with(|| {
            format!(
                "tab {id} repo → {}",
                match &new {
                    TabRepo::Repo(r) => r.display().to_string(),
                    _ => "none".into(),
                }
            )
        });
        self.tracker.set_repo(
            id as u32,
            match &new {
                TabRepo::Repo(r) => Some(r.clone()),
                _ => None,
            },
        );
        if let TabRepo::Repo(old) = std::mem::replace(&mut t.repo, new) {
            self.repos.release(&old);
        }
        if std::mem::take(&mut t.remember) {
            let root = match (&t.main_root, &t.repo) {
                (Some(main), _) => main.clone(), // a worktree belongs to its main checkout's project
                (None, TabRepo::Repo(r)) => r.clone(),
                _ => t.cwd.clone(),
            };
            self.projects.touch(&root, now_secs());
            self.mark_dirty(cx);
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
        for root in &roots {
            if let (Some(st), Some(p)) = (self.repos.status(root), self.projects.get_mut(root))
                && p.branch.as_deref() != Some(st.branch.as_str())
            {
                p.branch = Some(st.branch.clone()); // saved with the next change (the picker shows it)
            }
        }
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
        let commit = match self.tabs.get(self.active).map(|t| t.repo.clone()) {
            Some(TabRepo::Repo(root)) => {
                // The strip follows the branch: a new branch (switch, first status) → one lookup.
                let branch = self.repos.status(&root).map(|s| s.branch.clone());
                if branch.is_some_and(|b| self.prs.get(&root).is_none_or(|p| p.branch != b)) {
                    self.pr_lookup(root.clone(), false, cx);
                }
                Some(self.sync_commit_box(&root, cx))
            }
            _ => None,
        };
        let view = match self.tabs.get(self.active) {
            None => PanelView::NoSession,
            Some(t) => match &t.repo {
                TabRepo::Unknown | TabRepo::Pending => PanelView::Pending,
                TabRepo::NotRepo => PanelView::NotRepo,
                TabRepo::GitMissing => PanelView::GitMissing,
                TabRepo::Repo(root) => {
                    let view = self.tracker.view(t.id as u32);
                    let ran = t.claude_session.is_some() || !view.touched.is_empty();
                    let agent = ran.then(|| git_panel::AgentFiles { view, only: t.agent_only.unwrap_or(true) });
                    let ops = Box::new(self.git_ops.get(root).map(|o| o.view.clone()).unwrap_or_default());
                    let status = self.repos.status(root);
                    let pr = self.prs.get(root).filter(|p| status.as_ref().is_some_and(|s| s.branch == p.branch));
                    let pr = pr.and_then(|p| p.view.clone()).map(Box::new);
                    PanelView::Repo { root: root.clone(), status, agent, ops, commit, pr }
                }
            },
        };
        let selected = match &self.overlay {
            Some(Overlay::Diff(d)) => Some(d.read(cx).file.path.clone()),
            _ => None,
        };
        self.git_panel.update(cx, |p, cx| p.set(view, selected, cx));
        self.sync_files(false, cx);
        self.pr_poll_check(cx);
    }

    /// Changes panel: This agent / All for the active tab.
    pub fn set_agent_filter(&mut self, only: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.agent_only = Some(only);
            self.sync_panel(cx);
        }
    }

    /// Changes panel "Clear": forgets the active tab's agent files (the tracker wakes the UI).
    pub fn clear_agent_changes(&mut self, _: &mut Context<Self>) {
        if let Some(t) = self.tabs.get(self.active) {
            self.tracker.clear(t.id as u32);
        }
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
        let m = self.metrics.clone();
        let mode =
            (self.settings.diff_mode, self.settings.split_style, self.settings.diff_engine, self.diff_narrow(window));
        let view = cx.new(|cx| DiffView::new(root, file, &m, mode, ws, cx));
        self.show_overlay(Overlay::Diff(view), window, cx);
    }

    fn diff_narrow(&self, window: &Window) -> bool {
        self.grid_size(window).cols < git_panel::MIN_SPLIT_COLS
    }

    /// Ctrl+Shift+D (only while a diff is open) and the diff header's switch.
    pub fn toggle_diff_mode(&mut self, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Diff(_))) {
            let mode = match self.settings.diff_mode {
                settings::DiffMode::Unified => settings::DiffMode::Split,
                settings::DiffMode::Split => settings::DiffMode::Unified,
            };
            self.set_diff_mode(mode, cx);
        }
    }

    pub fn set_diff_mode(&mut self, mode: settings::DiffMode, cx: &mut Context<Self>) {
        self.change_settings(|s| s.diff_mode = mode, cx);
    }

    pub fn set_split_style(&mut self, style: settings::SplitStyle, cx: &mut Context<Self>) {
        self.change_settings(|s| s.split_style = style, cx);
    }

    pub fn set_diff_engine(&mut self, engine: settings::DiffEngine, cx: &mut Context<Self>) {
        self.change_settings(|s| s.diff_engine = engine, cx);
    }

    /// A change from the Settings page or a diff header switch: applied now, written to settings.json.
    pub fn change_settings(&mut self, f: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut new = self.settings.clone();
        f(&mut new);
        if new != self.settings {
            self.apply_settings(new, cx);
            self.save_settings(cx);
        }
    }

    /// Shows `text` in the status bar for a moment; a new one replaces it (and its timer).
    pub(crate) fn flash(&mut self, text: String, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |w, cx| {
                w.flash = None;
                cx.notify();
            });
        });
        self.flash = Some((text, task));
        cx.notify();
    }

    /// Ctrl+click on a path printed in a session: resolved against that session's directory (`~/` from home),
    /// checked off-thread, then shown in the preview at `line`. Not a file: a status-bar note.
    pub fn open_path_from(
        &mut self,
        view_id: EntityId,
        raw: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(i) = self.tab_of_view(view_id) else { return };
        let joined = match raw.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => self.tabs[i].cwd.join(&raw),
        };
        let path: PathBuf = joined.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
        cx.spawn_in(window, async move |this, cx| {
            let p = path.clone();
            let is_file = cx.background_executor().spawn(async move { p.is_file() }).await;
            let _ = this.update_in(cx, |w, window, cx| {
                if !is_file {
                    return w.flash(format!("Not a file: {}", tilde(&path)), cx);
                }
                w.open_file(path, false, window, cx);
                if let (Some(line), Some(Overlay::File(f))) = (line, &w.overlay) {
                    f.update(cx, |f, cx| f.go_to_line(line as usize, cx));
                }
            });
        })
        .detach();
    }

    /// The active session's project for quick open / content search: its repo root (else its directory), and its
    /// file list. Everything that may block (the walk, the slot's lock: the watcher thread holds it while patching)
    /// happens off the UI thread. A repo's list is built once and kept live; other directories and the "include
    /// ignored" list are walked again on each call.
    pub fn project_files(&self, ignored: bool, cx: &mut Context<Self>) -> Option<(PathBuf, Task<ProjectFiles>)> {
        let tab = self.tabs.get(self.active)?;
        let root = Self::tree_root(tab)?;
        let slot = (!ignored).then(|| self.repos.index(&root)).flatten();
        let r = root.clone();
        let task = cx.background_executor().spawn(async move {
            match slot {
                Some(slot) => {
                    let mut g = slot.lock().unwrap();
                    let ix = g.get_or_insert_with(|| git::index::Index::build(&r, false, git::index::CAP));
                    (ix.files.clone(), ix.capped)
                }
                None => {
                    let ix = git::index::Index::build(&r, ignored, git::index::CAP);
                    (ix.files, ix.capped)
                }
            }
        });
        Some((root, task))
    }

    /// Makes `new` the live settings, touching only what changed.
    fn apply_settings(&mut self, new: Settings, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.settings, new.clone());
        if old.font_size != new.font_size || old.ui_zoom != new.ui_zoom {
            self.metrics_stale = true; // rebuilt in render (needs the window)
        }
        if old.ui_zoom != new.ui_zoom {
            crate::theme::set_zoom(new.ui_zoom as f32 / 100.);
            self.flash(format!("Zoom {}%", new.ui_zoom), cx);
        }
        if old.scrollback != new.scrollback {
            for view in self.tabs.iter().filter_map(|t| t.view.as_ref()) {
                view.read(cx).session.set_scrollback(new.scrollback);
            }
        }
        if (old.diff_mode, old.split_style, old.diff_engine) != (new.diff_mode, new.split_style, new.diff_engine) {
            self.sync_diff_mode(cx);
        }
        if old.fetch_minutes != new.fetch_minutes {
            self.restart_fetch_timer(cx);
        }
        if old.name_with_claude != new.name_with_claude {
            self.setup_naming_setting(new.name_with_claude, cx);
        }
        if old.hosts != new.hosts {
            // Forge detection and every browser URL follow the rules: look the strips up again.
            git::remote::set_host_rules(new.hosts.clone());
            self.prs.clear();
            self.sync_panel(cx);
        }
        if old.layout != new.layout {
            for d in [Dock::Left, Dock::Right] {
                self.docks[d.ix()].width = new.layout.width(d) as f32;
            }
            self.flush_resize = true; // one PTY resize at the next render
            self.sync_panel(cx); // the Changes legend follows its dock's width
        }
        self.sync_settings_view(cx);
        cx.notify();
    }

    /// The open Settings page re-renders (it's a cached view).
    pub(crate) fn sync_settings_view(&self, cx: &mut Context<Self>) {
        if let Some(Overlay::Settings(v)) = &self.overlay {
            v.update(cx, |_, cx| cx.notify());
        }
    }

    /// Writes settings.json shortly after a change (the latest values; one write at a time), off the UI thread.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        // What we write is the whole truth now: the old file's problems are gone with it.
        self.settings_notes = if self.settings_broken {
            vec!["settings.json wasn't valid JSON: kept as settings.json.bad-<time>, written fresh".into()]
        } else {
            Vec::new()
        };
        if self.settings_save_pending {
            return;
        }
        self.settings_save_pending = true;
        self._settings_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let Ok((path, text, broken)) = this.update(cx, |w, _| {
                w.settings_save_pending = false;
                let text = w.settings.to_json();
                w.settings_text = Some(text.clone());
                (w.settings_path.clone(), text, std::mem::take(&mut w.settings_broken))
            }) else {
                return;
            };
            let result = cx.background_executor().spawn(async move { settings::save(&path, &text, broken) }).await;
            if let Err(e) = result {
                eprintln!("dirigent: saving settings failed: {e}");
                let _ = this.update(cx, |w, cx| {
                    w.settings_notes = vec![format!("Couldn't write settings.json: {e}")];
                    w.sync_settings_view(cx);
                    cx.notify();
                });
            }
        }));
    }

    /// Hand edits of settings.json apply live: the watcher pings, the file is read off the UI thread.
    fn watch_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (watcher, pings) = match settings::watch(&self.settings_path) {
            Ok(w) => w,
            Err(e) => return eprintln!("dirigent: settings.json is not watched (edits apply after a restart): {e}"),
        };
        self._settings_watch = Some(watcher);
        let path = self.settings_path.clone();
        self._settings_task = Some(cx.spawn_in(window, async move |this, cx| {
            while pings.recv().await.is_ok() {
                let p = path.clone();
                let text = cx.background_executor().spawn(async move { std::fs::read_to_string(p) }).await;
                let applied = this.update(cx, |w, cx| {
                    let text = match text {
                        Ok(t) => Some(t),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => {
                            w.settings_notes = vec![format!("settings.json: {e}")];
                            return cx.notify();
                        }
                    };
                    if text == w.settings_text {
                        return; // our own write
                    }
                    let parsed = match &text {
                        Some(t) => settings::parse(t, &w.settings),
                        None => settings::Parsed { settings: Settings::default(), notes: Vec::new(), broken: false },
                    };
                    crate::perf::event("settings reloaded");
                    w.settings_text = text;
                    w.settings_notes = parsed.notes;
                    w.settings_broken = parsed.broken;
                    w.apply_settings(parsed.settings, cx);
                });
                if applied.is_err() {
                    return;
                }
            }
        }));
    }

    /// Settings → Projects: a project's defaults as typed there (state file, not settings.json).
    pub(crate) fn set_project_defaults(&mut self, root: &Path, typed: &crate::projects::Typed, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.get_mut(root) {
            p.apply_defaults(typed, &home());
            self.mark_dirty(cx);
            self.sync_settings_view(cx);
        }
    }

    /// Ctrl+= / Ctrl++ / Ctrl+- (1 px steps, 8–32) and Ctrl+0 (default): the terminal font size setting.
    /// Ctrl+= / Ctrl++ (Some(1)), Ctrl+- (Some(-1)), Ctrl+0 (None): the UI zoom, 10 % steps, 50–200 %.
    pub(crate) fn zoom(&mut self, dir: Option<i32>, cx: &mut Context<Self>) {
        crate::perf::event_with(|| format!("zoom {dir:?}"));
        self.change_settings(|s| s.ui_zoom = s.zoom_step(dir), cx);
    }

    pub(crate) fn settings_open(&self) -> bool {
        matches!(self.overlay, Some(Overlay::Settings(_)))
    }

    /// Ctrl+, and the gear: the Settings page over the main area (again: closes it).
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Settings(_))) {
            return self.close_overlay(window, cx);
        }
        let ws = cx.entity().downgrade();
        let view = cx.new(|cx| crate::settings_view::SettingsView::new(ws, cx));
        self.show_overlay(Overlay::Settings(view), window, cx);
    }

    /// After a font size change: new metrics for every terminal and the open diff/preview, then the PTY resize.
    pub(crate) fn refresh_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.metrics_stale) {
            return;
        }
        // The PTYs follow in this same frame (`schedule_resize` runs right after): one layout and one resize per step,
        // not a second one when the debounce ends.
        self.flush_resize = true;
        // gpui's own rem-based helpers (`.px_2()` …) follow the zoom too.
        window.set_rem_size(gpui::px(16. * crate::theme::zoom())); // unscaled: this is the scaling
        self.metrics = Metrics::new(window, self.settings.font_size);
        let m = self.metrics.clone();
        for view in self.tabs.iter().filter_map(|t| t.view.clone()) {
            let m = m.clone();
            view.update(cx, |v, cx| v.set_metrics(m, cx));
        }
        match &self.overlay {
            Some(Overlay::Diff(v)) => v.update(cx, |v, cx| v.set_metrics(&m, cx)),
            Some(Overlay::File(v)) => v.update(cx, |v, cx| v.set_metrics(&m, cx)),
            Some(Overlay::Settings(v)) => v.update(cx, |_, cx| cx.notify()),
            None => {}
        }
        // Cached views replay their last frame until notified: every one of them lays out again, once.
        self.git_panel.update(cx, |_, cx| cx.notify());
        self.files_panel.update(cx, |_, cx| cx.notify());
        self.search_panel.update(cx, |p, cx| p.set_metrics(&m, cx));
        for b in self.commit_boxes.values() {
            b.update(cx, |_, cx| cx.notify());
        }
    }

    /// Hands the mode, split style, engine and the (settled) main-area width to the open diff; it re-renders only if
    /// one of them changed.
    fn sync_diff_mode(&self, cx: &mut Context<Self>) {
        if let Some(Overlay::Diff(v)) = &self.overlay {
            let narrow = self.applied.cols < git_panel::MIN_SPLIT_COLS;
            let (mode, style, engine) = (self.settings.diff_mode, self.settings.split_style, self.settings.diff_engine);
            v.update(cx, |v, cx| v.set_mode(mode, style, engine, narrow, cx));
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
        let m = self.metrics.clone();
        let view = cx.new(|cx| FileView::new(path, repo, base, &m, ws, cx));
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
            Overlay::Settings(v) => v.focus_handle(cx),
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
            Overlay::Settings(_) => None,
        }
    }

    /// Ctrl+Alt+F: the find bar of the active session (a diff, preview or Settings over it closes first).
    pub fn find_in_terminal(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
        if let Some(view) = self.tabs.get(self.active).and_then(|t| t.view.clone()) {
            view.update(cx, |v, cx| v.open_find(query, window, cx));
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
            None | Some(Overlay::Settings(_)) => {}
            Some(Overlay::Diff(view)) => {
                let (root, old, prev, settled) = {
                    let v = view.read(cx);
                    (v.root.clone(), v.file.clone(), v.layout(), v.structure_settled())
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
                let structural_cache = self.structural_cache.clone();
                let cancel2 = cancel.clone();
                self._overlay_task = Some(cx.spawn(async move |_, cx| {
                    let started = Instant::now();
                    let lines = cx
                        .background_executor()
                        .spawn(async move {
                            let diff = git::diff(&repo, &read_file, || current.load(Ordering::Relaxed) != generation)?;
                            // Same diff as shown: keep its layout (no work, and the view sees no change).
                            let hash = git::layout::hash(&diff);
                            let (layout, took) = match prev.filter(|p| p.hash == hash) {
                                Some(prev) => (prev, None),
                                None => {
                                    let t = Instant::now();
                                    (Arc::new(git::layout::layout(&diff)), Some(t.elapsed()))
                                }
                            };
                            // What each hunk has staged/unstaged (re-read every time: staging doesn't change the
                            // HEAD→worktree diff itself).
                            let rows: Vec<(u32, git::write::Span)> = layout
                                .lines
                                .iter()
                                .enumerate()
                                .filter_map(|(i, l)| Some((i as u32, l.span?)))
                                .collect();
                            let spans: Vec<_> = rows.iter().map(|r| r.1).collect();
                            let states = git::write::hunk_state(&repo, &read_file, &spans);
                            let hunks: HashMap<u32, (bool, bool)> = rows.iter().map(|r| r.0).zip(states).collect();
                            Some((layout, took, Arc::new(hunks)))
                        })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation {
                        return; // a newer read or a close happened meanwhile
                    }
                    crate::perf::event_with(|| match &lines {
                        Some((l, Some(took), _)) => format!(
                            "diff read: {} lines in {:.1} ms (layout {:.1} ms)",
                            l.lines.len(),
                            started.elapsed().as_secs_f64() * 1e3,
                            took.as_secs_f64() * 1e3
                        ),
                        Some((_, None, _)) => "diff read: unchanged, layout reused".into(),
                        None => "diff read failed".into(),
                    });
                    let reused = matches!(lines, Some((_, None, _)));
                    let base = lines.as_ref().map(|(l, _, _)| l.clone());
                    let hunks = lines.as_ref().map(|(_, _, h)| h.clone()).unwrap_or_default();
                    view.update(cx, |v, cx| {
                        v.set_hunks(hunks, cx);
                        v.set_layout(base.clone(), file.clone(), cx)
                    });
                    let Some(base) = base.filter(|_| !syntax_off()) else { return };
                    if reused && settled && base.colored {
                        return; // colors and the structural diff of this exact diff are shown already
                    }
                    // Second pass: syntax colors (both sides parsed as whole files), then one more publish.
                    let started = Instant::now();
                    let (base, colored, sides) = cx
                        .background_executor()
                        .spawn(async move {
                            match diff_syntax::color_diff(&base, &syn_repo, &syn_file, &cache, &cancel) {
                                Some((colored, sides)) => {
                                    let colored = colored.filter(|_| !base.colored).map(Arc::new);
                                    (base, colored, Some(sides))
                                }
                                None => (base, None, None),
                            }
                        })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    if let Some(colored) = &colored {
                        crate::perf::event_with(|| {
                            format!("diff syntax: colored in {:.1} ms", started.elapsed().as_secs_f64() * 1e3)
                        });
                        view.update(cx, |v, cx| v.set_layout(Some(colored.clone()), file.clone(), cx));
                    }
                    // Third pass: the structural diff from the same trees.
                    let base = colored.unwrap_or(base);
                    let started = Instant::now();
                    let structural = cx
                        .background_executor()
                        .spawn(async move {
                            let sides = sides.ok_or(syntax::structural::Fail::Unsupported)?;
                            diff_syntax::structure(&base, &sides, &structural_cache, &cancel2).map(Arc::new)
                        })
                        .await;
                    if current2.load(Ordering::Relaxed) != generation
                        || structural.as_ref().err() == Some(&syntax::structural::Fail::Cancelled)
                    {
                        return;
                    }
                    crate::perf::event_with(|| {
                        let what = match &structural {
                            Ok(_) => "ok".to_string(),
                            Err(e) => format!("{e:?}"),
                        };
                        format!("diff structural: {what} in {:.1} ms", started.elapsed().as_secs_f64() * 1e3)
                    });
                    view.update(cx, |v, cx| v.set_structural(structural, cx));
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
                        format!(
                            "file syntax: {} in {:.1} ms",
                            if colors.is_some() { "colored" } else { "plain" },
                            started.elapsed().as_secs_f64() * 1e3
                        )
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
        let missing: Vec<PathBuf> = wanted
            .into_iter()
            .filter(|d| !self.tree_cache.contains_key(d) && !self.tree_pending.contains_key(d))
            .collect();
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
                            format!(
                                "listed {} ({} entries) in {:.1} ms",
                                dir.display(),
                                listing.entries.len(),
                                started.elapsed().as_secs_f64() * 1e3
                            )
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
                crate::perf::event_with(|| {
                    format!("tree flatten: {} rows in {:.2} ms", rows.len(), started.elapsed().as_secs_f64() * 1e3)
                });
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
            agent: self.tracker.view(t.id as u32),
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
                self.change_tree(
                    |t| {
                        t.expanded.insert(p.clone());
                    },
                    cx,
                );
                self.request_listing(p, cx); // fresh on every expand (ignored dirs are never watched)
            }
            Action::Collapse(p) => self.change_tree(
                |t| {
                    t.expanded.remove(&p);
                },
                cx,
            ),
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
            Some(Overlay::Settings(v)) => Some(v.focus_handle(cx)),
            None => self.tabs.get(self.active).and_then(|t| t.view.as_ref()).map(|v| v.focus_handle(cx)),
        };
        if let Some(f) = focus {
            window.focus(&f, cx);
        }
    }

    /// Ctrl+Shift+G / Ctrl+Shift+E: show that panel in whichever dock holds it (Files also takes focus); again
    /// collapses that dock.
    fn show_panel_tab(&mut self, tab: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let files_focused = self.files_panel.focus_handle(cx).is_focused(window);
        if self.panel_visible(tab) && (tab != Panel::Files || files_focused) {
            self.set_dock_open(self.settings.layout.dock_of(tab), false, cx);
            self.focus_main(window, cx);
        } else {
            self.show_panel(tab, cx);
            if tab == Panel::Files {
                let f = self.files_panel.focus_handle(cx);
                window.focus(&f, cx);
            }
        }
    }

    /// Shows `tab` in its dock (never hides it).
    fn reveal_panel_tab(&mut self, tab: Panel, cx: &mut Context<Self>) {
        self.show_panel(tab, cx);
    }

    /// Opens `p`'s dock on it.
    fn show_panel(&mut self, p: Panel, cx: &mut Context<Self>) {
        let d = self.settings.layout.dock_of(p);
        self.set_dock_open(d, true, cx);
        self.set_panel_tab(p, cx);
    }

    fn set_dock_open(&mut self, d: Dock, open: bool, cx: &mut Context<Self>) {
        if self.docks[d.ix()].open != open {
            self.docks[d.ix()].open = open;
            self.pr_poll_check(cx);
            self.flush_resize = true;
            self.mark_dirty(cx);
            cx.notify();
        }
    }

    /// The panel's dock is open and shows it.
    fn panel_visible(&self, p: Panel) -> bool {
        let d = self.settings.layout.dock_of(p);
        self.docks[d.ix()].open && self.dock_shown(d) == Some(p)
    }

    /// What dock `d` shows (None: no panel lives there).
    fn dock_shown(&self, d: Dock) -> Option<Panel> {
        self.settings.layout.shown(d, self.docks[d.ix()].active)
    }

    /// Drawn width at 100 %: the live width, never under its panels' minimum.
    fn dock_width(&self, d: Dock) -> f32 {
        self.docks[d.ix()].width.max(self.settings.layout.min_width(d))
    }

    /// Width the dock takes beside the terminal (at 100 %).
    fn dock_taken(&self, d: Dock) -> f32 {
        let u = self.docks[d.ix()];
        self.settings.layout.taken(d, u.open, u.width, DIVIDER, COLLAPSED)
    }

    /// A tab dragged (or "Move to … dock") to `to`: saved in settings.json, shown there. One relayout, one PTY resize.
    pub(super) fn move_panel(&mut self, p: Panel, to: Dock, cx: &mut Context<Self>) {
        self.panel_drag = false;
        if self.settings.layout.dock_of(p) != to {
            self.change_settings(|s| s.layout.set_dock(p, to), cx);
        }
        self.show_panel(p, cx);
        cx.notify();
    }

    /// A Search panel result: the file in the preview, at the hit's line with the matched text marked.
    pub fn open_search_hit(
        &mut self,
        path: PathBuf,
        hit: Option<(u32, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(path, false, window, cx);
        if let (Some((line, text)), Some(Overlay::File(f))) = (hit, &self.overlay) {
            f.update(cx, |f, cx| f.go_to_match(line as usize, text, cx));
        }
    }

    fn set_panel_tab(&mut self, tab: Panel, cx: &mut Context<Self>) {
        let d = self.settings.layout.dock_of(tab);
        if self.docks[d.ix()].active != tab {
            self.docks[d.ix()].active = tab;
            self.pr_poll_check(cx);
            self.mark_dirty(cx);
            cx.notify();
        }
    }
}

impl Workspace {
    /// Mouse up: a divider drag ends (one PTY resize, the width saved), a panel drag that hit no dock is dropped.
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.panel_drag) {
            cx.notify();
        }
        if self.dragging {
            self.dragging = false;
            self.flush_resize = true;
            let widths = self.docks.map(|d| d.width.round() as u32);
            self.change_settings(
                |s| {
                    s.layout.left_width = widths[0].clamp(layout::WIDTH.0, layout::WIDTH.1);
                    s.layout.right_width = widths[1].clamp(layout::WIDTH.0, layout::WIDTH.1);
                },
                cx,
            );
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

/// `dir/.git` is a file (a linked worktree): its main checkout, from `gitdir: <main>/.git/worktrees/<name>`.
fn main_checkout(dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(".git")).ok()?;
    let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() { gitdir } else { dir.join(gitdir) };
    let common = gitdir.parent()?.parent()?; // <main>/.git
    (gitdir.parent()?.file_name()? == "worktrees" && common.file_name()? == ".git")
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten()
}

/// Pinned projects without tabs become empty groups after the tab groups (before "Other").
fn with_pinned(
    mut groups: Vec<(Option<PathBuf>, Vec<usize>)>,
    pinned: &[PathBuf],
) -> Vec<(Option<PathBuf>, Vec<usize>)> {
    let at = groups.iter().position(|(k, _)| k.is_none()).unwrap_or(groups.len());
    let empty: Vec<_> = pinned
        .iter()
        .filter(|p| !groups.iter().any(|(k, _)| k.as_ref() == Some(*p)))
        .map(|p| (Some(p.clone()), Vec::new()))
        .collect();
    groups.splice(at..at, empty);
    groups
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub(crate) fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
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
pub(crate) fn sh_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The "Session N" every tab got before automatic names: not a name the user chose.
fn is_default_name(name: &str) -> bool {
    name.strip_prefix("Session ").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{fmt_remaining, group_order, is_default_name, reorder, sh_word, with_pinned};
    use std::path::PathBuf;

    #[test]
    fn old_default_names_become_automatic() {
        assert!(is_default_name("Session 1") && is_default_name("Session 42"));
        for n in ["Session", "Session ", "Session 1a", "session 1", "API refactor", "Session 1 fix"] {
            assert!(!is_default_name(n), "{n}");
        }
    }

    #[test]
    fn reorder_moves_only_within_the_group_slots() {
        // Tabs 0..6; the group holds slots 1, 3, 4 (the others belong to other groups).
        let slots = [1, 3, 4];
        // Down: the first member to the end; its slot (the group's first) is taken by the next member.
        assert_eq!(reorder(6, &slots, 0, 2), vec![0, 3, 2, 4, 1, 5]);
        // Up: the last member to the top.
        assert_eq!(reorder(6, &slots, 2, 0), vec![0, 4, 2, 1, 3, 5]);
        // No move.
        assert_eq!(reorder(6, &slots, 1, 1), (0..6).collect::<Vec<_>>());
        // Other groups' slots never change, and the group's first slot stays its own: group order is kept.
        let order = reorder(6, &slots, 0, 2);
        for k in [0, 2, 5] {
            assert_eq!(order[k], k);
        }
        assert!(slots.contains(&order[1]));
        // Group order survives: first-occurrence order of the keys is the same before and after.
        let keys = [
            Some(PathBuf::from("/b")),
            Some(PathBuf::from("/a")),
            None,
            Some(PathBuf::from("/a")),
            Some(PathBuf::from("/a")),
            Some(PathBuf::from("/b")),
        ];
        let after: Vec<_> = order.iter().map(|&j| keys[j].clone()).collect();
        let firsts = |k: &[Option<PathBuf>]| group_order(k).into_iter().map(|(g, _)| g).collect::<Vec<_>>();
        assert_eq!(firsts(&keys), firsts(&after));
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
    fn pinned_projects_without_tabs_are_empty_groups_before_other() {
        let (a, p) = (PathBuf::from("/a"), PathBuf::from("/p"));
        let groups = with_pinned(group_order(&[None, Some(a.clone())]), &[a.clone(), p.clone()]);
        assert_eq!(groups, vec![(Some(a), vec![1]), (Some(p.clone()), vec![]), (None, vec![0])], "a has tabs: once");
        assert_eq!(with_pinned(Vec::new(), std::slice::from_ref(&p)), vec![(Some(p), vec![])]);
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
