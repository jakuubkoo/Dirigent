//! Projects in the UI: the New session picker (fuzzy over remembered projects, or path mode with directory
//! completion), the group-header dropdowns and the project rename/defaults dialogs. The model is `projects.rs`.

use gpui::{
    AnyElement, ClickEvent, FontWeight, HighlightStyle, MouseMoveEvent, Pixels, Point, SharedString, StyledText,
    UniformListScrollHandle, anchored, deferred, div, uniform_list,
};

use super::*;
use crate::projects;
use crate::theme::{self, MONO_FONT, radius, space, text_size, z};
use crate::ui::{self, icon};

pub(super) struct Picker {
    pub edit: LineEdit,
    /// "Start claude" toggle.
    pub claude: bool,
    pub rows: Vec<PickRow>,
    pub sel: usize,
    pub scroll: UniformListScrollHandle,
    /// Path mode: the listed directory and its subdirectory names (read off-thread, once per directory).
    listing: Option<(PathBuf, Vec<String>)>,
    pub error: Option<String>,
}

pub(super) enum PickRow {
    Project {
        root: PathBuf,
        name: String,
        /// Matched chars of `name`.
        hl: Vec<usize>,
        path: String,
        branch: Option<String>,
        last_used: i64,
        pinned: bool,
        auto: bool,
    },
    Dir {
        path: PathBuf,
        label: String,
    },
}

/// An open dropdown: under a group header button, or a session row's context menu.
pub(super) struct Menu {
    /// The project (group menus) or the session's directory.
    pub root: PathBuf,
    pub kind: MenuKind,
    pub at: Point<Pixels>,
    /// The keyboard selection (index into `menu_items`); the mouse only hovers.
    pub sel: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum MenuKind {
    /// ▾: New session / with claude.
    New,
    /// ⋯: pin, rename, defaults, remove.
    Project,
    /// Right click / Shift+F10 on a session row: the tab id.
    Session(usize),
    /// Right click in a session's terminal: the tab id.
    Terminal(usize),
    /// Right click on a dock's panel tab.
    Panel(crate::layout::Panel),
    /// The Changes panel's ⋯ and Commit ▾ (root = the repo).
    Git(GitMenu),
}

/// (action, label, one-line explanation)
/// (action, label, one-line explanation); the label can be built ("Switch to main and pull").
pub(super) type Item = (&'static str, SharedString, &'static str);

/// Up/Down over `n` items from `sel`, wrapping; nothing selected yet: Down takes the first, Up the last.
fn menu_step(sel: Option<usize>, n: usize, down: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    Some(match (sel, down) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(s), true) => (s + 1) % n,
        (Some(s), false) => (s + n - 1) % n,
    })
}

const PAGE: usize = 10;

impl Workspace {
    /// Ctrl+Shift+T / the title-bar `+`: the picker, listing projects at once (no IO: all in memory).
    pub(super) fn open_new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let picker = Picker {
            edit: LineEdit::new(""),
            claude: false,
            rows: Vec::new(),
            sel: 0,
            scroll: UniformListScrollHandle::new(),
            listing: None,
            error: None,
        };
        self.modal = Some(Modal::Picker(Box::new(picker)));
        self.refilter(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Where relative paths typed in the picker start: the active session's directory.
    fn picker_cwd(&self) -> PathBuf {
        self.tabs.get(self.active).map_or_else(|| self.last_dir.clone(), |t| t.cwd.clone())
    }

    /// Recomputes the rows for the query; path mode lists the typed parent off-thread when it isn't cached.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let cwd = self.picker_cwd();
        let Some(Modal::Picker(p)) = &mut self.modal else { return };
        let q = p.edit.text.clone();
        p.error = None;
        p.sel = 0;
        if projects::is_path_query(&q) {
            let (dir, seg) = projects::split_path_query(&q, &home(), &cwd);
            match &p.listing {
                Some((d, names)) if *d == dir => {
                    p.rows = projects::complete(names, &seg)
                        .into_iter()
                        .map(|n| PickRow::Dir { path: dir.join(n), label: n.to_string() })
                        .collect();
                }
                _ => {
                    p.rows.clear();
                    let d = dir.clone();
                    cx.spawn(async move |this, cx| {
                        let names = cx.background_executor().spawn({
                            let d = d.clone();
                            async move { projects::list_dirs(&d) }
                        });
                        let names = names.await;
                        let _ = this.update(cx, |w, cx| {
                            if let Some(Modal::Picker(p)) = &mut w.modal {
                                p.listing = Some((d, names));
                                w.refilter(cx);
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
            }
        } else {
            let q = q.trim();
            let mut scored: Vec<(i32, PickRow)> = self
                .projects
                .sorted()
                .into_iter()
                .filter_map(|i| {
                    let pr = &self.projects.0[i];
                    let (name, path) = (pr.display(), tilde(&pr.root));
                    let (score, hl) = projects::match_project(q, &name, &path)?;
                    let row = PickRow::Project {
                        root: pr.root.clone(),
                        name,
                        hl,
                        path,
                        branch: pr.branch.clone(),
                        last_used: pr.last_used,
                        pinned: pr.pinned,
                        auto: pr.auto_claude,
                    };
                    Some((score, row))
                })
                .collect();
            if !q.is_empty() {
                scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s)); // stable: pinned / recent order breaks ties
            }
            let Some(Modal::Picker(p)) = &mut self.modal else { return };
            p.rows = scored.into_iter().map(|(_, r)| r).collect();
        }
        if let Some(Modal::Picker(p)) = &self.modal {
            p.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
    }

    pub(super) fn picker_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(Modal::Picker(p)) = &mut self.modal else { return };
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let n = p.rows.len();
        let moved = |p: &mut Picker, to: usize| {
            p.sel = to.min(n.saturating_sub(1));
            p.scroll.scroll_to_item(p.sel, gpui::ScrollStrategy::Center);
        };
        match ks.key.as_str() {
            "escape" => return self.close_modal(window, cx),
            "up" => moved(p, p.sel.saturating_sub(1)),
            "down" => moved(p, p.sel + 1),
            "pageup" => moved(p, p.sel.saturating_sub(PAGE)),
            "pagedown" => moved(p, p.sel + PAGE),
            "enter" => return self.picker_open(None, m.shift, m.control, window, cx),
            "tab" => self.picker_complete(cx),
            "c" if m.control && m.shift => p.claude = !p.claude,
            "v" if m.control => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    p.edit.insert(text.trim());
                    self.refilter(cx);
                }
            }
            key => {
                if p.edit.key(key, ks.key_char.as_deref(), m.control || m.platform) == Edit::Changed {
                    self.refilter(cx);
                }
            }
        }
        cx.notify();
    }

    /// Tab: a project row → its path (into path mode); path mode → the common prefix of the candidates, else the
    /// selected directory + `/`.
    fn picker_complete(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Picker(p)) = &mut self.modal else { return };
        let q = p.edit.text.clone();
        let base = &q[..q.rfind('/').map_or(0, |i| i + 1)];
        let seg = &q[base.len()..];
        let next = match p.rows.get(p.sel) {
            Some(PickRow::Project { root, .. }) => format!("{}/", tilde(root)),
            Some(PickRow::Dir { label, .. }) => {
                let labels: Vec<&str> = p
                    .rows
                    .iter()
                    .filter_map(|r| if let PickRow::Dir { label, .. } = r { Some(label.as_str()) } else { None })
                    .collect();
                let common = projects::common_prefix(&labels);
                if labels.len() > 1 && common.len() > seg.len() {
                    format!("{base}{common}")
                } else {
                    format!("{base}{label}/")
                }
            }
            None => return,
        };
        p.edit = LineEdit::new(next);
        self.refilter(cx);
    }

    /// Enter / a click: a session in the row's project or directory (`ix` None: the selection). With no rows in
    /// path mode the typed path itself, if it is a directory (checked off-thread).
    /// Plain Enter / click → the setup dialog; Ctrl → open at once; Shift → at once with claude.
    pub(super) fn picker_open(
        &mut self,
        ix: Option<usize>,
        shift: bool,
        direct: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let direct = direct || shift;
        let cwd = self.picker_cwd();
        let Some(Modal::Picker(p)) = &mut self.modal else { return };
        let claude = (p.claude || shift).then_some(true);
        let dir = match p.rows.get(ix.unwrap_or(p.sel)) {
            Some(PickRow::Project { root, .. }) => root.clone(),
            Some(PickRow::Dir { path, .. }) => path.clone(),
            None if projects::is_path_query(&p.edit.text) => {
                let (dir, seg) = projects::split_path_query(&p.edit.text, &home(), &cwd);
                let typed = dir.join(seg);
                cx.spawn_in(window, async move |this, cx| {
                    let ok = cx.background_executor().spawn({
                        let t = typed.clone();
                        async move { t.is_dir() }
                    });
                    let ok = ok.await;
                    let _ = this.update_in(cx, |w, window, cx| {
                        if ok && !direct {
                            w.open_setup(typed, window, cx);
                        } else if ok {
                            w.modal = None;
                            w.new_session(typed, claude, window, cx);
                        } else if let Some(Modal::Picker(p)) = &mut w.modal {
                            p.error = Some(format!("Not a directory: {}", typed.display()));
                            cx.notify();
                        }
                    });
                })
                .detach();
                return;
            }
            None => return,
        };
        if !direct {
            return self.open_setup(dir, window, cx);
        }
        self.modal = None;
        self.new_session(dir, claude, window, cx);
        cx.notify();
    }

    pub(super) fn open_menu(&mut self, root: PathBuf, kind: MenuKind, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(Menu { root, kind, at, sel: None });
        cx.notify();
    }

    /// Tab `i`'s context menu at `at` (right click), or under its row with the first item selected (Shift+F10).
    pub(super) fn open_session_menu(
        &mut self,
        i: usize,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.modal, Some(Modal::Rename { .. })) {
            self.commit_rename(window, cx);
        }
        let Some(t) = self.tabs.get(i).filter(|_| self.modal.is_none()) else { return };
        let keyboard = at.is_none();
        // Sessions not shown: below the title bar, by the edge of its dock.
        let at = at.or(self.active_row.get()).unwrap_or_else(|| {
            let x = match self.settings.layout.sessions {
                crate::layout::Dock::Left => z(16.),
                crate::layout::Dock::Right => window.viewport_size().width - z(16. + 260.),
            };
            gpui::point(x, z(theme::height::TITLE_BAR + theme::height::HEADER))
        });
        // Focus stays where it is: the root's capture-phase `modal_key` gets the keys anyway, and Esc returns to the
        // terminal untouched (no cursor repaint).
        self.menu = Some(Menu { root: t.cwd.clone(), kind: MenuKind::Session(t.id), at, sel: keyboard.then_some(0) });
        cx.notify();
    }

    /// Right click in a terminal (Shift+right click while its app takes the mouse): Copy / Paste / Select all.
    pub fn open_terminal_menu(&mut self, view_id: EntityId, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(i) = self.tab_of_view(view_id) else { return };
        self.menu = Some(Menu { root: PathBuf::new(), kind: MenuKind::Terminal(self.tabs[i].id), at, sel: None });
        cx.notify();
    }

    /// The open menu's items (the same list is drawn and walked by the keyboard).
    fn menu_items(&self, menu: &Menu, cx: &gpui::App) -> Vec<Item> {
        match menu.kind {
            MenuKind::New => vec![
                ("setup", "New session…".into(), "Name it, or give it its own git worktree"),
                ("new", "New session".into(), ""),
                ("claude", "New session with claude".into(), ""),
            ],
            MenuKind::Project => {
                let pinned = self.projects.get(&menu.root).is_some_and(|p| p.pinned);
                let mut v = vec![
                    if pinned {
                        ("pin", "Unpin".into(), "The group disappears when its last session closes")
                    } else {
                        ("pin", "Pin to sidebar".into(), "Keep this group in the sidebar even with no sessions")
                    },
                    ("rename", "Rename…".into(), "The name shown in the sidebar and the picker"),
                    ("defaults", "Defaults…".into(), "Start claude, its flags, worktree setup command and location"),
                    ("worktrees", "Worktrees…".into(), "List, open or clean up this repo's git worktrees"),
                ];
                if self.projects.get(&menu.root).is_some() {
                    v.push((
                        "remove",
                        "Remove from recent projects".into(),
                        "Forget it in the picker; files and open sessions stay",
                    ));
                }
                v
            }
            MenuKind::Terminal(id) => {
                let view = self.tab_idx(id).and_then(|i| self.tabs[i].view.as_ref());
                let mut v = Vec::new();
                if view.is_some_and(|v| v.read(cx).has_selection()) {
                    v.push(("copy", "Copy".into(), "Ctrl+Shift+C"));
                }
                v.push(("paste", "Paste".into(), "Ctrl+Shift+V"));
                v.push(("select_all", "Select all".into(), ""));
                v
            }
            MenuKind::Git(kind) => self.git_menu_items(&menu.root, kind, cx),
            MenuKind::Panel(p) => vec![match self.settings.layout.dock_of(p) {
                crate::layout::Dock::Left => ("move", "Move to right dock".into(), ""),
                crate::layout::Dock::Right => ("move", "Move to left dock".into(), ""),
            }],
            MenuKind::Session(id) => {
                let Some(i) = self.tab_idx(id) else { return Vec::new() };
                let t = &self.tabs[i];
                let exited = t.view.as_ref().is_some_and(|v| v.read(cx).session.is_exited());
                let resumable = t.claude_session.is_some() && (exited || self.agent(i).state == agent::State::NoAgent);
                let files = !self.tracker.view(id as u32).touched.is_empty();
                let mut v = vec![
                    ("rename", "Rename".into(), ""),
                    ("close", "Close session".into(), ""),
                    ("restart", "Restart shell".into(), ""),
                    (
                        "duplicate",
                        "Duplicate".into(),
                        "A new session in the same directory, with the project's defaults",
                    ),
                ];
                if resumable {
                    v.push(("resume", "Resume Claude session".into(), ""));
                }
                if self.has_unread(i, cx) {
                    v.push(("read", "Mark as read".into(), ""));
                }
                v.push(("copy", "Copy path".into(), ""));
                v.push(("open", "Open folder in file manager".into(), ""));
                if files {
                    v.push((
                        "forget",
                        "Forget agent's files".into(),
                        "Clears this agent's file list; no file is touched",
                    ));
                }
                v
            }
        }
    }

    /// Keys while a menu is open (capture phase): arrows walk it, Enter runs the selection, Esc closes; nothing else
    /// gets through.
    pub(super) fn menu_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else { return };
        match key {
            "escape" => self.menu = None,
            "up" | "down" => {
                let n = self.menu_items(menu, cx).len();
                let sel = menu_step(menu.sel, n, key == "down");
                if let Some(m) = &mut self.menu {
                    m.sel = sel;
                }
            }
            "enter" => {
                let action = menu.sel.and_then(|s| self.menu_items(menu, cx).get(s).map(|i| i.0));
                if let Some(action) = action {
                    return self.menu_action(action, window, cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn menu_action(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let root = menu.root;
        if let MenuKind::Terminal(id) = menu.kind {
            if let Some(view) = self.tab_idx(id).and_then(|i| self.tabs[i].view.clone()) {
                view.update(cx, |v, cx| match action {
                    "copy" => v.copy(cx),
                    "paste" => v.paste_clipboard(cx),
                    "select_all" => v.select_all(cx),
                    _ => {}
                });
            }
            return cx.notify();
        }
        if let MenuKind::Git(_) = menu.kind {
            self.git_menu_action(root, action, window, cx);
            return cx.notify();
        }
        if let MenuKind::Panel(p) = menu.kind {
            if action == "move" {
                self.move_panel(p, self.settings.layout.dock_of(p).other(), cx);
            }
            return cx.notify();
        }
        if let MenuKind::Session(id) = menu.kind {
            if let Some(i) = self.tab_idx(id) {
                self.session_action(i, action, root, window, cx);
            }
            return cx.notify();
        }
        match action {
            "setup" => self.open_setup(root, window, cx),
            "worktrees" => self.open_worktrees(root, window, cx),
            "new" => {
                self.new_session(root, Some(false), window, cx);
            }
            "claude" => {
                self.new_session(root, Some(true), window, cx);
            }
            "pin" => {
                if self.projects.get(&root).is_none() {
                    self.projects.touch(&root, now_secs());
                }
                if let Some(p) = self.projects.get_mut(&root) {
                    p.pinned = !p.pinned;
                }
                self.mark_dirty(cx);
            }
            "rename" => {
                // Prefilled with what is shown now (fix a typo, or clear it for the folder name).
                let name =
                    self.projects.get(&root).map(|p| p.display()).unwrap_or_else(|| {
                        root.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    });
                self.modal = Some(Modal::ProjectRename { root, edit: LineEdit::new(name) });
                window.focus(&self.focus, cx);
            }
            "defaults" => {
                let p = self.projects.get(&root).cloned().unwrap_or_default();
                let flags = p.claude_args.iter().map(|a| sh_word(a)).collect::<Vec<_>>().join(" ");
                self.modal = Some(Modal::ProjectDefaults(Box::new(Defaults {
                    root,
                    auto: p.auto_claude,
                    flags: LineEdit::new(flags),
                    setup: LineEdit::new(p.setup_cmd.clone().unwrap_or_default()),
                    wt_dir: LineEdit::new(p.worktrees_dir.as_deref().map(tilde).unwrap_or_default()),
                    focus: 1,
                })));
                window.focus(&self.focus, cx);
            }
            "remove" => {
                self.projects.remove(&root);
                self.mark_dirty(cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn session_action(&mut self, i: usize, action: &str, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            "rename" => self.start_rename(i, window, cx),
            "close" => self.close_session(i, false, window, cx),
            "restart" => self.close_session(i, true, window, cx),
            "duplicate" => self.duplicate(i, window, cx),
            "resume" => self.resume_one(i, window, cx),
            "read" => self.mark_read(i, cx),
            "copy" => cx.write_to_clipboard(gpui::ClipboardItem::new_string(dir.display().to_string())),
            "open" => {
                // Waited on its own thread: no zombie, and a slow file manager never holds the UI.
                std::thread::spawn(move || {
                    let status = std::process::Command::new("xdg-open")
                        .arg(&dir)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    if !status.as_ref().is_ok_and(|s| s.success()) {
                        eprintln!("open folder {}: {status:?}", dir.display());
                    }
                });
            }
            "forget" => {
                self.tracker.clear(self.tabs[i].id as u32);
                self.sync_panel(cx);
            }
            _ => {}
        }
    }

    /// The group header's `+`: a session in that project per its default; Shift: the opposite.
    pub(super) fn group_plus(&mut self, root: PathBuf, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let auto = match self.settings.plus_opens {
            crate::settings::PlusOpens::Project => self.projects.get(&root).is_some_and(|p| p.auto_claude),
            crate::settings::PlusOpens::Shell => false,
            crate::settings::PlusOpens::Claude => true,
        };
        self.new_session(root, Some(auto != shift), window, cx);
    }

    /// The dropdown (deferred: drawn above everything) and, below it, a full-window layer that closes it on any
    /// click outside.
    pub(super) fn menu_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let t = theme::get();
        let (items, sel) = (self.menu_items(menu, cx), menu.sel);
        let card = div()
            .occlude()
            .min_w(z(200.))
            .py(z(space::XS))
            .rounded(z(radius::MD))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .text_size(z(text_size::SM))
            .max_w(z(340.))
            .children(items.into_iter().enumerate().map(|(n, (action, label, hint))| {
                div()
                    .id(action)
                    .when(sel == Some(n), |d| d.bg(t.bg.hover))
                    .px(z(space::MD))
                    .py(z(space::XS))
                    .cursor_pointer()
                    .text_color(if action == "remove" { t.state.error } else { t.text.primary })
                    .hover(|s| s.bg(t.bg.hover))
                    .child(label)
                    .when(!hint.is_empty(), |d| {
                        d.child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(hint))
                    })
                    .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| w.menu_action(action, window, cx)))
            }));
        Some(
            div()
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|w, _, _, cx| {
                        w.menu = None;
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    cx.listener(|w, _, _, cx| {
                        w.menu = None;
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    gpui::MouseButton::Middle,
                    cx.listener(|w, _, _, cx| {
                        w.menu = None;
                        cx.notify();
                    }),
                )
                .child(deferred(anchored().position(menu.at).snap_to_window().child(card)).with_priority(1))
                .into_any_element(),
        )
    }

    /// The picker card: search row, project/directory list, hints.
    pub(super) fn picker_view(&self, p: &Picker, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let h = (window.viewport_size().height - z(160.)).min(z(560.)).max(z(240.));
        let path_mode = projects::is_path_query(&p.edit.text);
        let toggle = div()
            .id("picker-claude")
            .flex_none()
            .flex()
            .items_center()
            .gap(z(space::XS))
            .cursor_pointer()
            .text_size(z(text_size::XS))
            .text_color(if p.claude { t.text.primary } else { t.text.muted })
            .child(
                div()
                    .size(z(11.))
                    .rounded(z(3.))
                    .border_1()
                    .border_color(if p.claude { t.accent } else { t.border.strong })
                    .when(p.claude, |d| d.bg(t.accent)),
            )
            .child("Start claude")
            .tooltip(ui::tip("Start claude in the new session (Ctrl+Shift+C; Shift+Enter for once)"))
            .on_click(cx.listener(|w, _: &ClickEvent, _, cx| {
                if let Some(Modal::Picker(p)) = &mut w.modal {
                    p.claude = !p.claude;
                    cx.notify();
                }
            }));
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
            .child(ui::icon(if path_mode { icon::FOLDER } else { icon::PLUS }, 14., t.text.dim))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .when(p.edit.text.is_empty(), |d| {
                        d.child(div().w(z(1.5)).h(z(16.)).bg(t.accent))
                            .child(div().text_color(t.text.faint).child("Search projects, or type a path (/ ~ .)"))
                    })
                    .when(!p.edit.text.is_empty(), |d| {
                        d.child(p.edit.before_cursor().to_string())
                            .child(div().w(z(1.5)).h(z(16.)).bg(t.accent))
                            .child(p.edit.after_cursor().to_string())
                    }),
            )
            .child(toggle);
        let n = p.rows.len();
        let list = uniform_list(
            "picker-rows",
            n,
            cx.processor(|w: &mut Workspace, range: std::ops::Range<usize>, _window, cx| {
                let Some(Modal::Picker(p)) = &w.modal else { return Vec::new() };
                range.map(|i| picker_row(p, i, now_secs(), cx)).collect()
            }),
        )
        .track_scroll(&p.scroll)
        .flex_1();
        let empty = if n == 0 {
            Some(div().p(z(space::LG)).text_size(z(text_size::SM)).text_color(t.text.dim).child(if path_mode {
                if p.edit.text.ends_with('/') {
                    "No subdirectories. Enter opens this directory."
                } else {
                    "No match. Enter opens the typed path if it is a directory."
                }
            } else if self.projects.0.is_empty() {
                "No projects yet: type a path (/ ~ .) to open one."
            } else {
                "No project matches."
            }))
        } else {
            None
        };
        div()
            .w(z(720.))
            .h(h)
            .p(z(space::LG))
            .flex()
            .flex_col()
            .gap(z(space::MD))
            .rounded(z(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(search)
            .child(div().flex_1().min_h_0().flex().flex_col().child(list).children(empty))
            .children(p.error.clone().map(|e| div().text_size(z(text_size::SM)).text_color(t.state.error).child(e)))
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(
                "↑↓ select · Enter set up… · Ctrl+Enter open now · Shift+Enter now with claude · Tab complete · Esc",
            ))
            .into_any_element()
    }
}

fn picker_row(p: &Picker, i: usize, now: i64, cx: &mut Context<Workspace>) -> AnyElement {
    let t = theme::get();
    let selected = i == p.sel;
    let row = div()
        .id(("pick", i))
        .w_full()
        .flex()
        .items_center()
        .gap(z(space::MD))
        .h(z(40.))
        .px(z(space::MD))
        .rounded(z(radius::MD))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.bg.selected_focus))
        .on_mouse_move(cx.listener(move |w, _: &MouseMoveEvent, _, cx| {
            if let Some(Modal::Picker(p)) = &mut w.modal
                && p.sel != i
            {
                p.sel = i;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |w, ev: &ClickEvent, window, cx| {
            w.picker_open(Some(i), ev.modifiers().shift, ev.modifiers().control, window, cx)
        }));
    match &p.rows[i] {
        PickRow::Project { name, hl, path, branch, last_used, pinned, auto, .. } => {
            // Matched chars bold + accent.
            let hl: Vec<_> = hl
                .iter()
                .filter_map(|&c| name.char_indices().nth(c).map(|(b, ch)| b..b + ch.len_utf8()))
                .map(|r| {
                    (
                        r,
                        HighlightStyle {
                            color: Some(t.accent.into()),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            row.child(ui::icon(
                if *pinned { icon::PIN } else { icon::FOLDER },
                14.,
                if *pinned { t.accent } else { t.text.dim },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(z(text_size::MD))
                            .text_color(t.text.primary)
                            .child(StyledText::new(SharedString::from(name.clone())).with_highlights(hl)),
                    )
                    .child(div().truncate().text_size(z(text_size::XS)).text_color(t.text.dim).child(path.clone())),
            )
            .when(*auto, |d| d.child(div().flex_none().child(ui::icon(icon::GENERATE, 12., t.text.dim))))
            .children(branch.clone().map(|b| {
                div()
                    .flex_none()
                    .max_w(z(180.))
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .text_size(z(text_size::XS))
                    .text_color(t.text.muted)
                    .child(ui::icon(icon::BRANCH, 11., t.text.dim))
                    .child(div().truncate().font_family(MONO_FONT).child(b))
            }))
            .child(
                div()
                    .flex_none()
                    .w(z(36.))
                    .text_size(z(text_size::XS))
                    .text_color(t.text.dim)
                    .child(if *last_used > 0 { ui::fmt_ago(now - last_used) } else { String::new() }),
            )
            .into_any_element()
        }
        PickRow::Dir { label, path } => row
            .child(ui::icon(icon::FOLDER, 14., t.text.dim))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(z(text_size::MD))
                    .text_color(t.text.primary)
                    .child(format!("{label}/")),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(z(300.))
                    .truncate()
                    .text_size(z(text_size::XS))
                    .text_color(t.text.dim)
                    .child(tilde(path)),
            )
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::menu_step;

    #[test]
    fn menu_keys_wrap() {
        assert_eq!(menu_step(None, 3, true), Some(0));
        assert_eq!(menu_step(None, 3, false), Some(2));
        assert_eq!(menu_step(Some(2), 3, true), Some(0));
        assert_eq!(menu_step(Some(0), 3, false), Some(2));
        assert_eq!(menu_step(Some(1), 3, true), Some(2));
        assert_eq!(menu_step(None, 0, true), None);
    }
}
