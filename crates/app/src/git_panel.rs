//! Read-only git UI: the repo store (one watcher per repo, shared by its tabs), the right-side changes panel and
//! the diff view shown over the terminal. Both views are separate entities rendered with `.cached(..)` and only
//! virtualized lists (`uniform_list`): a 10k-line diff shapes only the rows on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use git::layout::{DLine, Engine, Layout, SplitRow};
use git::{FileChange, LineKind, Status};
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, StyledText, UniformListScrollHandle,
    WeakEntity, Window, div, prelude::*, uniform_list,
};

use crate::scrollbar::{self, scrollbar};
use crate::settings::{DiffEngine, DiffMode, SplitStyle};
use crate::theme::{self, MONO_FONT, height, radius, space, text_size, z};
use crate::ui::{self, icon, line_counts, tip};
use crate::workspace::Workspace;
use syntax::structural::Fail;

/// One watcher per repo root, refcounted by the tabs whose cwd is in it. The last tab leaving drops it (and its
/// inotify watches).
pub struct Repos {
    map: HashMap<PathBuf, Entry>,
    /// Pinged (bounded(1), coalescing) after any watcher's refresh.
    ping: Sender<()>,
    /// Directories whose listing changed (from all repo watchers and the app's `DirWatcher`), for file trees.
    dirty: Arc<Mutex<HashSet<PathBuf>>>,
    /// Gets every status (per-agent changed files), on the watcher threads.
    tracker: Arc<crate::changes::Tracker>,
}

struct Entry {
    repo: git::Repo,
    watcher: git::Watcher,
    tabs: usize,
    /// The repo's file list (quick open, content search): built on first use, then patched on the watcher thread
    /// from each update's changed directories.
    index: IndexSlot,
}

/// None until first used.
pub type IndexSlot = Arc<Mutex<Option<git::index::Index>>>;

impl Repos {
    pub fn new(tracker: Arc<crate::changes::Tracker>) -> (Self, Receiver<()>) {
        let (ping, rx) = async_channel::bounded(1);
        (Self { map: HashMap::new(), ping, dirty: Default::default(), tracker }, rx)
    }

    pub fn acquire(&mut self, repo: git::Repo) {
        if let Some(e) = self.map.get_mut(&repo.root) {
            e.tabs += 1;
            return;
        }
        let report = self.reporter();
        let (tracker, root) = (self.tracker.clone(), repo.root.clone());
        let index: IndexSlot = Default::default();
        let ix = index.clone();
        let on_update = move |u: git::Update| {
            tracker.status(&root, u.status, u.settled);
            if !u.dirs.is_empty()
                && let Some(ix) = ix.lock().unwrap().as_mut()
            {
                ix.refresh(&u.dirs);
            }
            // Batches of only ignored paths are reported for the tracker; the UI has nothing to do then.
            if u.refreshed || !u.dirs.is_empty() {
                report(u.dirs);
            }
        };
        match git::Watcher::start(repo.clone(), on_update) {
            Ok(watcher) => {
                self.tracker.add_repo(&repo.root, watcher.received());
                self.map.insert(repo.root.clone(), Entry { repo, watcher, tabs: 1, index });
            }
            Err(e) => eprintln!("dirigent: can't watch {}: {e}", repo.root.display()),
        }
    }

    pub fn release(&mut self, root: &Path) {
        if let Some(e) = self.map.get_mut(root) {
            e.tabs -= 1;
            if e.tabs == 0 {
                self.map.remove(root);
                self.tracker.remove_repo(root);
            }
        }
    }

    /// A callback that records changed directories and pings the UI (for watcher threads).
    pub fn reporter(&self) -> impl Fn(Vec<PathBuf>) + Send + 'static {
        let (ping, dirty) = (self.ping.clone(), self.dirty.clone());
        move |dirs| {
            if !dirs.is_empty() {
                dirty.lock().unwrap().extend(dirs);
            }
            let _ = ping.try_send(());
        }
    }

    pub fn take_dirty_dirs(&self) -> Vec<PathBuf> {
        self.dirty.lock().unwrap().drain().collect()
    }

    pub fn status(&self, root: &Path) -> Option<Arc<Status>> {
        self.map.get(root)?.watcher.latest()
    }

    /// The repo's file index slot (None: no watcher for that root).
    pub fn index(&self, root: &Path) -> Option<IndexSlot> {
        self.map.get(root).map(|e| e.index.clone())
    }

    pub fn repo(&self, root: &Path) -> Option<&git::Repo> {
        self.map.get(root).map(|e| &e.repo)
    }

    /// Changes whenever the repo's `git status` ran again (an open diff re-reads then).
    pub fn refreshes(&self, root: &Path) -> u64 {
        self.map.get(root).map_or(0, |e| e.watcher.refreshes())
    }

    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.map.keys()
    }

    pub fn refresh(&self, root: &Path) {
        if let Some(e) = self.map.get(root) {
            e.watcher.refresh();
        }
    }

    #[cfg(test)]
    fn watched(&self) -> Vec<(PathBuf, usize)> {
        let mut v: Vec<_> = self.map.iter().map(|(k, e)| (k.clone(), e.tabs)).collect();
        v.sort();
        v
    }
}

/// What the panel shows for the active tab.
#[derive(Clone)]
pub enum PanelView {
    NoSession,
    /// Repo discovery still running (or the tab isn't spawned).
    Pending,
    NotRepo,
    GitMissing,
    /// `agent`: the tab's agent ran (its changed files and the This agent / All choice).
    Repo {
        root: PathBuf,
        status: Option<Arc<Status>>,
        agent: Option<AgentFiles>,
        /// Git writes: running / failed (with retry).
        ops: Box<crate::git_ops::OpsView>,
        commit: Option<gpui::Entity<crate::commit_box::CommitBox>>,
        /// The branch's PR/MR strip (None until looked up).
        pr: Option<Box<crate::pr::View>>,
    },
}

#[derive(Clone)]
pub struct AgentFiles {
    pub view: Arc<crate::changes::View>,
    /// "This agent" selected (else All).
    pub only: bool,
}

impl PanelView {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Repo { root: a, status: sa, agent: ga, ops: oa, commit: ca, pr: pa },
                Self::Repo { root: b, status: sb, agent: gb, ops: ob, commit: cb, pr: pb },
            ) => {
                a == b
                    && oa == ob
                    && pa == pb
                    && ca.as_ref().map(|c| c.entity_id()) == cb.as_ref().map(|c| c.entity_id())
                    && match (sa, sb) {
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        (None, None) => true,
                        _ => false,
                    }
                    && match (ga, gb) {
                        (Some(x), Some(y)) => Arc::ptr_eq(&x.view, &y.view) && x.only == y.only,
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

pub struct GitPanel {
    view: PanelView,
    /// Path of the file whose diff is open.
    selected: Option<String>,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    workspace: WeakEntity<Workspace>,
}

/// The panel can't be dragged narrower than this.
pub const MIN_WIDTH: f32 = 240.;

/// Flex shrink weight: within a row the directory gives up its width long before the file name does.
pub fn shrink<E: Styled>(mut e: E, weight: f32) -> E {
    e.style().flex_shrink = Some(weight);
    e
}

impl GitPanel {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            view: PanelView::Pending,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            workspace,
        }
    }

    /// Re-renders only if something it shows changed.
    pub fn set(&mut self, view: PanelView, selected: Option<String>, cx: &mut Context<Self>) {
        if !view.same(&self.view) || selected != self.selected {
            self.view = view;
            self.selected = selected;
            cx.notify();
        }
    }

    fn row(
        &self,
        ix: usize,
        root: &Path,
        st: &Status,
        marks: Option<&crate::changes::View>,
        agent_dot: bool,
    ) -> gpui::AnyElement {
        use crate::git_ops::Job;
        let t = theme::get();
        let Some(f) = st.files.get(ix) else {
            return ui::note(format!("+{} more files not listed", st.more)).h(z(height::LIST_ROW)).into_any_element();
        };
        let (name, dir) = match f.path.rsplit_once('/') {
            Some((d, n)) => (n.to_string(), format!("{d}/")),
            None => (f.path.clone(), String::new()),
        };
        let dir = match &f.orig {
            Some(o) => format!("{dir} ← {o}"),
            None => dir,
        };
        let selected = self.selected.as_deref() == Some(f.path.as_str());
        let (file, root, ws) = (f.clone(), root.to_path_buf(), self.workspace.clone());
        // The checkbox is the index: checked = staged, a dash = partly staged. Click: stage the rest / unstage.
        let check = {
            let (ws, root, f) = (self.workspace.clone(), root.clone(), f.clone());
            let state = match (f.staged, f.unstaged) {
                (true, false) => ui::Check::On,
                (true, true) => ui::Check::Partial,
                _ => ui::Check::Off,
            };
            div()
                .id(("stage", ix))
                .flex_none()
                .p(z(2.))
                .cursor_pointer()
                .child(ui::checkbox(state))
                .tooltip(tip(match state {
                    ui::Check::On => "Staged · click to unstage",
                    ui::Check::Partial => "Partly staged · click to stage the rest",
                    ui::Check::Off => "Click to stage",
                }))
                .on_click(move |_: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let job = if state == ui::Check::On {
                        Job::Unstage(vec![f.clone()])
                    } else {
                        Job::Stage(vec![f.clone()])
                    };
                    let _ = ws.update(cx, |w, cx| w.run_git(root.clone(), job, cx));
                })
        };
        let discard = {
            let (ws, root, f) = (self.workspace.clone(), root.to_path_buf(), f.clone());
            div()
                .id(("discard", ix))
                .flex_none()
                .invisible()
                .group_hover("git-row", |s| s.visible())
                .p(z(2.))
                .rounded(z(radius::SM))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg.elevated))
                .child(ui::icon(icon::TRASH, 12., t.text.muted))
                .tooltip(tip("Discard changes…"))
                .on_click(move |_: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let job = Job::Discard(f.clone());
                    let _ = ws.update(cx, |w, cx| w.confirm_discard(root.clone(), job, window, cx));
                })
        };
        let kind_color = theme::git_kind(f.kind);
        let mark = marks.and_then(|m| m.files.get(&f.path));
        div()
            .id(("git-file", ix))
            .group("git-row")
            .w_full() // a uniform_list row is laid out as a root: without this it takes its min-content width
            .overflow_hidden()
            .h(z(height::LIST_ROW))
            .pl(z(space::MD))
            .pr(z(14.)) // room for the scrollbar
            .flex()
            .items_center()
            .gap(z(space::SM))
            .cursor_pointer()
            .text_size(z(text_size::MD))
            .when(selected, |d| d.bg(t.bg.selected))
            .when(!selected, |d| d.hover(|s| s.bg(t.bg.hover)))
            // Not `cx.listener`: the workspace updates this panel in response, which must not happen while the
            // panel itself is being updated (gpui panics on re-entrant updates).
            .on_click(move |_: &ClickEvent, window, cx| {
                let (root, file) = (root.clone(), file.clone());
                let _ = ws.update(cx, |w, cx| w.toggle_diff(root, file, window, cx));
            })
            .child(check)
            .child(ui::icon(ui::file_icon(&name), 14., kind_color))
            // Name and directory share what's left; counts never shrink. The directory truncates first.
            .child(shrink(div().min_w_0().truncate().text_color(t.text.primary), 1.).child(name))
            .child(
                shrink(
                    div().flex_grow(1.).min_w_0().truncate().text_size(z(text_size::XS)).text_color(t.text.dim),
                    10_000.,
                )
                .child(dir),
            )
            .children(mark.map(|m| {
                let mark = |id: &'static str, path: &'static str, hint: &'static str| {
                    div().id((id, ix)).flex_none().child(ui::icon(path, 12., t.text.dim)).tooltip(tip(hint))
                };
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .when(m.shared, |d| d.child(mark("mark-shared", icon::SHARED, "Also changed by another session")))
                    .when(m.via_command, |d| {
                        d.child(mark("mark-cmd", icon::VIA_COMMAND, "Changed by a shell command (best effort)"))
                    })
                    .when(agent_dot, |d| {
                        d.child(
                            div()
                                .id(("mark-agent", ix))
                                .size(z(5.))
                                .rounded_full()
                                .bg(t.accent)
                                .tooltip(tip("Changed by this session's agent")),
                        )
                    })
            }))
            .child(discard)
            .child(div().flex_none().text_size(z(text_size::XS)).child(line_counts(f.lines)))
            .into_any_element()
    }
}

/// A small bordered button for the panel's bars.
fn bar_button(id: &'static str) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(z(space::XS))
        .h(z(24.))
        .px(z(space::SM))
        .rounded(z(radius::MD))
        .bg(t.bg.elevated)
        .border_1()
        .border_color(t.border.subtle)
        .text_size(z(text_size::SM))
        .text_color(t.text.primary)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg.hover))
}

impl GitPanel {
    /// Branch (opens the switcher), the Sync button (its label is the safe next step) and the ⋯ menu.
    fn repo_bar(&self, root: &Path, st: &Status, busy: bool) -> impl IntoElement + use<> {
        use crate::git_ops::Job;
        let t = theme::get();
        let detached = st.branch.starts_with("(detached");
        // An upstream without ahead/behind: its remote branch was deleted (usually merged) and pruned.
        let gone = st.upstream.is_some() && st.ab.is_none();
        // (label, counts, hint, emphasized)
        let (label, counts, hint, strong): (&'static str, String, &'static str, bool) = match (&st.upstream, st.ab) {
            _ if detached => ("Detached", String::new(), "Not on a branch: switch to one to sync", false),
            (Some(_), None) => (
                "Remote deleted",
                String::new(),
                "Remote branch deleted (merged?): it's gone from the remote, usually after its pull/merge request \
                 was merged. Nothing is pushed by itself. Click for: switch to the default branch and pull, or \
                 publish again (asks first).",
                false,
            ),
            (None, _) => ("Publish", String::new(), "Push this branch to the remote and track it (push -u)", true),
            (Some(_), Some((0, 0))) => ("Up to date", String::new(), "Nothing to push or pull · click to fetch", false),
            (Some(_), Some((a, 0))) => ("Push", format!("↑{a}"), "Fetch, then push your commits", true),
            (Some(_), Some((0, b))) => ("Pull", format!("↓{b}"), "Fetch, then fast-forward to the upstream", true),
            (Some(_), Some((a, b))) => (
                "Diverged",
                format!("↑{a} ↓{b}"),
                "Both sides have new commits: Sync fetches and explains; merge or rebase in a terminal",
                false,
            ),
        };
        let enabled = !detached && !busy;
        let (ws, ws2, ws3, r, r2) = (
            self.workspace.clone(),
            self.workspace.clone(),
            self.workspace.clone(),
            root.to_path_buf(),
            root.to_path_buf(),
        );
        let sync = bar_button("git-sync")
            .when(strong && enabled, |d| {
                d.bg(t.accent).border_color(t.accent).text_color(t.text.on_accent).hover(|s| s.opacity(0.85))
            })
            .when(!enabled, |d| d.text_color(t.text.faint).cursor_not_allowed())
            .when(label == "Diverged" || gone, |d| d.text_color(t.state.needs_you))
            .child(label)
            .when(!counts.is_empty(), |d| d.child(div().text_size(z(text_size::XS)).opacity(0.8).child(counts)))
            .tooltip(tip(hint))
            .on_click(move |ev: &ClickEvent, _, cx| {
                let at = ev.position();
                if gone {
                    // Nothing is pushed by itself: the choices are in the ⋯ menu.
                    let _ = ws.update(cx, |w, cx| w.open_git_menu(r.clone(), crate::workspace::GitMenu::More, at, cx));
                } else if enabled {
                    let _ = ws.update(cx, |w, cx| w.run_git(r.clone(), Job::Sync, cx));
                }
            });
        div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .px(z(space::MD))
            .py(z(space::SM))
            .child(
                bar_button("git-branch")
                    .flex_1()
                    .min_w_0()
                    .child(ui::icon(icon::BRANCH, 13., t.accent))
                    .child(div().flex_1().min_w_0().truncate().child(st.branch.clone()))
                    .child(ui::icon(icon::CHEVRON_DOWN, 12., t.text.dim))
                    .tooltip(tip("Switch or create a branch (Ctrl+Shift+O)"))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        let _ = ws2.update(cx, |w, cx| w.open_branches(window, cx));
                    }),
            )
            .child(sync)
            .child(
                bar_button("git-more")
                    .px(z(space::XS))
                    .child(ui::icon(icon::MORE, 14., t.text.muted))
                    .tooltip(tip("Fetch, pull, push, PR status, forget the agent's files"))
                    .on_click(move |ev: &ClickEvent, _, cx| {
                        let at = ev.position();
                        let _ = ws3
                            .update(cx, |w, cx| w.open_git_menu(r2.clone(), crate::workspace::GitMenu::More, at, cx));
                    }),
            )
    }

    /// The running job (with Cancel for network jobs), else the last note (click to dismiss).
    fn status_line(&self, root: &Path, ops: &crate::git_ops::OpsView) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let (ws, r) = (self.workspace.clone(), root.to_path_buf());
        let line = || {
            div().flex().items_center().gap(z(space::SM)).px(z(space::LG)).pb(z(space::SM)).text_size(z(text_size::XS))
        };
        if let Some(label) = &ops.running {
            return Some(
                line()
                    .text_color(t.text.muted)
                    .child(ui::icon(icon::REFRESH, 11., t.text.muted))
                    .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                    .when(ops.cancellable, |d| {
                        d.child(
                            div()
                                .id("git-cancel")
                                .flex_none()
                                .cursor_pointer()
                                .text_color(t.accent)
                                .hover(|s| s.text_color(t.text.primary))
                                .child("Cancel")
                                .tooltip(tip("Stop it (git and its ssh are killed; nothing is half-written)"))
                                .on_click(move |_: &ClickEvent, _, cx| {
                                    let _ = ws.update(cx, |w, cx| w.cancel_git(r.clone(), cx));
                                }),
                        )
                    })
                    .into_any_element(),
            );
        }
        let note = ops.note.clone()?;
        Some(
            line()
                .text_color(t.text.muted)
                .child(div().flex_1().min_w_0().child(note))
                .child(
                    div()
                        .id("git-note-x")
                        .flex_none()
                        .cursor_pointer()
                        .child(ui::icon(icon::CLOSE, 11., t.text.dim))
                        .tooltip(tip("Dismiss"))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws.update(cx, |w, cx| w.dismiss_git_note(r.clone(), cx));
                        }),
                )
                .into_any_element(),
        )
    }

    /// The branch's PR/MR: number, state and checks with View, or Create, or a link when gh/glab aren't there.
    /// `enabled`: no job running (Create is shown but inactive meanwhile, so the strip doesn't flicker).
    /// `gone`: the remote branch was deleted: no Create (it would publish the branch again).
    fn pr_strip(&self, root: &Path, pr: &crate::pr::View, enabled: bool, gone: bool) -> Option<gpui::AnyElement> {
        use crate::pr::{Checks, Info, State};
        let t = theme::get();
        let noun = pr.noun();
        let strip = || {
            div()
                .mx(z(space::MD))
                .mb(z(space::SM))
                .px(z(space::SM))
                .h(z(28.))
                .flex()
                .items_center()
                .gap(z(space::SM))
                .rounded(z(radius::MD))
                .bg(t.bg.input)
                .text_size(z(text_size::XS))
        };
        let link = |id: &'static str, label: String, url: String| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .gap(z(space::XXS))
                .cursor_pointer()
                .text_color(t.accent)
                .hover(|s| s.text_color(t.text.primary))
                .child(label)
                .child(ui::icon(icon::EXTERNAL, 11., t.accent))
                .on_click(move |_: &ClickEvent, _, cx| cx.open_url(&url))
        };
        let (ws, r) = (self.workspace.clone(), root.to_path_buf());
        let create = move || {
            bar_button("pr-create")
                .h(z(22.))
                .text_size(z(text_size::XS))
                .when(!enabled, |d| d.text_color(t.text.faint).cursor_not_allowed())
                .child(ui::icon(icon::PULL_REQUEST, 12., t.text.muted))
                .child(if noun == "MR" { "Create MR…" } else { "Create PR…" })
                .tooltip(tip(
                    "Push if needed, then open the new request (gh, GitLab push options, or the browser form)",
                ))
                .on_click(move |_: &ClickEvent, _, cx| {
                    if enabled {
                        let _ = ws.update(cx, |w, cx| w.run_git(r.clone(), crate::git_ops::Job::CreatePr, cx));
                    }
                })
        };
        match &pr.info {
            Info::Hidden => None,
            Info::Known(Some(p)) => {
                let (state, color) = match p.state {
                    State::Open => ("Open", t.state.done),
                    State::Draft => ("Draft", t.text.muted),
                    State::Merged => ("Merged", t.git.renamed),
                    State::Closed => ("Closed", t.state.error),
                };
                let checks = match p.checks {
                    _ if matches!(p.state, State::Merged | State::Closed) => None,
                    Checks::None => None,
                    Checks::Passing => Some(("checks passing", t.text.dim)),
                    Checks::Pending => Some(("checks running", t.state.working)),
                    Checks::Failing => Some(("checks failing", t.state.error)),
                };
                let closed = matches!(p.state, State::Merged | State::Closed);
                Some(
                    strip()
                        .child(ui::icon(icon::PULL_REQUEST, 13., color))
                        .child(div().flex_none().text_color(t.text.primary).child(format!("#{}", p.number)))
                        .child(div().flex_none().text_color(color).child(state))
                        .children(checks.map(|(c, col)| div().min_w_0().truncate().text_color(col).child(c)))
                        .child(div().flex_1())
                        .when(closed && !gone, |d| d.child(create()))
                        .child(link("pr-view", "View".into(), p.url.clone()))
                        .into_any_element(),
                )
            }
            Info::Known(None) if gone => None,
            Info::Known(None) => Some(
                strip()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.text.dim)
                            .child(format!("No {noun} for this branch")),
                    )
                    .child(create())
                    .into_any_element(),
            ),
            Info::Link(url) => Some(
                strip()
                    .child(div().flex_1())
                    .when(!gone, |d| d.child(create()))
                    .child(link(
                        "pr-list",
                        if noun == "MR" { "View merge requests".into() } else { "View pull requests".into() },
                        url.clone(),
                    ))
                    .into_any_element(),
            ),
        }
    }

    /// A failed git write: its message (hook output scrolls), Retry when it can help, dismiss.
    fn error_banner(&self, root: &Path, e: crate::git_ops::OpError) -> impl IntoElement + use<> {
        let t = theme::get();
        let (ws, ws2, r, r2) = (self.workspace.clone(), self.workspace.clone(), root.to_path_buf(), root.to_path_buf());
        div()
            .mx(z(space::MD))
            .mb(z(space::SM))
            .p(z(space::SM))
            .flex()
            .flex_col()
            .gap(z(space::XS))
            .rounded(z(radius::MD))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.state.error)
            .text_size(z(text_size::XS))
            .child(
                div()
                    .id("git-error-text")
                    .max_h(z(160.))
                    .overflow_y_scroll()
                    .font_family(MONO_FONT)
                    .text_color(t.text.primary)
                    .children(e.message.lines().map(|l| div().child(l.to_string()))),
            )
            .child(
                div()
                    .flex()
                    .gap(z(space::MD))
                    .children(e.retry.is_some().then(|| {
                        div()
                            .id("git-retry")
                            .cursor_pointer()
                            .text_color(t.accent)
                            .hover(|s| s.text_color(t.text.primary))
                            .child("Retry")
                            .on_click(move |_: &ClickEvent, _, cx| {
                                let _ = ws.update(cx, |w, cx| w.retry_git(r.clone(), cx));
                            })
                    }))
                    .child(
                        div()
                            .id("git-dismiss")
                            .cursor_pointer()
                            .text_color(t.text.muted)
                            .hover(|s| s.text_color(t.text.primary))
                            .child("Dismiss")
                            .on_click(move |_: &ClickEvent, _, cx| {
                                let _ = ws2.update(cx, |w, cx| w.dismiss_git_error(r2.clone(), cx));
                            }),
                    ),
            )
    }

    /// This agent / All with their file counts, in a tab whose agent ran.
    fn scope_switch(&self, only: bool, agent_n: usize, all_n: usize) -> impl IntoElement + use<> {
        let (ws, ws2) = (self.workspace.clone(), self.workspace.clone());
        div().px(z(space::MD)).pb(z(space::SM)).flex().child(
            segmented()
                .child(
                    segment("agent-only", format!("This agent  {agent_n}"), only, true)
                        .tooltip(tip("Files this session's agent changed"))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws.update(cx, |w, cx| w.set_agent_filter(true, cx));
                        }),
                )
                .child(
                    segment("agent-all", format!("All  {all_n}"), !only, true)
                        .tooltip(tip("Every change in the repo"))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws2.update(cx, |w, cx| w.set_agent_filter(false, cx));
                        }),
                ),
        )
    }

    /// The list's header: a checkbox for every listed file (stage the rest / unstage all), counts, staged count.
    /// `whole`: the list is the whole repo (All), so staging is `git add -A` (also files past the list cap).
    /// `only`: the listed files' indices (This agent); None = every file (All).
    fn list_header(
        &self,
        root: &Path,
        st: Arc<Status>,
        only: Option<Arc<Vec<usize>>>,
        add: u64,
        del: u64,
    ) -> impl IntoElement + use<> {
        use crate::git_ops::Job;
        let t = theme::get();
        let whole = only.is_none();
        let files = || -> Box<dyn Iterator<Item = &FileChange> + '_> {
            match &only {
                Some(ix) => Box::new(ix.iter().filter_map(|&i| st.files.get(i))),
                None => Box::new(st.files.iter()),
            }
        };
        // Counted here without cloning; the click clones what it stages.
        let staged = files().filter(|f| f.staged).count();
        let state = if staged == 0 {
            ui::Check::Off
        } else if files().all(|f| f.staged && !f.unstaged) {
            ui::Check::On
        } else {
            ui::Check::Partial
        };
        let n = files().count();
        // The click works out its files from the same Arcs (nothing is cloned per render).
        let listed = move || -> Vec<FileChange> {
            match &only {
                Some(ix) => ix.iter().filter_map(|&i| st.files.get(i).cloned()).collect(),
                None => st.files.clone(),
            }
        };
        let (ws, r) = (self.workspace.clone(), root.to_path_buf());
        let clamp = |v: u64| v.min(u32::MAX as u64) as u32;
        div()
            .flex()
            .items_center()
            .gap(z(space::SM))
            .px(z(space::MD))
            .pb(z(space::XS))
            .text_size(z(text_size::XS))
            .text_color(t.text.muted)
            .child(
                div()
                    .id("stage-listed")
                    .flex_none()
                    .p(z(2.))
                    .when(n > 0, |d| d.cursor_pointer())
                    .child(ui::checkbox(state))
                    .tooltip(tip(if state == ui::Check::On {
                        "Unstage all listed files"
                    } else {
                        "Stage all listed files"
                    }))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if n == 0 {
                            return;
                        }
                        let job = if state == ui::Check::On {
                            Job::Unstage(listed())
                        } else if whole {
                            Job::StageAll
                        } else {
                            Job::Stage(listed().into_iter().filter(|f| f.unstaged).collect())
                        };
                        let _ = ws.update(cx, |w, cx| w.run_git(r.clone(), job, cx));
                    }),
            )
            .child(div().flex_none().child(format!("{n} file{}", if n == 1 { "" } else { "s" })))
            .when(n > 0, |d| d.child(line_counts(Some((clamp(add), clamp(del))))))
            .child(div().flex_1())
            .when(staged > 0, |d| d.child(div().flex_none().text_color(t.text.dim).child(format!("{staged} staged"))))
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("git panel render");
        let t = theme::get();
        let body = match &self.view {
            PanelView::NoSession => ui::note("No session").into_any_element(),
            PanelView::Pending => ui::note("…").into_any_element(),
            PanelView::NotRepo => ui::note("Not a git repository").into_any_element(),
            PanelView::GitMissing => ui::note("git not found on PATH").into_any_element(),
            PanelView::Repo { status: None, .. } => ui::note("Reading status…").into_any_element(),
            PanelView::Repo { root, status: Some(st), agent, ops, commit, pr } => {
                let (root, st) = (root.clone(), st.clone());
                let busy = ops.running.is_some();
                let error = ops.error.clone().map(|e| self.error_banner(&root, e));
                // This agent: only its files (indices into the status); All: everything.
                let agent_ix: Option<Vec<usize>> = agent.as_ref().map(|a| {
                    st.files
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| a.view.files.contains_key(&f.path))
                        .map(|(i, _)| i)
                        .collect()
                });
                let only: Option<Arc<Vec<usize>>> =
                    agent.as_ref().filter(|a| a.only).and_then(|_| agent_ix.clone()).map(Arc::new);
                let (_, add, del) = match (&only, agent) {
                    (Some(_), Some(a)) => a.view.summary.unwrap_or_default(),
                    _ => st.totals(),
                };
                let marks = agent.as_ref().map(|a| a.view.clone());
                // In All, the agent's files get a dot (in This agent every row is one).
                let dot = only.is_none();
                let warning = st.watch_error.clone().map(|e| {
                    let (root, ws) = (root.clone(), self.workspace.clone());
                    div()
                        .id("git-refresh")
                        .mx(z(space::MD))
                        .mb(z(space::SM))
                        .px(z(space::MD))
                        .py(z(space::XS))
                        .flex()
                        .gap(z(space::SM))
                        .rounded(z(radius::MD))
                        .bg(t.bg.elevated)
                        .text_size(z(text_size::XS))
                        .text_color(t.state.needs_you)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.bg.hover))
                        .child(ui::icon(icon::REFRESH, 12., t.state.needs_you))
                        .child(format!("{e}. Click to refresh."))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let _ = ws.update(cx, |w, _| w.refresh_repo(&root));
                        })
                });
                let detached = st.branch.starts_with("(detached");
                let gone = st.upstream.is_some() && st.ab.is_none();
                let pr_strip = pr.as_ref().filter(|_| !detached).and_then(|p| self.pr_strip(&root, p, !busy, gone));
                let scope = agent.as_ref().map(|a| {
                    self.scope_switch(a.only, agent_ix.as_ref().map_or(0, Vec::len), st.files.len() + st.more)
                });
                let count = only.as_ref().map_or(st.files.len() + (st.more > 0) as usize, |o| o.len());
                let header = self.list_header(&root, st.clone(), only.clone(), add, del);
                let bar = self.repo_bar(&root, &st, busy);
                let list = if count == 0 {
                    ui::note(if only.is_some() { "No changes by this agent" } else { "No changes" }).into_any_element()
                } else {
                    let root = root.clone();
                    let rows = uniform_list(
                        "git-files",
                        count,
                        cx.processor(move |p, range: std::ops::Range<usize>, _, _| {
                            let file = |ix: usize| only.as_ref().map_or(ix, |o| o[ix]);
                            range.map(|ix| p.row(file(ix), &root, &st, marks.as_deref(), dot)).collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full();
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(rows)
                        .child(scrollbar(self.scroll.clone(), self.drag.clone(), cx.entity_id()))
                        .into_any_element()
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(bar)
                    .children(self.status_line(&root, ops))
                    .children(error)
                    .children(warning)
                    .children(pr_strip)
                    .child(div().mx(z(space::MD)).mb(z(space::SM)).h(z(1.)).bg(t.border.subtle))
                    .children(scope)
                    .child(header)
                    .child(list)
                    .children(
                        commit.clone().map(|c| div().flex_none().border_t_1().border_color(t.border.subtle).child(c)),
                    )
                    .into_any_element()
            }
        };
        div().flex().flex_col().size_full().bg(t.bg.panel).child(body)
    }
}

/// Stage / Unstage / Discard… on a hunk header row (git's hunks, whatever the engine or style).
#[derive(Clone)]
struct HunkActions {
    hunks: Arc<HashMap<u32, (bool, bool)>>,
    root: PathBuf,
    file: FileChange,
    ws: WeakEntity<Workspace>,
}

impl HunkActions {
    fn buttons(&self, row: u32, span: git::write::Span) -> gpui::Div {
        use crate::git_ops::Job;
        let t = theme::get();
        let (staged, unstaged) = self.hunks.get(&row).copied().unwrap_or_default();
        let button = |id: &'static str, label: &'static str, hint: &'static str, job: Job, confirm: bool| {
            let (ws, root) = (self.ws.clone(), self.root.clone());
            div()
                .id((id, row as usize))
                .flex_none()
                .px(z(space::SM))
                .rounded(z(radius::SM))
                .font_family(theme::UI_FONT)
                .text_size(z(text_size::XS))
                .text_color(t.text.muted)
                .cursor_pointer()
                .hover(|s| s.bg(t.bg.hover).text_color(t.text.primary))
                .child(label)
                .tooltip(tip(hint))
                .on_click(move |_: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let (root, job) = (root.clone(), job.clone());
                    let _ = ws.update(cx, |w, cx| {
                        if confirm { w.confirm_discard(root, job, window, cx) } else { w.run_git(root, job, cx) }
                    });
                })
        };
        let f = &self.file;
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(z(space::XS))
            .when(unstaged, |d| {
                d.child(button("hunk-stage", "Stage", "Stage this hunk", Job::StageHunk(f.clone(), span), false))
            })
            .when(staged, |d| {
                d.child(button(
                    "hunk-unstage",
                    "Unstage",
                    "Unstage this hunk",
                    Job::UnstageHunk(f.clone(), span),
                    false,
                ))
            })
            .when(staged || unstaged, |d| {
                d.child(button(
                    "hunk-discard",
                    "Discard…",
                    "Put this hunk back to the last commit",
                    Job::DiscardHunk(f.clone(), span),
                    true,
                ))
            })
    }
}

/// Below this many terminal columns in the main area, split falls back to unified (each side needs ~50 columns
/// of code plus its gutter).
pub const MIN_SPLIT_COLS: u16 = 120;

/// The diff of one file, over the terminal area. Esc closes it.
pub struct DiffView {
    pub root: PathBuf,
    pub file: FileChange,
    /// The word-diff layout (plain, then colored). None while the first read is running; later reads keep showing
    /// the previous diff until they finish.
    layout: Option<Arc<Layout>>,
    /// The structural diff of the same diff (third publish), or why there is none. Both None while it's computed.
    structural: Option<Arc<Layout>>,
    fallback: Option<Fail>,
    failed: bool,
    /// The saved preference; `narrow` forces unified without changing it.
    mode: DiffMode,
    narrow: bool,
    split_style: SplitStyle,
    engine: DiffEngine,
    /// Hunk rows (line index) → (has staged parts, has unstaged parts): which hunk buttons to show.
    hunks: Arc<HashMap<u32, (bool, bool)>>,
    scroll: UniformListScrollHandle,
    drag: scrollbar::Drag,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    line_height: gpui::Pixels,
    /// One px under the terminal's font size (Settings → Terminal).
    font_size: gpui::Pixels,
}

impl DiffView {
    pub fn new(
        root: PathBuf,
        file: FileChange,
        m: &crate::terminal_element::Metrics,
        (mode, split_style, engine, narrow): (DiffMode, SplitStyle, DiffEngine, bool),
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            split_style,
            engine,
            root,
            file,
            layout: None,
            structural: None,
            fallback: None,
            hunks: Default::default(),
            failed: false,
            mode,
            narrow,
            scroll: UniformListScrollHandle::new(),
            drag: Default::default(),
            focus: cx.focus_handle(),
            workspace,
            line_height: m.line_height,
            font_size: m.font_size - z(1.),
        }
    }

    pub fn set_metrics(&mut self, m: &crate::terminal_element::Metrics, cx: &mut Context<Self>) {
        (self.line_height, self.font_size) = (m.line_height, m.font_size - z(1.));
        cx.notify();
    }

    /// The current layout, handed to the next read so an unchanged diff isn't laid out again.
    pub fn layout(&self) -> Option<Arc<Layout>> {
        self.layout.clone()
    }

    pub fn set_layout(&mut self, layout: Option<Arc<Layout>>, file: FileChange, cx: &mut Context<Self>) {
        match layout {
            Some(l) => {
                if self.layout.as_ref().is_some_and(|old| Arc::ptr_eq(old, &l)) && file == self.file && !self.failed {
                    return;
                }
                // A different diff: its structural result is computed again (colors alone keep it).
                if self.layout.as_ref().is_none_or(|old| old.hash != l.hash) {
                    self.structural = None;
                    self.fallback = None;
                }
                self.layout = Some(l);
                self.failed = false;
            }
            None => self.failed = true,
        }
        self.file = file;
        cx.notify();
    }

    pub fn set_hunks(&mut self, hunks: Arc<HashMap<u32, (bool, bool)>>, cx: &mut Context<Self>) {
        if *hunks != *self.hunks {
            self.hunks = hunks;
            cx.notify();
        }
    }

    /// The structural pass finished: its layout, or why the word diff stays.
    pub fn set_structural(&mut self, result: Result<Arc<Layout>, Fail>, cx: &mut Context<Self>) {
        let top = self.top_line();
        match result {
            Ok(l) => (self.structural, self.fallback) = (Some(l), None),
            Err(e) => (self.structural, self.fallback) = (None, Some(e)),
        }
        self.restore_top(top);
        cx.notify();
    }

    /// The structural pass ran for the current diff (ok or fell back): a refresh of the same diff skips it.
    pub fn structure_settled(&self) -> bool {
        self.structural.is_some() || self.fallback.is_some()
    }

    /// What's drawn: the structural layout when it's preferred and available, else the word one.
    fn shown(&self) -> Option<&Arc<Layout>> {
        match (&self.structural, self.engine) {
            (Some(s), DiffEngine::Auto) => Some(s),
            _ => self.layout.as_ref(),
        }
    }

    /// The unified line at the top of the view (rows differ between split layouts).
    fn top_line(&self) -> Option<usize> {
        let l = self.shown()?;
        let top = self.scroll.0.borrow().base_handle.logical_scroll_top().0;
        Some(if self.split() { split_to_line(l, top) } else { top })
    }

    fn restore_top(&self, line: Option<usize>) {
        if let (Some(line), Some(l), true) = (line, self.shown(), self.split()) {
            self.scroll.scroll_to_item_strict(line_to_split(l, line), gpui::ScrollStrategy::Top);
        }
    }

    fn split(&self) -> bool {
        self.mode == DiffMode::Split && !self.narrow
    }

    /// Mode, split style or width changed: keep the line at the top in view across a mode switch.
    pub fn set_mode(
        &mut self,
        mode: DiffMode,
        split_style: SplitStyle,
        engine: DiffEngine,
        narrow: bool,
        cx: &mut Context<Self>,
    ) {
        if (mode, split_style, engine, narrow) == (self.mode, self.split_style, self.engine, self.narrow) {
            return;
        }
        self.split_style = split_style;
        if engine != self.engine {
            let top = self.top_line();
            self.engine = engine;
            self.restore_top(top);
        }
        let was_split = self.split();
        (self.mode, self.narrow) = (mode, narrow);
        if was_split != self.split()
            && let Some(l) = &self.layout
        {
            let top = self.scroll.0.borrow().base_handle.logical_scroll_top().0;
            let row = if was_split { split_to_line(l, top) } else { line_to_split(l, top) };
            self.scroll.scroll_to_item_strict(row, gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Perf harness: syntax colors arrived.
    pub fn colored(&self) -> bool {
        self.layout.as_ref().is_some_and(|l| l.colored)
    }

    /// Perf harness: how many rows are loaded, and jump to one.
    pub fn loaded_rows(&self) -> usize {
        self.shown().map_or(0, |l| if self.split() { l.split.len() } else { l.lines.len() })
    }

    pub fn scroll_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// File-level Stage / Unstage and Discard…, the view menu (mode, style, engine) and ×.
    fn header_actions(&self) -> impl IntoElement + use<> {
        use crate::git_ops::Job;
        let t = theme::get();
        let f = self.file.clone();
        let (split, _, style, structural, _) = self.view_state();
        let mut view = vec![if split { "Split" } else { "Unified" }];
        if split {
            view.push(if style == SplitStyle::Clean { "Clean" } else { "Classic" });
        }
        view.push(if structural { "Structural" } else { "Word" });
        let small = |id: &'static str| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .gap(z(space::XXS))
                .h(z(22.))
                .px(z(space::SM))
                .rounded(z(radius::SM))
                .text_size(z(text_size::XS))
                .text_color(t.text.muted)
                .cursor_pointer()
                .hover(|s| s.bg(t.bg.hover).text_color(t.text.primary))
        };
        let stage = if f.unstaged {
            Some(("Stage", "Stage the whole file", Job::Stage(vec![f.clone()])))
        } else if f.staged {
            Some(("Unstage", "Unstage the whole file", Job::Unstage(vec![f.clone()])))
        } else {
            None
        };
        let (ws, ws2, ws3, ws4) =
            (self.workspace.clone(), self.workspace.clone(), self.workspace.clone(), self.workspace.clone());
        let (r, r2, r3) = (self.root.clone(), self.root.clone(), self.root.clone());
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(z(space::XS))
            .children(stage.map(|(label, hint, job)| {
                small("diff-stage").border_1().border_color(t.border.subtle).child(label).tooltip(tip(hint)).on_click(
                    move |_: &ClickEvent, _, cx| {
                        let _ = ws.update(cx, |w, cx| w.run_git(r.clone(), job.clone(), cx));
                    },
                )
            }))
            .child(
                small("diff-discard")
                    .border_1()
                    .border_color(t.border.subtle)
                    .child("Discard…")
                    .tooltip(tip("Back to the last commit (asks first)"))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        let job = Job::Discard(f.clone());
                        let _ = ws2.update(cx, |w, cx| w.confirm_discard(r2.clone(), job, window, cx));
                    }),
            )
            .child(
                small("diff-view")
                    .child(view.join(" · "))
                    .child(ui::icon(icon::CHEVRON_DOWN, 11., t.text.dim))
                    .tooltip(tip("Unified / split (Ctrl+Shift+D), split style, structural / word"))
                    .on_click(move |ev: &ClickEvent, _, cx| {
                        let at = ev.position();
                        let _ = ws3.update(cx, |w, cx| {
                            w.open_git_menu(r3.clone(), crate::workspace::GitMenu::DiffView, at, cx)
                        });
                    }),
            )
            .child(
                small("diff-close")
                    .px(z(space::XS))
                    .child(ui::icon(icon::CLOSE, 13., t.text.muted))
                    .tooltip(tip("Close (Esc)"))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        let _ = ws4.update(cx, |w, cx| w.close_overlay(window, cx));
                    }),
            )
    }

    /// What the view menu shows: (split shown, too narrow for split, style, structural shown, why structural is
    /// off).
    pub fn view_state(&self) -> (bool, bool, SplitStyle, bool, Option<&'static str>) {
        let structural = self.shown().is_some_and(|l| l.engine == Engine::Structural);
        let why = match (self.engine, self.fallback, &self.structural) {
            (DiffEngine::Word, ..) => None,
            (_, Some(f), _) => Some(f.reason()),
            (_, None, None) => Some("Computing the syntax-aware diff…"),
            _ => None,
        };
        (self.split(), self.narrow, self.split_style, structural, why)
    }
}

/// A small segmented control (the diff header's switches).
pub(crate) fn segmented() -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_none()
        .p(z(space::XXS))
        .gap(z(space::XXS))
        .rounded(z(radius::MD))
        .border_1()
        .border_color(t.border.subtle)
}

pub(crate) fn segment(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    on: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .px(z(space::SM))
        .rounded(z(radius::SM))
        .text_size(z(text_size::XS))
        .when(on, |d| d.bg(t.bg.selected).text_color(t.text.primary))
        .when(!on && enabled, |d| d.text_color(t.text.muted).cursor_pointer().hover(|s| s.bg(t.bg.hover)))
        .when(!enabled, |d| d.text_color(t.text.faint).cursor_not_allowed())
        .child(label.into())
}

/// First split row showing unified line `ix` (on either side).
fn line_to_split(l: &Layout, ix: usize) -> usize {
    let ix = ix as u32;
    l.split
        .iter()
        .position(|r| match *r {
            SplitRow::Full(i) => i >= ix,
            SplitRow::Pair(a, b) => a.is_some_and(|i| i >= ix) || b.is_some_and(|i| i >= ix),
        })
        .unwrap_or(0)
}

/// The unified line at split row `ix`.
fn split_to_line(l: &Layout, ix: usize) -> usize {
    match l.split.get(ix) {
        Some(SplitRow::Full(i)) => *i as usize,
        Some(SplitRow::Pair(a, b)) => a.or(*b).unwrap_or(0) as usize,
        None => 0,
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Digit columns of the old/new line-number gutters.
#[derive(Clone, Copy)]
struct Geom {
    lh: gpui::Pixels,
    old: usize,
    new: usize,
}

fn digits(n: u32) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn line_colors(kind: LineKind) -> (Option<gpui::Rgba>, gpui::Rgba, Option<gpui::Rgba>, char) {
    let t = theme::get();
    let d = &t.diff;
    match kind {
        LineKind::Add => (Some(d.add_bg), d.add_fg, Some(d.add_word_bg), '+'),
        LineKind::Del => (Some(d.del_bg), d.del_fg, Some(d.del_word_bg), '-'),
        LineKind::Hunk => (None, d.hunk, None, ' '),
        LineKind::Header | LineKind::Meta => (None, t.text.dim, None, ' '),
        LineKind::Context => (None, d.context, None, ' '),
    }
}

/// How a line is drawn. Clean (split only): no line background or marker; a paired changed line shows syntax
/// colors with only its changed tokens in red/green (bold, faint background); an unpaired one is all red/green.
/// Structural layouts add Quiet (a removed/added line with nothing changed on it: reformatting) and Moved (only
/// moved code on it): syntax colors, no line background.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    Classic,
    CleanPaired,
    CleanWhole,
    Quiet,
    Moved,
}

/// Clean style colors of a changed line: (text/number color, token background); None for other kinds.
fn clean_tone(kind: LineKind) -> Option<(gpui::Rgba, gpui::Rgba)> {
    let d = &theme::get().diff;
    match kind {
        LineKind::Del => Some((d.clean_del, d.clean_del_bg)),
        LineKind::Add => Some((d.clean_add, d.clean_add_bg)),
        _ => None,
    }
}

/// One line as a single text run: right-aligned line numbers (`nums`: number, width), marker, text; gutter
/// color, syntax colors and word highlights as highlight ranges. One shaped line per row keeps scrolling cheap.
/// `clean`: the Clean split style (no +/- markers).
fn line_text(l: &DLine, nums: &[(Option<u32>, usize)], look: Look, clean: bool) -> gpui::Div {
    use std::fmt::Write;
    let t = theme::get();
    let (_, mut fg, word_bg, marker) = line_colors(l.kind);
    let tone = clean_tone(l.kind);
    let (marker, gutter_color) = match look {
        Look::Classic => (marker, t.diff.gutter),
        Look::CleanPaired | Look::CleanWhole => (' ', tone.map_or(t.diff.gutter, |(c, _)| c)),
        Look::Quiet => (if clean || tone.is_none() { ' ' } else { '~' }, t.diff.gutter),
        Look::Moved => (if clean { ' ' } else { marker }, t.diff.moved),
    };
    let mut s = String::with_capacity(l.text.len() + 16);
    for &(n, w) in nums {
        match n {
            Some(n) => write!(s, "{n:>w$} ").unwrap(),
            None => write!(s, "{:w$} ", "").unwrap(),
        }
    }
    let gutter = s.len();
    write!(s, " {marker} ").unwrap();
    let start = s.len();
    s.push_str(&l.text);
    let mut hl = vec![(0..gutter, HighlightStyle { color: Some(gutter_color.into()), ..Default::default() })];
    let range = |r: &git::layout::Run| start + r.range.start as usize..start + r.range.end as usize;
    let syntax = |r: &git::layout::Run| r.class.and_then(|c| t.syntax.color(c)).map(Into::into);
    let moved_bg = |r: &git::layout::Run| r.moved.then(|| t.diff.moved_bg.into());
    match (look, tone) {
        // Whole added/removed line: all of it in the tone, nothing else.
        (Look::CleanWhole, Some((c, _))) => fg = c,
        // Code colors; changed tokens in the tone (bold, faint background), moved ones on the moved background.
        (Look::CleanPaired | Look::Quiet | Look::Moved, _) => {
            fg = t.diff.context;
            hl.extend(l.runs.iter().map(|r| {
                let style = match tone {
                    Some((c, bg)) if r.word => HighlightStyle {
                        color: Some(c.into()),
                        background_color: Some(bg.into()),
                        font_weight: Some(gpui::FontWeight::BOLD),
                        ..Default::default()
                    },
                    _ => HighlightStyle { color: syntax(r), background_color: moved_bg(r), ..Default::default() },
                };
                (range(r), style)
            }));
        }
        // Classic (and context lines): syntax sets the text color; the word-diff background sits on top.
        _ => hl.extend(l.runs.iter().map(|r| {
            let style = HighlightStyle {
                color: syntax(r),
                background_color: word_bg.filter(|_| r.word).map(Into::into).or_else(|| moved_bg(r)),
                ..Default::default()
            };
            (range(r), style)
        })),
    }
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(fg)
        .child(StyledText::new(s).with_highlights(hl))
}

/// "moved to L12" (old side) / "moved from L40" (new side) at the end of a moved block's first line.
fn moved_hint(l: &DLine) -> Option<gpui::Div> {
    let n = l.moved?;
    let what = if l.kind == LineKind::Del { "moved to" } else { "moved from" };
    Some(
        div()
            .flex_none()
            .pl(z(space::SM))
            .text_size(z(text_size::XS))
            .text_color(theme::get().diff.moved)
            .child(format!("{what} L{n}")),
    )
}

/// A structural layout's look for one line (per side: alignment by matched nodes can put a changed line next to
/// a reformatted one).
fn structural_look(l: &DLine, clean: bool) -> Look {
    if !matches!(l.kind, LineKind::Del | LineKind::Add) {
        return if clean { Look::CleanWhole } else { Look::Classic };
    }
    if l.same {
        Look::Quiet
    } else if l.words.is_empty() {
        Look::Moved
    } else if !clean {
        Look::Classic
    } else if whole(l) {
        Look::CleanWhole
    } else {
        Look::CleanPaired
    }
}

/// All of the line's non-blank text is changed (a new or deleted line): drawn as a whole line, not token-wise.
fn whole(l: &DLine) -> bool {
    let changed: usize = l
        .words
        .iter()
        .map(|r| l.text[r.start as usize..r.end as usize].chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    changed >= l.text.chars().filter(|c| !c.is_whitespace()).count()
}

fn unified_row(l: &DLine, g: Geom, structural: bool) -> gpui::Div {
    let look = if structural { structural_look(l, false) } else { Look::Classic };
    let (bg, ..) = line_colors(l.kind);
    div()
        .flex()
        .w_full()
        .h(g.lh)
        .pr(z(14.))
        .when_some(bg.filter(|_| look == Look::Classic), |r, bg| r.bg(bg))
        .child(line_text(l, &[(l.old, g.old), (l.new, g.new)], look, false))
        .children(moved_hint(l))
}

fn split_half(l: Option<&DLine>, old_side: bool, g: Geom, look: Look, clean: bool) -> gpui::Div {
    let half = div().flex().flex_1().flex_basis(z(0.)).min_w_0().h_full().overflow_hidden();
    let Some(l) = l else {
        return half.when(!clean, |h| h.bg(theme::get().diff.filler));
    };
    let num = if old_side { (l.old, g.old) } else { (l.new, g.new) };
    if !clean {
        let (bg, ..) = line_colors(l.kind);
        return half
            .when_some(bg.filter(|_| look == Look::Classic), |h, bg| h.bg(bg))
            .child(line_text(l, &[num], look, false))
            .children(moved_hint(l));
    }
    // Clean: a thin bar by the number marks a whole added/removed line, or moved code (same width everywhere, so
    // text aligns).
    let bar = match look {
        Look::CleanWhole => clean_tone(l.kind).map(|(c, _)| c),
        Look::Moved => Some(theme::get().diff.moved),
        _ => None,
    };
    half.child(div().flex_none().w(z(2.)).h_full().when_some(bar, |b, c| b.bg(c)))
        .child(line_text(l, &[num], look, true))
        .children(moved_hint(l))
}

/// Clean style of a split row: a removed/added pair with changed tokens to show is drawn token-wise; anything else
/// (one-sided, too different, nothing token-level changed) as whole lines.
fn clean_look(left: Option<&DLine>, right: Option<&DLine>) -> Look {
    match (left, right) {
        (Some(x), Some(y))
            if x.kind == LineKind::Del && y.kind == LineKind::Add && !(x.words.is_empty() && y.words.is_empty()) =>
        {
            Look::CleanPaired
        }
        _ => Look::CleanWhole,
    }
}

fn split_row(l: &Layout, r: SplitRow, g: Geom, style: SplitStyle) -> gpui::Div {
    match r {
        SplitRow::Full(i) => {
            div().flex().w_full().h(g.lh).child(line_text(&l.lines[i as usize], &[(None, g.old)], Look::Classic, false))
        }
        SplitRow::Pair(a, b) => {
            let get = |i: Option<u32>| i.map(|i| &l.lines[i as usize]);
            let (left, right) = (get(a), get(b));
            let clean = style == SplitStyle::Clean;
            let (ll, rl) = if l.engine == Engine::Structural {
                let look = |x: Option<&DLine>| x.map_or(Look::CleanWhole, |x| structural_look(x, clean));
                (look(left), look(right))
            } else {
                let look = if clean { clean_look(left, right) } else { Look::Classic };
                (look, look)
            };
            div()
                .flex()
                .w_full()
                .h(g.lh)
                .pr(z(14.))
                .child(split_half(left, true, g, ll, clean))
                .child(div().flex_none().w(z(1.)).h_full().bg(theme::get().border.subtle))
                .child(split_half(right, false, g, rl, clean))
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::event("diff view render");
        let t = theme::get();
        let f = &self.file;
        let name = f.path.rsplit_once('/').map_or(f.path.as_str(), |(_, n)| n);
        let header = ui::overlay_header()
            .child(ui::icon(ui::file_icon(name), 14., theme::git_kind(f.kind)))
            .child(div().min_w_0().truncate().text_color(t.text.primary).child(match &f.orig {
                Some(o) => format!("{o} → {}", f.path),
                None => f.path.clone(),
            }))
            .child(div().text_size(z(text_size::XS)).child(line_counts(f.lines)))
            .child(ui::pill(match (f.staged, f.unstaged) {
                (true, true) => "staged + unstaged",
                (true, false) => "staged",
                _ if f.kind == '?' => "untracked",
                _ => "unstaged",
            }))
            .child(div().flex_1())
            .child(self.header_actions());
        let body = match (self.shown(), self.failed) {
            (_, true) => ui::note("Couldn't read the diff").text_color(t.state.error).into_any_element(),
            (None, _) => ui::note("Loading…").into_any_element(),
            (Some(l), _) if l.lines.is_empty() => {
                ui::note("No textual changes (mode change or binary)").into_any_element()
            }
            (Some(l), _) => {
                let (l, split, style) = (l.clone(), self.split(), self.split_style);
                let structural = l.engine == Engine::Structural;
                let actions = HunkActions {
                    hunks: self.hunks.clone(),
                    root: self.root.clone(),
                    file: self.file.clone(),
                    ws: self.workspace.clone(),
                };
                let g = Geom { lh: self.line_height, old: digits(l.max_old), new: digits(l.max_new) };
                let rows = if split { l.split.len() } else { l.lines.len() };
                let count = rows + l.truncated as usize;
                let list = uniform_list("diff-lines", count, move |range, _, _| {
                    range
                        .map(|ix| {
                            if ix >= rows {
                                return div()
                                    .h(g.lh)
                                    .pl(z(space::LG))
                                    .text_color(theme::get().state.needs_you)
                                    .child("… diff truncated (too large to show)");
                            }
                            let (row, line) = if split {
                                let line = match l.split[ix] {
                                    SplitRow::Full(i) => Some(i),
                                    _ => None,
                                };
                                (split_row(&l, l.split[ix], g, style), line)
                            } else {
                                (unified_row(&l.lines[ix], g, structural), Some(ix as u32))
                            };
                            match line.and_then(|i| Some((i, l.lines[i as usize].span?))) {
                                Some((i, span)) => row.child(actions.buttons(i, span)),
                                None => row,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .font_family(MONO_FONT)
                // One glyph per character: a ligature (`>=` → `≥`) would split under a word highlight.
                .font_features(gpui::FontFeatures::disable_ligatures())
                .text_size(self.font_size)
                .line_height(self.line_height);
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
            .on_key_down({
                // Plain closure, not `cx.listener`: closing re-enters the workspace, which reads this view.
                let ws = self.workspace.clone();
                move |ev: &KeyDownEvent, window, cx| {
                    let ks = &ev.keystroke;
                    if ks.key == "escape" && !ks.modifiers.modified() {
                        let _ = ws.update(cx, |w, cx| w.close_overlay(window, cx));
                        cx.stop_propagation();
                    }
                }
            })
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_style_pairs_only_rows_with_token_changes() {
        let line = |kind, words: Vec<std::ops::Range<u32>>| DLine {
            kind,
            old: None,
            new: None,
            text: "x".into(),
            words,
            moves: Vec::new(),
            moved: None,
            same: false,
            span: None,
            runs: Vec::new(),
        };
        let (del, add) = (line(LineKind::Del, vec![]), line(LineKind::Add, std::iter::once(0..1).collect()));
        assert!(clean_look(Some(&del), Some(&add)) == Look::CleanPaired, "one side's change is enough");
        let plain_add = line(LineKind::Add, vec![]);
        assert!(clean_look(Some(&del), Some(&plain_add)) == Look::CleanWhole, "zipped/too different: whole lines");
        assert!(clean_look(None, Some(&add)) == Look::CleanWhole);
        let ctx = line(LineKind::Context, vec![]);
        assert!(clean_look(Some(&ctx), Some(&ctx)) == Look::CleanWhole, "context has no tone anyway");
    }

    fn repo(name: &str) -> git::Repo {
        let d = std::env::temp_dir().join(format!("dirigent-repos-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&d).status().unwrap().success());
        match git::discover(&d) {
            git::Discovery::Repo(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn watchers_are_shared_and_dropped_with_the_last_tab() {
        let (a, b) = (repo("a"), repo("b"));
        let (mut repos, _rx) = Repos::new(crate::changes::Tracker::new().0);
        repos.acquire(a.clone());
        repos.acquire(a.clone()); // second tab in the same repo
        assert_eq!(repos.watched(), vec![(a.root.clone(), 2)]);
        // One tab cd's into repo B: A's watcher stays for the other tab.
        repos.release(&a.root);
        repos.acquire(b.clone());
        assert_eq!(repos.watched(), vec![(a.root.clone(), 1), (b.root.clone(), 1)]);
        // The other one follows: A is unused and dropped.
        repos.release(&a.root);
        repos.acquire(b.clone());
        assert_eq!(repos.watched(), vec![(b.root.clone(), 2)]);
        repos.release(&b.root);
        repos.release(&b.root);
        assert!(repos.watched().is_empty());
        for r in [a, b] {
            let _ = std::fs::remove_dir_all(r.root);
        }
    }
}
