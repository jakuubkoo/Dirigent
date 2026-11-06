//! Rendering of the workspace: title bar, grouped session sidebar, slim header, main area, right panel, status
//! bar and modals. All styling comes from `theme` tokens. State changes live in the parent module.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    ClickEvent, Context, DragMoveEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent, Render, SharedString,
    StyleRefinement, Transformation, Window, canvas, div, percentage,
};

use super::*;
use crate::theme::{self, MONO_FONT, UI_FONT, height, radius, space, text_size, z};
use crate::ui::{self, icon, line_counts, tip};

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::root_render();
        self.active_row.set(None); // set again while the active row paints
        self.flush_pending_open(window, cx);
        self.spinner_drawn.set(false);
        self.refresh_metrics(window, cx);
        self.schedule_resize(window, cx);
        self.arm_minute_timer(cx);
        let t = theme::get();
        let chrome = Chrome::of(window);

        let main = match (&self.overlay, self.tabs.get(self.active)) {
            (Some(Overlay::Diff(v)), _) => v.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (Some(Overlay::File(v)), _) => v.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (Some(Overlay::Settings(v)), _) => {
                v.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            }
            (None, Some(Tab { view: Some(view), .. })) => {
                view.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            }
            _ => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(z(space::MD))
                .child(ui::icon(icon::TERMINAL, 28., t.text.faint))
                .child(div().text_color(t.text.dim).child("No sessions"))
                .child(div().text_size(z(text_size::SM)).text_color(t.text.faint).child("Ctrl+Shift+T to start one"))
                .into_any_element(),
        };

        // Settings belongs to no session: the title says so instead of the active one.
        let mut title = match self.tabs.get(self.active) {
            _ if self.settings_open() => "Settings — Dirigent".to_string(),
            Some(tab) => format!("{} — Dirigent", tab.name),
            None => "Dirigent".to_string(),
        };
        match self.needs_you_count() {
            0 => {}
            1 => title.push_str(" · 1 needs you"),
            n => title.push_str(&format!(" · {n} need you")),
        }
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }

        let left = self.dock(Dock::Left, cx);
        let right = self.dock(Dock::Right, cx);

        let title_bar = self.title_bar(window, chrome.csd, cx);
        self.spin_tick(cx);

        div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .font_family(UI_FONT)
            .text_size(z(text_size::MD))
            .text_color(t.text.primary)
            // With blur the window is transparent where nothing paints: every region paints its own background.
            .when(!self.translucent, |d| d.bg(t.bg.base))
            .when(chrome.border > z(0.), |d| d.border(chrome.border).border_color(t.border.strong))
            // Capture phase: an open modal gets every key first, wherever focus ended up (a click can focus the diff).
            .capture_key_down(cx.listener(Self::modal_key))
            // A click outside the inline session rename field saves it (like Enter).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|w, _, window, cx| {
                    if matches!(w.modal, Some(Modal::Rename { .. })) {
                        w.commit_rename(window, cx);
                    }
                }),
            )
            .on_action(cx.listener(|w, _: &NewSession, window, cx| w.open_new_session(window, cx)))
            .on_action(cx.listener(|w, _: &NewInProject, window, cx| w.new_in_project(window, cx)))
            .on_action(cx.listener(|w, _: &CloseSession, window, cx| w.close_active(window, cx)))
            .on_action(cx.listener(|w, _: &NextSession, window, cx| w.cycle(1, window, cx)))
            .on_action(cx.listener(|w, _: &PrevSession, window, cx| w.cycle(-1, window, cx)))
            .on_action(cx.listener(|w, _: &Rename, window, cx| w.start_rename(w.active, window, cx)))
            .on_action(cx.listener(|w, _: &ToggleSidebar, _, cx| w.toggle_sidebar(cx)))
            .on_action(cx.listener(|w, _: &NextNeedsYou, window, cx| w.next_needs_you(window, cx)))
            .on_action(cx.listener(|w, _: &ResumeAll, window, cx| w.resume_all(window, cx)))
            .on_action(cx.listener(|w, _: &ShowChanges, window, cx| w.show_panel_tab(Panel::Changes, window, cx)))
            .on_action(cx.listener(|w, _: &ShowFiles, window, cx| w.show_panel_tab(Panel::Files, window, cx)))
            .on_action(cx.listener(|w, _: &ToggleDiffMode, _, cx| w.toggle_diff_mode(cx)))
            .on_action(cx.listener(|w, _: &OpenSettings, window, cx| w.open_settings(window, cx)))
            .on_action(cx.listener(|w, _: &ZoomIn, _, cx| w.zoom(Some(1), cx)))
            .on_action(cx.listener(|w, _: &ZoomOut, _, cx| w.zoom(Some(-1), cx)))
            .on_action(cx.listener(|w, _: &ZoomReset, _, cx| w.zoom(None, cx)))
            .on_action(cx.listener(|w, _: &SessionMenu, window, cx| w.open_session_menu(w.active, None, window, cx)))
            .on_action(cx.listener(|w, _: &QuickOpen, window, cx| w.open_cc("", window, cx)))
            .on_action(cx.listener(|w, _: &CommandPalette, window, cx| w.open_cc(">", window, cx)))
            .on_action(cx.listener(|w, _: &FindInFiles, window, cx| w.open_cc("#", window, cx)))
            .on_action(cx.listener(|w, _: &GitSync, _, cx| w.git_active(crate::git_ops::Job::Sync, cx)))
            .on_action(cx.listener(|w, _: &GitFetch, _, cx| w.git_active(crate::git_ops::Job::Fetch, cx)))
            .on_action(cx.listener(|w, _: &GitPull, _, cx| w.git_active(crate::git_ops::Job::Pull, cx)))
            .on_action(cx.listener(|w, _: &GitPush, _, cx| w.git_active(crate::git_ops::Job::Push, cx)))
            .on_action(cx.listener(|w, _: &SwitchBranch, window, cx| w.open_branches(window, cx)))
            .on_action(cx.listener(|w, _: &Activate1, window, cx| w.activate_nth(0, window, cx)))
            .on_action(cx.listener(|w, _: &Activate2, window, cx| w.activate_nth(1, window, cx)))
            .on_action(cx.listener(|w, _: &Activate3, window, cx| w.activate_nth(2, window, cx)))
            .on_action(cx.listener(|w, _: &Activate4, window, cx| w.activate_nth(3, window, cx)))
            .on_action(cx.listener(|w, _: &Activate5, window, cx| w.activate_nth(4, window, cx)))
            .on_action(cx.listener(|w, _: &Activate6, window, cx| w.activate_nth(5, window, cx)))
            .on_action(cx.listener(|w, _: &Activate7, window, cx| w.activate_nth(6, window, cx)))
            .on_action(cx.listener(|w, _: &Activate8, window, cx| w.activate_nth(7, window, cx)))
            .on_action(cx.listener(|w, _: &Activate9, window, cx| w.activate_nth(8, window, cx)))
            .on_drag_move::<DockDrag>(cx.listener(|w, e: &DragMoveEvent<DockDrag>, window, cx| {
                let d = e.drag(cx).0;
                w.dragging = true;
                // Stored at 100 %: the pointer's distance from the window edge ÷ zoom.
                let zoom = theme::zoom();
                let min = w.settings.layout.min_width(d);
                let max = (f32::from(window.viewport_size().width) * 0.6 / zoom).max(min);
                let dist = match d {
                    Dock::Left => e.event.position.x - e.bounds.left(),
                    Dock::Right => e.bounds.right() - e.event.position.x,
                };
                w.docks[d.ix()].width = (f32::from(dist) / zoom).clamp(min, max);
                if w.settings.layout.changes == d {
                    w.sync_panel(cx);
                }
                cx.notify();
            }))
            .on_drop::<DockDrag>(cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|w, _, _, cx| w.end_drag(cx)))
            .child(title_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .children(left)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .bg(t.bg.base)
                            // Diff and preview belong to the session; Settings fills the main area from the top.
                            .when(!self.settings_open(), |d| d.child(self.session_header()))
                            .child(div().flex_1().min_h_0().overflow_hidden().child(main)),
                    )
                    .children(right),
            )
            .child(self.status_bar(cx))
            .children(self.modal_view(window, cx))
            .children(self.menu_view(cx))
            .children(self.cc_view(window, cx))
            .when(chrome.csd, |d| d.children(window_chrome::resize_handles(chrome.tiling)))
    }
}

/// The brand's width (mark, name, +, gear).
const BRAND_W: f32 = 150.;

/// Width of the drop strip a collapsed or empty dock shows while a panel tab is dragged.
const DROP_STRIP: f32 = 28.;

/// A draggable 4 px divider with a 1 px hairline.
fn divider<D: Clone + 'static>(id: &'static str, drag: D) -> impl IntoElement {
    let t = theme::get();
    div()
        .id(id)
        .flex_none()
        .w(z(DIVIDER))
        .h_full()
        .flex()
        .justify_center()
        .cursor_col_resize()
        .bg(t.bg.panel)
        .hover(|s| s.bg(t.bg.hover))
        .child(div().w(z(1.)).h_full().bg(t.border.subtle))
        .on_drag(drag, |_, _, _, cx| cx.new(|_| DragGhost))
}

/// A square icon button in the title bar: never starts a window move.
fn icon_button(id: impl Into<gpui::ElementId>, glyph: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    square_button(id, glyph, active).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A square icon button (dock tabs: draggable, so the press must reach the drag).
fn square_button(id: impl Into<gpui::ElementId>, glyph: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex_none()
        .size(z(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(z(radius::MD))
        .cursor_pointer()
        .when(active, |d| d.bg(t.bg.selected))
        .hover(|s| s.bg(t.bg.hover))
        .child(ui::icon(glyph, 15., if active { t.accent } else { t.text.muted }))
}

/// `~/Code/project/crates/app/src` → `~/Code/…/app/src` (keeps the first two and the last two components).
pub(super) fn short_path(p: &Path) -> String {
    let full = tilde(p);
    let parts: Vec<&str> = full.split('/').collect();
    if parts.len() <= 5 {
        return full;
    }
    format!("{}/{}/…/{}/{}", parts[0], parts[1], parts[parts.len() - 2], parts[parts.len() - 1])
}

/// "<1m", "4m", "2h", "3d".
static SPIN_EPOCH: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
const SPIN_FRAME: Duration = Duration::from_millis(100);

fn fmt_for(secs: i64) -> String {
    if secs < 60 { "<1m".into() } else { ui::fmt_ago(secs) }
}

impl Workspace {
    /// One 10 fps re-render while a spinner is on screen and the window is visible; stops by itself otherwise.
    fn spin_tick(&mut self, cx: &mut Context<Self>) {
        if !(self.spinner_drawn.get() && self.window_visible) {
            self._spin = None;
            return;
        }
        if self._spin.is_none() {
            self._spin = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(SPIN_FRAME).await;
                    let alive = this.update(cx, |w, cx| {
                        let on = w.spinner_drawn.get() && w.window_visible;
                        if on {
                            cx.notify();
                        }
                        on
                    });
                    if !matches!(alive, Ok(true)) {
                        break;
                    }
                }
            }));
        }
    }

    fn panel_bg(&self) -> gpui::Rgba {
        let t = theme::get();
        if self.translucent { t.bg.panel_translucent } else { t.bg.panel }
    }

    /// App mark, name, new session and settings: always at the left of the title bar, wherever the panels live.
    fn brand(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(z(space::SM))
            .w(z(BRAND_W))
            .h_full()
            .px(z(space::MD))
            .child(ui::icon(icon::TERMINAL, 15., t.accent))
            .child(div().flex_1().text_size(z(text_size::SM)).font_weight(FontWeight::MEDIUM).child("Dirigent"))
            .child(
                icon_button("new", icon::PLUS, false)
                    .tooltip(tip("New session  Ctrl+Shift+T"))
                    .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_new_session(window, cx))),
            )
            .child(
                icon_button("settings", icon::SETTINGS, self.settings_open())
                    .tooltip(tip("Settings  Ctrl+,"))
                    .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_settings(window, cx))),
            )
    }

    /// The top bar: the brand at the left, the Command Center box centred in the window. Client-side decorations
    /// add the window controls and the move / maximize / window-menu handling; server-side ones get the same bar
    /// without them (the window manager draws its own title above).
    fn title_bar(&self, window: &Window, csd: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        // The box is centred in the whole bar, kept clear of both sides (its own layer has no hitbox, so the bar
        // still drags everywhere else).
        let controls_w = if csd { 3. * 40. } else { 0. };
        let reserve = BRAND_W.max(controls_w) + space::SM;
        div()
            .id("title-bar")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(z(height::TITLE_BAR))
            .bg(t.bg.panel)
            .border_b_1()
            .border_color(t.border.subtle)
            .child(self.brand(cx))
            .child(div().flex_1())
            .when(csd, |d| d.child(window_chrome::window_controls(window)))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(z(reserve))
                    .child(self.cc_box(cx)),
            )
            .when(csd, |d| {
                d.on_mouse_down(
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
            })
    }

    /// The Sessions panel: project groups and their session rows.
    fn sessions_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        let now = now_secs();
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        for (key, members) in self.groups() {
            let collapsed = self.collapsed.contains(&group_id(&key));
            let needs = members.iter().filter(|&&i| self.agent(i).state == agent::State::NeedsYou).count();
            let project = key.as_ref().and_then(|r| self.projects.get(r));
            let pinned = project.is_some_and(|p| p.pinned);
            let name: SharedString = match &key {
                Some(root) => project
                    .map(|p| p.display())
                    .unwrap_or_else(|| {
                        root.file_name().map_or(root.display().to_string(), |n| n.to_string_lossy().into_owned())
                    })
                    .into(),
                None => "Other".into(),
            };
            let id = group_id(&key);
            let gname = SharedString::from(format!("gh-{}", id.display()));
            let menu_here = self
                .menu
                .as_ref()
                .is_some_and(|m| !matches!(m.kind, projects_ui::MenuKind::Session(_)) && Some(&m.root) == key.as_ref());
            // Hover buttons: + (a session per the project default; Shift: the opposite), ▾ (with/without claude),
            // ⋯ (pin, rename, defaults, remove). Never toggle the group.
            let buttons = key.clone().map(|root| {
                let b = |id: &'static str, glyph: &'static str, hint: &'static str| {
                    div()
                        .id(id)
                        .flex_none()
                        .size(z(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(z(radius::SM))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.bg.selected))
                        .child(ui::icon(glyph, 13., t.text.muted))
                        .tooltip(tip(hint))
                };
                let (r1, r2, r3) = (root.clone(), root.clone(), root);
                div()
                    .ml_auto()
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(!menu_here, |d| d.opacity(0.).group_hover(gname.clone(), |s| s.opacity(1.)))
                    .child(b("g-plus", icon::PLUS, "New session here (Shift: the other claude choice)").on_click(
                        cx.listener(move |w, ev: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            w.group_plus(r1.clone(), ev.modifiers().shift, window, cx);
                        }),
                    ))
                    .child(b("g-new", icon::CHEVRON_DOWN, "New session…").on_click(cx.listener(
                        move |w, ev: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            w.open_menu(r2.clone(), projects_ui::MenuKind::New, ev.position(), cx);
                        },
                    )))
                    .child(b("g-more", icon::MORE, "Project: pin, rename, defaults").on_click(cx.listener(
                        move |w, ev: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            w.open_menu(r3.clone(), projects_ui::MenuKind::Project, ev.position(), cx);
                        },
                    )))
            });
            items.push(
                div()
                    .id(SharedString::from(format!("group-{}", id.display())))
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .h(z(height::GROUP_HEADER))
                    .mt(z(space::SM))
                    .px(z(space::MD))
                    .mx(z(space::SM))
                    .rounded(z(radius::MD))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg.hover))
                    .text_size(z(text_size::SM))
                    .child(ui::icon(if collapsed { icon::CHEVRON_RIGHT } else { icon::CHEVRON_DOWN }, 12., t.text.dim))
                    .group(gname.clone())
                    .child(
                        div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(t.text.muted).child(name),
                    )
                    .when(pinned, |d| d.child(div().flex_none().child(ui::icon(icon::PIN, 10., t.text.dim))))
                    .child(
                        div()
                            .flex_none()
                            .text_size(z(text_size::XS))
                            .text_color(t.text.dim)
                            .child(members.len().to_string()),
                    )
                    .when(needs > 0, |d| {
                        d.child(
                            div()
                                .when(buttons.is_none(), |d| d.ml_auto())
                                .flex_none()
                                .text_size(z(text_size::XS))
                                .text_color(t.state.needs_you)
                                .child(format!("{needs} need{} you", if needs == 1 { "s" } else { "" })),
                        )
                    })
                    .children(buttons)
                    .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| w.toggle_group(id.clone(), cx)))
                    .into_any_element(),
            );
            if !collapsed {
                for i in members {
                    items.push(self.session_row(i, now, cx));
                }
            }
        }
        div().id("sessions").size_full().overflow_y_scroll().pb(z(space::MD)).children(items)
    }

    /// One dock: its tabs and the panel it shows, the divider on the terminal's side. Collapsed: a 10 px strip that
    /// reopens it; empty: nothing, except a drop strip while a tab is dragged.
    fn dock(&mut self, d: Dock, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let lay = self.settings.layout;
        let shown = self.dock_shown(d);
        let target = move |el: gpui::Stateful<gpui::Div>, cx: &mut Context<Self>| {
            el.drag_over::<PanelDrag>(
                move |s, p: &PanelDrag, _, _| {
                    if lay.dock_of(p.0) != d { s.bg(t.bg.hover) } else { s }
                },
            )
            .on_drop(cx.listener(move |w, p: &PanelDrag, _, cx| w.move_panel(p.0, d, cx)))
        };
        let Some(shown) = shown.filter(|_| self.docks[d.ix()].open) else {
            let strip = shown.is_some();
            if !strip && !self.panel_drag {
                return None;
            }
            let el = div()
                .id(if d == Dock::Left { "dock-strip-left" } else { "dock-strip-right" })
                .flex_none()
                .w(z(if self.panel_drag { DROP_STRIP } else { COLLAPSED }))
                .h_full()
                .bg(self.panel_bg())
                .border_color(t.border.subtle)
                .map(|e| if d == Dock::Left { e.border_r_1() } else { e.border_l_1() })
                .when(strip, |e| {
                    e.cursor_pointer()
                        .hover(|s| s.bg(t.bg.hover))
                        .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| w.set_dock_open(d, true, cx)))
                });
            return Some(target(el, cx).into_any_element());
        };
        let body = match shown {
            Panel::Sessions => self.sessions_list(cx).into_any_element(),
            Panel::Changes => self.git_panel.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            Panel::Files => self.files_panel.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            Panel::Search => {
                self.search_panel.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            }
        };
        let content = div()
            .id(if d == Dock::Left { "dock-left" } else { "dock-right" })
            .w(z(self.dock_width(d)))
            .h_full()
            .flex()
            .flex_col()
            .bg(if lay.sessions == d { self.panel_bg() } else { t.bg.panel })
            .child(self.panel_tabs(d, shown, cx))
            .child(div().flex_1().min_h_0().child(body));
        let content = target(content, cx);
        let divider = divider(if d == Dock::Left { "divider" } else { "git-divider" }, DockDrag(d));
        let row = div().flex().flex_none().h_full();
        Some(
            match d {
                Dock::Left => row.child(content).child(divider),
                Dock::Right => row.child(divider).child(content),
            }
            .into_any_element(),
        )
    }

    fn session_row(&self, i: usize, now: i64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let tab = &self.tabs[i];
        let session = tab.view.as_ref().map(|v| v.read(cx).session.clone());
        let (unread, exited) = session.as_ref().map_or((false, false), |s| (s.is_unread(), s.is_exited()));
        // While Settings is open no session is shown, so none is marked active; a click closes Settings.
        let active = i == self.active && !self.settings_open();
        let agent = if exited { agent::Agent::default() } else { self.agent(i) };
        let needs_you = agent.state == agent::State::NeedsYou;
        if let Some(Modal::Rename { tab: r, edit }) = &self.modal
            && *r == i
        {
            return div()
                .id(("tab", tab.id))
                .px(z(space::MD))
                .py(z(space::XS))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // inside: keep editing
                .child(text_field(edit))
                .into_any_element();
        }
        let bold = agent.unseen || (unread && !active);
        let state_icon: gpui::AnyElement = match agent.state {
            _ if exited => div()
                .id(("restart", tab.id))
                .flex_none()
                .rounded(z(radius::SM))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg.hover))
                .child(ui::icon(icon::RESTART, 14., t.state.error))
                .tooltip(tip("Exited: click to restart"))
                .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| {
                    if let Some(id) = w.tabs.get(i).and_then(|t| t.view.as_ref()).map(|v| v.entity_id()) {
                        w.restart(id, window, cx);
                    }
                    cx.stop_propagation();
                }))
                .into_any_element(),
            agent::State::Working => {
                // All spinners share one 10 fps tick (`spin_tick`), whatever their number; the angle comes from a
                // common clock, so they turn in step.
                self.spinner_drawn.set(true);
                let phase = (SPIN_EPOCH.elapsed().as_millis() % 1000) as f32 / 1000.;
                ui::icon(icon::SPINNER, 14., t.state.working)
                    .with_transformation(Transformation::rotate(percentage(phase)))
                    .into_any_element()
            }
            agent::State::NeedsYou => div()
                .flex_none()
                .size(z(14.))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(z(8.)).rounded_full().bg(t.state.needs_you))
                .into_any_element(),
            agent::State::Done => {
                ui::icon(icon::DONE, 14., if agent.unseen { t.state.done } else { t.state.done_seen })
                    .into_any_element()
            }
            agent::State::Ready => div()
                .flex_none()
                .size(z(14.))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(z(7.)).rounded_full().border_1().border_color(t.text.dim))
                .into_any_element(),
            agent::State::NoAgent if tab.claude_session.is_some() => {
                ui::icon(icon::RESUMABLE, 13., t.text.dim).into_any_element()
            }
            agent::State::NoAgent if unread && !active => div()
                .flex_none()
                .size(z(14.))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(z(6.)).rounded_full().bg(t.accent))
                .into_any_element(),
            agent::State::NoAgent => div().flex_none().size(z(14.)).into_any_element(),
        };
        let ago = agent.last_activity.map(|a| ui::fmt_ago(now - a));
        let (repo_totals, branch) = match &tab.repo {
            TabRepo::Repo(root) => {
                let st = self.repos.status(root);
                (st.as_ref().map(|s| s.totals()).filter(|t| t.0 > 0), st.map(|s| s.branch.clone()))
            }
            _ => (None, None),
        };
        // A tab whose agent ran shows that agent's files; the repo's totals only where no agent ran.
        let changes = self.tracker.view(tab.id as u32);
        let agent_ran = tab.claude_session.is_some() || !changes.touched.is_empty();
        let status = if agent_ran {
            changes.summary.map(|(n, a, d)| (Some(n), a, d))
        } else {
            repo_totals.map(|(_, a, d)| (None, a, d))
        };
        let second: gpui::AnyElement = if tab.dir_missing {
            div()
                .flex()
                .items_center()
                .gap(z(space::XS))
                .text_color(t.state.needs_you)
                .child(ui::icon(icon::WARNING, 11., t.state.needs_you))
                .child(div().min_w_0().truncate().child(format!("missing · {}", tilde(&tab.cwd))))
                .into_any_element()
        } else {
            let label = branch.unwrap_or_else(|| short_path(&tab.cwd));
            div()
                .flex()
                .items_center()
                .gap(z(space::XS))
                // A linked worktree's branch: accent icon (its own checkout; it groups under the main project).
                .when(matches!(tab.repo, TabRepo::Repo(_)), |d| {
                    d.child(ui::icon(icon::BRANCH, 11., if tab.main_root.is_some() { t.accent } else { t.text.dim }))
                })
                .child(div().min_w_0().truncate().child(label))
                .children(status.map(|(files, add, del)| {
                    let clamp = |v: u64| v.min(u32::MAX as u64) as u32;
                    div()
                        .ml_auto()
                        .flex_none()
                        .flex()
                        .gap(z(space::XS))
                        .children(
                            files
                                .map(|n| div().child(if n == 1 { "1 file".to_string() } else { format!("{n} files") })),
                        )
                        .child(line_counts(Some((clamp(add), clamp(del)))))
                }))
                .into_any_element()
        };
        let bar = if needs_you {
            Some(t.state.needs_you)
        } else if active {
            Some(t.accent)
        } else {
            None
        };
        let row = SharedString::from(format!("row-{}", tab.id));
        // Hover: an x left of the state icon (over the "ago" text), on the row's own background. Paint-time only.
        let close = div()
            .id(("close", tab.id))
            .absolute()
            .right(z(18.))
            .top(z(-3.))
            .size(z(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(z(radius::SM))
            .bg(if active { t.bg.selected } else { t.bg.hover })
            .cursor_pointer()
            .opacity(0.)
            .group_hover(row.clone(), |s| s.opacity(1.))
            .child(ui::icon(icon::CLOSE, 13., t.text.muted))
            .tooltip(tip("Close session (or middle click)"))
            .on_click(cx.listener(move |w, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                w.close_session(i, false, window, cx);
            }));
        let group = self.tab_group(i);
        let drag = SessionDrag { id: tab.id, group: group.clone(), pos: i, name: tab.name.clone().into() };
        let (tab_id, accent, anchor) = (tab.id, t.accent, self.active_row.clone());
        div()
            .id(("tab", tab.id))
            .group(row)
            .relative()
            .h(z(height::SESSION_ROW))
            .mx(z(space::SM))
            .px(z(space::MD))
            .flex()
            .flex_col()
            .justify_center()
            .gap(z(space::XXS))
            .rounded(z(radius::MD))
            .cursor_pointer()
            .when(active, |d| d.bg(t.bg.selected))
            .when(!active, |d| d.hover(|s| s.bg(t.bg.hover)))
            .children(bar.map(|c| div().absolute().left_0().top(z(10.)).bottom(z(10.)).w(z(2.)).rounded_full().bg(c)))
            // Shift+F10 opens the active row's menu just below it: its bottom-left, as painted.
            .when(active, |d| {
                d.child(
                    canvas(move |b, _, _| anchor.set(Some(b.bottom_left())), |_, _, _, _| {}).absolute().size_full(),
                )
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |w, e: &MouseDownEvent, window, cx| {
                    w.open_session_menu(i, Some(e.position), window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |w, _, window, cx| w.close_session(i, false, window, cx)),
            )
            // Reorder within the group: a line where it will land (above when moving up, below when moving down).
            .on_drag(drag, |d: &SessionDrag, _, _, cx| cx.new(|_| RowGhost(d.name.clone())))
            .drag_over::<SessionDrag>(move |s, d, _, _| match () {
                _ if d.group != group || d.id == tab_id => s,
                _ if d.pos > i => s.border_t_2().border_color(accent),
                _ => s.border_b_2().border_color(accent),
            })
            .on_drop(cx.listener(move |w, d: &SessionDrag, _, cx| w.drop_session(d, i, cx)))
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
                    .items_center()
                    .gap(z(space::SM))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(z(text_size::MD))
                            .font_weight(if bold { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(if active || bold { t.text.primary } else { t.text.muted })
                            .child(tab.name.clone()),
                    )
                    .children(
                        ago.map(|a| div().flex_none().text_size(z(text_size::XS)).text_color(t.text.dim).child(a)),
                    )
                    .child(div().relative().flex_none().child(state_icon).child(close)),
            )
            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(second))
            .into_any_element()
    }

    /// Above the terminal: session name, short path, and the agent state with how long it's been in it.
    fn session_header(&self) -> impl IntoElement + use<> {
        let t = theme::get();
        let Some(tab) = self.tabs.get(self.active) else {
            return div().h(z(height::HEADER)).flex_none().bg(t.bg.elevated);
        };
        let agent = self.agent(self.active);
        let since = agent.state_since.map(|s| fmt_for(now_secs() - s));
        let state = match agent.state {
            agent::State::Working => Some(("Working", t.state.working)),
            agent::State::NeedsYou => Some(("Needs you", t.state.needs_you)),
            agent::State::Done => Some(("Done", t.state.done)),
            agent::State::Ready => Some(("Claude ready", t.text.muted)),
            agent::State::NoAgent => None,
        };
        ui::overlay_header()
            .child(ui::icon(icon::TERMINAL, 13., t.text.dim))
            .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(tab.name.clone()))
            .child(div().min_w_0().truncate().text_color(t.text.dim).child(short_path(&tab.cwd)))
            .children(state.map(|(label, color)| {
                div()
                    .ml_auto()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .text_size(z(text_size::XS))
                    .child(div().size(z(6.)).rounded_full().bg(color))
                    .child(div().text_color(t.text.muted).child(match &since {
                        Some(s) => format!("{label} · {s}"),
                        None => label.to_string(),
                    }))
            }))
    }

    /// A dock's tabs: one icon per panel living there (always shown, even alone: drag it or right click it to move
    /// the panel to the other dock).
    fn panel_tabs(&self, d: Dock, shown: Panel, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        let lay = self.settings.layout;
        let changes = match self.tabs.get(self.active).map(|t| &t.repo) {
            Some(TabRepo::Repo(root)) if lay.changes == d => self.repos.status(root).map_or(0, |s| s.totals().0),
            _ => 0,
        };
        let ws = cx.entity().downgrade();
        let tabs: Vec<_> = lay
            .panels(d)
            .map(|p| {
                let active = p == shown;
                let (glyph, hint) = match p {
                    Panel::Sessions => (icon::SESSIONS, "Sessions  Ctrl+Shift+B"),
                    Panel::Files => (icon::FILES, "Files  Ctrl+Shift+E"),
                    Panel::Changes => (icon::CHANGES, "Changes  Ctrl+Shift+G"),
                    Panel::Search => (icon::SEARCH, "Search in files  Ctrl+Shift+F"),
                };
                let ws = ws.clone();
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .border_b_2()
                    .border_color(if active { t.accent } else { gpui::transparent_black().into() })
                    .child(
                        square_button(SharedString::from(hint), glyph, active)
                            .tooltip(tip(hint))
                            .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| w.set_panel_tab(p, cx)))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |w, e: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    w.open_menu(PathBuf::new(), projects_ui::MenuKind::Panel(p), e.position, cx);
                                }),
                            )
                            .on_drag(PanelDrag(p), move |drag, _, _, cx| {
                                // Not inside a workspace update here: flag the drag so empty / collapsed docks
                                // offer a drop strip.
                                let _ = ws.update(cx, |w, cx| {
                                    w.panel_drag = true;
                                    cx.notify();
                                });
                                let label = drag.0.label();
                                cx.new(|_| RowGhost(label.into()))
                            }),
                    )
            })
            .collect();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(z(space::XS))
            .h(z(height::TAB_BAR))
            .px(z(space::MD))
            .border_b_1()
            .border_color(t.border.subtle)
            .children(tabs)
            .when(changes > 0, |d| {
                d.child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(changes.to_string()))
            })
            .child(div().flex_1())
            .child(
                square_button(
                    if d == Dock::Left { "collapse-left" } else { "collapse-right" },
                    if d == Dock::Left { icon::SIDEBAR_CLOSE } else { icon::DOCK_RIGHT_CLOSE },
                    false,
                )
                .tooltip(tip(if lay.sessions == d { "Hide dock  Ctrl+Shift+B" } else { "Hide dock" }))
                .on_click(cx.listener(move |w, _: &ClickEvent, _, cx| w.set_dock_open(d, false, cx))),
            )
    }

    /// Bottom bar: Claude usage limits, session counts, the active branch.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::get();
        let now = now_secs();
        let (five, seven) = self.agents.as_ref().map_or((None, None), |h| {
            let b = h.board.lock().unwrap();
            (b.five_hour, b.seven_day)
        });
        let limit = |label: &'static str, l: Option<hook::Limit>| {
            l.map(|l| {
                let (pct, detail) = if l.resets_at > now {
                    (l.used_pct, format!("{:.0}% · {}", l.used_pct, fmt_remaining(l.resets_at - now)))
                } else {
                    (0., "reset".to_string())
                };
                div()
                    .flex()
                    .items_center()
                    .gap(z(space::SM))
                    .child(div().text_color(t.text.dim).child(label))
                    .child(ui::meter(pct, 56.))
                    .child(div().text_color(t.text.muted).child(detail))
            })
        };
        let needs = self.needs_you_count();
        let branch = match self.tabs.get(self.active).map(|t| &t.repo) {
            Some(TabRepo::Repo(root)) => self.repos.status(root).map(|s| (s.branch.clone(), s.ab)),
            _ => None,
        };
        let n = self.tabs.len();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(z(space::XL))
            .h(z(height::STATUS_BAR))
            .px(z(space::LG))
            .bg(t.bg.panel)
            .border_t_1()
            .border_color(t.border.subtle)
            .text_size(z(text_size::XS))
            .children(limit("5h", five))
            .children(limit("Week", seven))
            .child(div().flex_1())
            .children(self.flash.as_ref().map(|(text, _)| div().text_color(t.text.primary).child(text.clone())))
            .when(!self.settings_notes.is_empty(), |d| {
                let n = self.settings_notes.len();
                d.child(
                    div()
                        .id("settings-notes")
                        .flex()
                        .items_center()
                        .gap(z(space::XS))
                        .cursor_pointer()
                        .text_color(t.state.needs_you)
                        .child(ui::icon(icon::WARNING, 11., t.state.needs_you))
                        .child(format!("Settings: {n} problem{}", if n == 1 { "" } else { "s" }))
                        .on_click(cx.listener(|w, _: &ClickEvent, window, cx| {
                            if !w.settings_open() {
                                w.open_settings(window, cx);
                            }
                        })),
                )
            })
            .children(branch.map(|(b, ab)| {
                div()
                    .id("status-branch")
                    .flex()
                    .items_center()
                    .gap(z(space::XS))
                    .text_color(t.text.muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.text.primary))
                    .child(ui::icon(icon::BRANCH, 11., t.text.dim))
                    .child(b)
                    .children(ab.map(ui::ahead_behind).filter(|s| !s.is_empty()))
                    .tooltip(tip("Switch or create a branch (Ctrl+Shift+O)"))
                    .on_click(cx.listener(|w, _: &ClickEvent, window, cx| w.open_branches(window, cx)))
            }))
            .child(div().text_color(t.text.dim).child(format!("{n} session{}", if n == 1 { "" } else { "s" })))
            .when(needs > 0, |d| {
                d.child(
                    div()
                        .text_color(t.state.needs_you)
                        .child(format!("{needs} need{} you", if needs == 1 { "s" } else { "" })),
                )
            })
    }

    fn modal_view(&self, window: &Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let card = || {
            div()
                .w(z(520.))
                .h_auto()
                .p(z(space::XL))
                .flex()
                .flex_col()
                .gap(z(space::MD))
                .rounded(z(radius::LG))
                .bg(t.bg.elevated)
                .border_1()
                .border_color(t.border.strong)
        };
        // `occlude`: clicks never reach (and focus) what is under the modal.
        let scrim =
            || div().absolute().size_full().occlude().flex().justify_center().items_start().pt(z(120.)).bg(t.bg.scrim);
        if let Some(Modal::Confirm { title, body, files, ok, danger, .. }) = &self.modal {
            let (ok, danger) = (*ok, *danger);
            // Commit-confirm list: shared / via-command files stand out (they may hold other work).
            let list = (!files.is_empty()).then(|| {
                div()
                    .id("confirm-files")
                    .max_h(z(240.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(z(space::XXS))
                    .p(z(space::SM))
                    .rounded(z(radius::MD))
                    .bg(t.bg.input)
                    .font_family(MONO_FONT)
                    .text_size(z(text_size::XS))
                    .children(files.iter().map(|(path, m)| {
                        let flagged = m.shared || m.via_command;
                        let tag = |icon_path: &'static str, label: &'static str| {
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(z(space::XXS))
                                .child(ui::icon(icon_path, 11., t.state.needs_you))
                                .child(label)
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap(z(space::SM))
                            .text_color(if flagged { t.state.needs_you } else { t.text.primary })
                            .child(div().min_w_0().flex_1().truncate().child(path.clone()))
                            .when(m.shared, |d| d.child(tag(icon::SHARED, "shared")))
                            .when(m.via_command, |d| d.child(tag(icon::VIA_COMMAND, "via command")))
                    }))
            });
            let flagged = files.iter().any(|(_, m)| m.shared || m.via_command);
            let button = |id: &'static str, label: &'static str, danger: bool| {
                div()
                    .id(id)
                    .px(z(space::MD))
                    .py(z(space::XS))
                    .rounded(z(radius::MD))
                    .text_size(z(text_size::SM))
                    .cursor_pointer()
                    .when(danger, |d| d.bg(t.state.error).text_color(t.text.on_accent))
                    .when(!danger && label != "Cancel", |d| d.bg(t.accent).text_color(t.text.on_accent))
                    .when(label == "Cancel", |d| d.border_1().border_color(t.border.strong).text_color(t.text.primary))
                    .hover(|s| s.opacity(0.85))
                    .child(label)
            };
            return Some(
                scrim()
                    .child(
                        card()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(z(space::SM))
                                    .child(if danger { ui::icon(icon::WARNING, 15., t.state.error) } else { ui::icon(icon::BRANCH, 15., t.accent) })
                                    .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(title.clone())),
                            )
                            .child(div().text_size(z(text_size::SM)).text_color(t.text.muted).child(body.clone()))
                            .children(list)
                            .when(flagged, |d| {
                                d.child(div().text_size(z(text_size::XS)).text_color(t.state.needs_you).child(
                                    "Highlighted: shared with another session or changed by a shell command (best effort): check they hold only this agent's work.",
                                ))
                            })
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(z(space::SM))
                                    .child(button("confirm-cancel", "Cancel", false).on_click(cx.listener(|w, _: &ClickEvent, window, cx| {
                                        w.close_modal(window, cx)
                                    })))
                                    .child(button("confirm-ok", ok, danger).on_click(cx.listener(|w, _: &ClickEvent, window, cx| {
                                        w.confirm_ok(window, cx)
                                    }))),
                            )
                            .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child(format!("Enter to {} · Esc to cancel", ok.to_lowercase()))),
                    )
                    .into_any_element(),
            );
        }
        match &self.modal {
            Some(Modal::Picker(p)) => Some(
                div()
                    .absolute()
                    .size_full()
                    .occlude()
                    .flex()
                    .justify_center()
                    .items_center()
                    .bg(t.bg.scrim)
                    .child(self.picker_view(p, window, cx))
                    .into_any_element(),
            ),
            Some(Modal::ProjectRename { root, edit }) => Some(
                scrim()
                    .child(
                        card()
                            .child(
                                div().font_weight(FontWeight::SEMIBOLD).child(format!("Rename {}", short_path(root))),
                            )
                            .child(
                                div()
                                    .text_size(z(text_size::SM))
                                    .text_color(t.text.muted)
                                    .child("Shown in the sidebar and the picker. Empty: the folder name."),
                            )
                            .child(text_field(edit))
                            .child(
                                div()
                                    .text_size(z(text_size::XS))
                                    .text_color(t.text.dim)
                                    .child("Enter to save · Esc to cancel"),
                            ),
                    )
                    .into_any_element(),
            ),
            Some(Modal::ProjectDefaults(d)) => {
                let words = hook::split_words(&d.flags.text);
                let kept = hook::resume_args(&words);
                let dropped = dropped_words(&words, &kept);
                let auto = d.auto;
                let label = |text: &'static str| div().text_size(z(text_size::SM)).text_color(t.text.muted).child(text);
                Some(
                    scrim()
                        .child(
                            card()
                                .w(z(600.))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Defaults for {}", short_path(&d.root))))
                                .child(
                                    div()
                                        .id("defaults-auto")
                                        .flex()
                                        .items_center()
                                        .gap(z(space::SM))
                                        .p(z(2.))
                                        .cursor_pointer()
                                        .text_size(z(text_size::SM))
                                        .text_color(if d.focus == 0 { t.text.primary } else { t.text.muted })
                                        .child(div().size(z(12.)).rounded(z(3.)).border_1().border_color(if auto { t.accent } else { t.border.strong }).when(auto, |d| d.bg(t.accent)))
                                        .child("Start claude in new sessions")
                                        .on_click(cx.listener(|w, _: &ClickEvent, _, cx| {
                                            if let Some(Modal::ProjectDefaults(d)) = &mut w.modal {
                                                d.auto = !d.auto;
                                                d.focus = 0;
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(label("Claude flags (e.g. --model opus --effort high). Only flags safe to replay are kept; permission bypasses never."))
                                .child(setup::text_input(&d.flags, d.focus == 1, "--model opus"))
                                .when(!dropped.is_empty(), |e| {
                                    e.child(div().text_size(z(text_size::XS)).text_color(t.state.needs_you).child(format!("Will be dropped (not allowed): {}", dropped.join(" "))))
                                })
                                .child(label("Setup command for new worktrees (runs in the session before claude)"))
                                .child(setup::text_input(&d.setup, d.focus == 2, "e.g. pnpm install"))
                                .child(label("Where new worktrees go (empty: <repo>-worktrees next to the repo)"))
                                .child(setup::text_input(&d.wt_dir, d.focus == 3, "~/worktrees/project"))
                                .child(div().text_size(z(text_size::XS)).text_color(t.text.dim).child("Tab next field · Space toggles · Enter save · Esc cancel")),
                        )
                        .into_any_element(),
                )
            }
            Some(Modal::Setup(s)) => Some(scrim().child(self.setup_view(s, cx)).into_any_element()),
            Some(Modal::CloseWorktree(c)) => Some(scrim().child(self.close_wt_view(c, cx)).into_any_element()),
            Some(Modal::Worktrees(l)) => Some(scrim().child(self.worktrees_view(l, cx)).into_any_element()),
            Some(Modal::Branches(b)) => Some(scrim().child(self.branches_view(b, window, cx)).into_any_element()),
            _ => None,
        }
    }
}

/// The typed words `resume_args` didn't keep (kept is a subsequence of words).
fn dropped_words(words: &[String], kept: &[String]) -> Vec<String> {
    let mut k = kept.iter().peekable();
    words
        .iter()
        .filter(|w| {
            if k.peek() == Some(w) {
                k.next();
                false
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

fn text_field(edit: &LineEdit) -> impl IntoElement {
    let t = theme::get();
    div()
        .flex()
        .items_center()
        .px(z(space::MD))
        .py(z(space::SM))
        .rounded(z(radius::MD))
        .bg(t.bg.input)
        .border_1()
        .border_color(t.accent)
        .text_size(z(text_size::MD))
        .child(edit.before_cursor().to_string())
        .child(div().w(z(1.5)).h(z(16.)).bg(t.accent))
        .child(edit.after_cursor().to_string())
}

/// Collapsed-state key of a group (`""` for "Other").
pub(super) fn group_id(key: &Option<PathBuf>) -> PathBuf {
    key.clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths() {
        assert_eq!(short_path(Path::new("/a/b")), "/a/b");
        let home = home();
        assert_eq!(short_path(&home.join("Code/x")), "~/Code/x");
        assert_eq!(short_path(&home.join("Code/proj/crates/app/src")), "~/Code/…/app/src");
        assert_eq!(fmt_for(5), "<1m");
        assert_eq!(fmt_for(125), "2m");
    }
}
