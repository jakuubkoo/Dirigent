//! Projects in the UI: the New session picker (fuzzy over remembered projects, or path mode with directory
//! completion), the group-header dropdowns and the project rename/defaults dialogs. The model is `projects.rs`.

use gpui::{
    AnyElement, ClickEvent, FontWeight, HighlightStyle, MouseMoveEvent, Pixels, Point, SharedString, StyledText,
    UniformListScrollHandle, anchored, deferred, div, px, uniform_list,
};

use super::*;
use crate::projects;
use crate::theme::{self, MONO_FONT, radius, space, text_size};
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

/// An open dropdown under a group header button.
pub(super) struct Menu {
    pub root: PathBuf,
    pub kind: MenuKind,
    pub at: Point<Pixels>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum MenuKind {
    /// ▾: New session / with claude.
    New,
    /// ⋯: pin, rename, defaults, remove.
    Project,
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
                let labels: Vec<&str> = p.rows.iter().filter_map(|r| if let PickRow::Dir { label, .. } = r { Some(label.as_str()) } else { None }).collect();
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
    pub(super) fn picker_open(&mut self, ix: Option<usize>, shift: bool, direct: bool, window: &mut Window, cx: &mut Context<Self>) {
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
        self.menu = Some(Menu { root, kind, at });
        cx.notify();
    }

    fn menu_action(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let root = menu.root;
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
                let name = self.projects.get(&root).map(|p| p.display()).unwrap_or_else(|| {
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

    /// The group header's `+`: a session in that project per its default; Shift: the opposite.
    pub(super) fn group_plus(&mut self, root: PathBuf, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let auto = self.projects.get(&root).is_some_and(|p| p.auto_claude);
        self.new_session(root, Some(auto != shift), window, cx);
    }

    /// The dropdown (deferred: drawn above everything) and, below it, a full-window layer that closes it on any
    /// click outside.
    pub(super) fn menu_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let t = theme::get();
        // (action, label, one-line explanation)
        let items: Vec<(&'static str, &'static str, &'static str)> = match menu.kind {
            MenuKind::New => vec![
                ("setup", "New session…", "Name it, or give it its own git worktree"),
                ("new", "New session", ""),
                ("claude", "New session with claude", ""),
            ],
            MenuKind::Project => {
                let pinned = self.projects.get(&menu.root).is_some_and(|p| p.pinned);
                let mut v = vec![
                    if pinned {
                        ("pin", "Unpin", "The group disappears when its last session closes")
                    } else {
                        ("pin", "Pin to sidebar", "Keep this group in the sidebar even with no sessions")
                    },
                    ("rename", "Rename…", "The name shown in the sidebar and the picker"),
                    ("defaults", "Defaults…", "Start claude, its flags, worktree setup command and location"),
                    ("worktrees", "Worktrees…", "List, open or clean up this repo's git worktrees"),
                ];
                if self.projects.get(&menu.root).is_some() {
                    v.push(("remove", "Remove from recent projects", "Forget it in the picker; files and open sessions stay"));
                }
                v
            }
        };
        let card = div()
            .occlude()
            .min_w(px(200.))
            .py(px(space::XS))
            .rounded(px(radius::MD))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .text_size(px(text_size::SM))
            .max_w(px(340.))
            .children(items.into_iter().map(|(action, label, hint)| {
                div()
                    .id(action)
                    .px(px(space::MD))
                    .py(px(space::XS))
                    .cursor_pointer()
                    .text_color(if action == "remove" { t.state.error } else { t.text.primary })
                    .hover(|s| s.bg(t.bg.hover))
                    .child(label)
                    .when(!hint.is_empty(), |d| d.child(div().text_size(px(text_size::XS)).text_color(t.text.dim).child(hint)))
                    .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| w.menu_action(action, window, cx)))
            }));
        Some(
            div()
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|w, _, _, cx| {
                    w.menu = None;
                    cx.notify();
                }))
                .child(deferred(anchored().position(menu.at).snap_to_window().child(card)).with_priority(1))
                .into_any_element(),
        )
    }

    /// The picker card: search row, project/directory list, hints.
    pub(super) fn picker_view(&self, p: &Picker, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::get();
        let h = (window.viewport_size().height - px(160.)).min(px(560.)).max(px(240.));
        let path_mode = projects::is_path_query(&p.edit.text);
        let toggle = div()
            .id("picker-claude")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .cursor_pointer()
            .text_size(px(text_size::XS))
            .text_color(if p.claude { t.text.primary } else { t.text.muted })
            .child(div().size(px(11.)).rounded(px(3.)).border_1().border_color(if p.claude { t.accent } else { t.border.strong }).when(p.claude, |d| d.bg(t.accent)))
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
            .gap(px(space::SM))
            .px(px(space::MD))
            .py(px(space::SM))
            .rounded(px(radius::MD))
            .bg(t.bg.input)
            .border_1()
            .border_color(t.accent)
            .text_size(px(text_size::MD))
            .child(ui::icon(if path_mode { icon::FOLDER } else { icon::PLUS }, 14., t.text.dim))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .when(p.edit.text.is_empty(), |d| d.child(div().w(px(1.5)).h(px(16.)).bg(t.accent)).child(div().text_color(t.text.faint).child("Search projects, or type a path (/ ~ .)")))
                    .when(!p.edit.text.is_empty(), |d| {
                        d.child(p.edit.before_cursor().to_string()).child(div().w(px(1.5)).h(px(16.)).bg(t.accent)).child(p.edit.after_cursor().to_string())
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
            Some(div().p(px(space::LG)).text_size(px(text_size::SM)).text_color(t.text.dim).child(if path_mode {
                if p.edit.text.ends_with('/') { "No subdirectories. Enter opens this directory." } else { "No match. Enter opens the typed path if it is a directory." }
            } else if self.projects.0.is_empty() {
                "No projects yet: type a path (/ ~ .) to open one."
            } else {
                "No project matches."
            }))
        } else {
            None
        };
        div()
            .w(px(720.))
            .h(h)
            .p(px(space::LG))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .rounded(px(radius::LG))
            .bg(t.bg.elevated)
            .border_1()
            .border_color(t.border.strong)
            .child(search)
            .child(div().flex_1().min_h_0().flex().flex_col().child(list).children(empty))
            .children(p.error.clone().map(|e| div().text_size(px(text_size::SM)).text_color(t.state.error).child(e)))
            .child(
                div()
                    .text_size(px(text_size::XS))
                    .text_color(t.text.dim)
                    .child("↑↓ select · Enter set up… · Ctrl+Enter open now · Shift+Enter now with claude · Tab complete · Esc"),
            )
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
        .gap(px(space::MD))
        .h(px(40.))
        .px(px(space::MD))
        .rounded(px(radius::MD))
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
        .on_click(cx.listener(move |w, ev: &ClickEvent, window, cx| w.picker_open(Some(i), ev.modifiers().shift, ev.modifiers().control, window, cx)));
    match &p.rows[i] {
        PickRow::Project { name, hl, path, branch, last_used, pinned, auto, .. } => {
            // Matched chars bold + accent.
            let hl: Vec<_> = hl
                .iter()
                .filter_map(|&c| name.char_indices().nth(c).map(|(b, ch)| b..b + ch.len_utf8()))
                .map(|r| (r, HighlightStyle { color: Some(t.accent.into()), font_weight: Some(FontWeight::BOLD), ..Default::default() }))
                .collect();
            row.child(ui::icon(if *pinned { icon::PIN } else { icon::FOLDER }, 14., if *pinned { t.accent } else { t.text.dim }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_size(px(text_size::MD)).text_color(t.text.primary).child(StyledText::new(SharedString::from(name.clone())).with_highlights(hl)))
                        .child(div().truncate().text_size(px(text_size::XS)).text_color(t.text.dim).child(path.clone())),
                )
                .when(*auto, |d| d.child(div().flex_none().child(ui::icon(icon::GENERATE, 12., t.text.dim))))
                .children(branch.clone().map(|b| {
                    div()
                        .flex_none()
                        .max_w(px(180.))
                        .flex()
                        .items_center()
                        .gap(px(space::XS))
                        .text_size(px(text_size::XS))
                        .text_color(t.text.muted)
                        .child(ui::icon(icon::BRANCH, 11., t.text.dim))
                        .child(div().truncate().font_family(MONO_FONT).child(b))
                }))
                .child(div().flex_none().w(px(36.)).text_size(px(text_size::XS)).text_color(t.text.dim).child(if *last_used > 0 { ui::fmt_ago(now - last_used) } else { String::new() }))
                .into_any_element()
        }
        PickRow::Dir { label, path } => row
            .child(ui::icon(icon::FOLDER, 14., t.text.dim))
            .child(div().flex_1().min_w_0().truncate().text_size(px(text_size::MD)).text_color(t.text.primary).child(format!("{label}/")))
            .child(div().flex_none().max_w(px(300.)).truncate().text_size(px(text_size::XS)).text_color(t.text.dim).child(tilde(path)))
            .into_any_element(),
    }
}
